use std::collections::HashMap;

use anyhow::Result;
use rayon::prelude::*;

use crate::graph::store::GraphStore;
use crate::graph::store_util::copy_edges_with_bad_record_retry;
use crate::learned::LearnedStore;
use crate::model::{FileExtraction, RelationKind};

use super::inherits::resolve_inherits;
use super::{escape, shortest_id, ResolveStats};

/// Post-indexing pass that resolves call edges using cross-file symbol lookup.
/// Builds symbol map from the full graph (not just re-indexed files) so
/// incremental indexing doesn't lose cross-file resolution.
/// Caller must hold WriteLock (creates CALLS and INHERITS edges).
pub fn resolve_calls_incremental(
    store: &GraphStore,
    extractions: &[FileExtraction],
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    if extractions.is_empty() {
        return Ok(ResolveStats {
            total_calls: 0,
            resolved: 0,
            unresolved: 0,
            learned_resolved: 0,
            inherits_resolved: 0,
        });
    }

    let conn = store.connection()?;

    // Build global symbol table from full graph: name -> [(id, file, kind)]
    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    for (name, id, file, kind) in store.get_all_symbols()? {
        symbol_map.entry(name).or_default().push((id, file, kind));
    }

    let mut stats = resolve_with_map(&conn, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, extractions, &symbol_map)?;
    resolve_custom_edges(&conn, extractions, &symbol_map)?;
    Ok(stats)
}

/// Post-indexing pass that resolves call edges using cross-file symbol lookup.
///
/// Problem: During extraction, `authenticate()` called in `main.py` creates
/// a CALLS relation targeting `main.py::authenticate`. But the real symbol
/// is `auth.py::authenticate`. This pass:
///
/// Caller must hold WriteLock.
/// 1. Builds a symbol table from all extractions
/// 2. For each CALLS relation where the target doesn't exist locally,
///    searches the global symbol table by name
/// 3. Creates the resolved CALLS edge in the graph
pub fn resolve_calls(
    store: &GraphStore,
    extractions: &[FileExtraction],
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    let conn = store.connection()?;

    // Build global symbol table: name -> list of (id, file, kind)
    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    for ext in extractions {
        for sym in &ext.symbols {
            symbol_map.entry(sym.name.clone()).or_default().push((
                sym.id.clone(),
                ext.file.clone(),
                sym.kind.as_str().to_string(),
            ));
        }
    }

    let mut stats = resolve_with_map(&conn, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, extractions, &symbol_map)?;
    resolve_custom_edges(&conn, extractions, &symbol_map)?;
    Ok(stats)
}

/// Cross-file resolution for `RelationKind::Custom` edges (AIF3X-331 #16:
/// INJECTS_DEPENDENCY, REGISTERS_MIDDLEWARE), mirroring what `resolve_with_map`
/// does for CALLS. Extraction always scopes a custom edge's target_id to its
/// own file (`{file}::{name}`) since it has no cross-file symbol table to
/// consult at parse time — same as a raw dangling CALLS target. Without this
/// pass, a registration referencing an imported symbol (e.g. `Depends(fn)`
/// where `fn` lives in another file) points at a target_id that never exists
/// in the graph, so the edge silently vanishes at write time (upsert_all_bulk's
/// `MATCH (a),(b) WHERE ... CREATE` finds nothing to attach to).
///
/// Caller must hold WriteLock. Each edge kind is resolved and written to its
/// own rel table (never CALLS) to keep call-graph semantics unchanged.
fn resolve_custom_edges(
    conn: &kuzu::Connection<'_>,
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
) -> Result<()> {
    let known_ids: std::collections::HashSet<&str> = symbol_map
        .values()
        .flat_map(|v| v.iter().map(|(id, _, _)| id.as_str()))
        .collect();

    let mut by_edge_kind: HashMap<&str, Vec<(String, String)>> = HashMap::new();

    for ext in extractions {
        for rel in &ext.relations {
            let RelationKind::Custom(edge_name) = &rel.kind else {
                continue;
            };

            let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);
            if known_ids.contains(rel.target_id.as_str()) {
                // Already resolves (e.g. local-file target) — write as-is.
                by_edge_kind
                    .entry(edge_name.as_str())
                    .or_default()
                    .push((rel.source_id.clone(), rel.target_id.clone()));
                continue;
            }

            // Cross-file: look up by bare name in the global symbol table,
            // same single-candidate-only policy resolve_with_map uses before
            // falling back to import-scope disambiguation — collision
            // handling across multiple same-named candidates is intentionally
            // out of scope for this pass (see AIF3X-331 #16 design doc).
            if let Some(candidates) = symbol_map.get(target_name) {
                if candidates.len() == 1 {
                    by_edge_kind
                        .entry(edge_name.as_str())
                        .or_default()
                        .push((rel.source_id.clone(), candidates[0].0.clone()));
                }
            }
        }
    }

    for (edge_name, pairs) in &by_edge_kind {
        if pairs.is_empty() {
            continue;
        }
        let mut seen: std::collections::HashSet<&(String, String)> =
            std::collections::HashSet::new();
        let valid: Vec<&(String, String)> = pairs
            .iter()
            .filter(|(src, tgt)| {
                known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str())
            })
            .filter(|pair| seen.insert(pair))
            .collect();
        if valid.is_empty() {
            continue;
        }
        crate::graph::schema::ensure_custom_edge_table(conn, edge_name)?;
        const CHUNK_SIZE: usize = 500;
        for chunk in valid.chunks(CHUNK_SIZE) {
            let pair_list: Vec<String> = chunk
                .iter()
                .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
                .collect();
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{}]->(b)",
                pair_list.join(", "),
                edge_name
            ));
        }
    }

    Ok(())
}

/// Write ExternalRef nodes + EXTERNAL_CALL edges for calls whose receiver
/// resolved to a real class/type name but that type has no local Symbol
/// (see graph/schema.rs's ExternalRef comment for why this exists). Caller
/// must hold WriteLock.
fn write_external_calls(
    conn: &kuzu::Connection<'_>,
    external_calls: &[(String, String, String)],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
    extractions: &[FileExtraction],
) {
    let mut known_ids: std::collections::HashSet<&str> = symbol_map
        .values()
        .flat_map(|v| v.iter().map(|(id, _, _)| id.as_str()))
        .collect();
    for ext in extractions {
        for sym in &ext.symbols {
            known_ids.insert(&sym.id);
        }
    }

    let mut seen: std::collections::HashSet<&(String, String, String)> =
        std::collections::HashSet::new();
    let valid: Vec<&(String, String, String)> = external_calls
        .iter()
        .filter(|(caller, _, _)| known_ids.contains(caller.as_str()))
        .filter(|triple| seen.insert(triple))
        .collect();
    if valid.is_empty() {
        return;
    }

    const CHUNK_SIZE: usize = 500;
    for chunk in valid.chunks(CHUNK_SIZE) {
        let rows: Vec<String> = chunk
            .iter()
            .map(|(caller, receiver, method)| {
                let ref_id = format!("{}::{}", receiver, method);
                format!(
                    "{{caller: '{}', ref_id: '{}', qualifier: '{}', method: '{}'}}",
                    escape(caller),
                    escape(&ref_id),
                    escape(receiver),
                    escape(method)
                )
            })
            .collect();
        let _ = conn.query(&format!(
            "UNWIND [{}] AS r \
             MERGE (e:ExternalRef {{id: r.ref_id}}) \
             ON CREATE SET e.qualifier = r.qualifier, e.method = r.method \
             WITH r, e \
             MATCH (a:Symbol) WHERE a.id = r.caller \
             CREATE (a)-[:EXTERNAL_CALL]->(e)",
            rows.join(", ")
        ));
    }
}

/// Caller must hold WriteLock.
fn resolve_with_map(
    conn: &kuzu::Connection<'_>,
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    let mut resolved = 0;
    let mut unresolved = 0;
    let mut total_dangling = 0;
    let mut resolved_pairs: Vec<(String, String)> = Vec::new();
    let mut learned_resolved = 0usize;

    // Build class-method index: "ClassName::method" -> symbol_id
    let mut class_method_map: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for candidates in symbol_map.values() {
        for (id, _file, kind) in candidates {
            if kind == "Method" || kind == "Function" {
                let parts: Vec<&str> = id.rsplitn(3, "::").collect();
                if parts.len() >= 2 {
                    let method = parts[0];
                    let class = parts[1];
                    let key = format!("{}::{}", class, method);
                    class_method_map
                        .entry(key)
                        .or_default()
                        .push((id.clone(), _file.clone()));
                }
            }
        }
    }

    // Build a flat HashSet of all known symbol IDs for learned-store lookups
    let all_symbol_ids: std::collections::HashSet<&str> = if learned_store.is_some() {
        symbol_map
            .values()
            .flat_map(|v| v.iter().map(|(id, _, _)| id.as_str()))
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Parallel resolution: each file resolved independently, results merged
    struct FileResolveResult {
        resolved: usize,
        unresolved: usize,
        dangling: usize,
        learned: usize,
        pairs: Vec<(String, String)>,
        // (caller_id, receiver_type, method_name) for calls whose receiver
        // resolved to a real class/type name but that type has no local
        // Symbol — e.g. a statically-linked lib whose source isn't indexed.
        // See ExternalRef/EXTERNAL_CALL in graph/schema.rs for why this
        // exists instead of letting these vanish into `unresolved` with no
        // trace.
        external_calls: Vec<(String, String, String)>,
    }

    let file_results: Vec<FileResolveResult> = extractions
        .par_iter()
        .map(|ext| {
            let mut res = FileResolveResult {
                resolved: 0,
                unresolved: 0,
                dangling: 0,
                learned: 0,
                pairs: Vec::new(),
                external_calls: Vec::new(),
            };

            let local_symbols: HashMap<&str, &str> = ext
                .symbols
                .iter()
                .map(|s| (s.name.as_str(), s.id.as_str()))
                .collect();

            let imported_stems: std::collections::HashSet<String> = ext
                .relations
                .iter()
                .filter(|r| r.kind == RelationKind::Imports)
                .map(|r| {
                    // target_id is "{file}::{module}" — strip the file prefix
                    // before taking the last dotted segment, otherwise a
                    // bare (dot-free) module name like a relative import's
                    // falls back to splitting the file's own ".py" extension
                    // instead (e.g. "a/b.py::risk_service" -> "py::risk_service").
                    let module = r.target_id.rsplit("::").next().unwrap_or(&r.target_id);
                    let raw = module.rsplit(['/', '\\', '.']).next().unwrap_or(module);
                    raw.to_lowercase()
                })
                .collect();

            let source_is_sql = ext.file.ends_with(".sql");

            for rel in &ext.relations {
                if rel.kind != RelationKind::Calls {
                    continue;
                }

                let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);

                if let Some(&target_id) = local_symbols.get(target_name) {
                    // Target resolves locally by bare name, but rel.source_id
                    // is also bare (extraction's find_enclosing_function only
                    // ever returns an unqualified name — see relations.rs) —
                    // e.g. "DebugViewModel.cs::ExecuteCrashManagedBackground"
                    // rather than the real "...::DebugViewModel::ExecuteCrashManagedBackground".
                    // The initial bulk write (store_bulk.rs) uses rel.source_id
                    // verbatim with no such fixup, so its MATCH never finds a
                    // node and the edge silently never gets created — this was
                    // dropping every local same-class call (e.g. a WPF event
                    // handler's body calling another method on the same class).
                    // Resolve the source the same way cross-file pairs already
                    // get fixed up downstream (see `fixed_pairs` below), but do
                    // it here so local calls actually get a pair pushed at all.
                    let source_name = rel.source_id.rsplit("::").next().unwrap_or(&rel.source_id);
                    let source_id = local_symbols
                        .get(source_name)
                        .copied()
                        .unwrap_or(rel.source_id.as_str());
                    res.pairs
                        .push((source_id.to_string(), target_id.to_string()));
                    res.resolved += 1;
                    continue;
                }

                res.dangling += 1;

                // Layer 3: Learned pattern lookup (from prior SCIP corrections).
                if let Some(ls) = learned_store {
                    if let Some(pattern) = ls.lookup(&ext.file, target_name) {
                        if all_symbol_ids.contains(pattern.resolved_to_symbol.as_str()) {
                            res.pairs
                                .push((rel.source_id.clone(), pattern.resolved_to_symbol.clone()));
                            res.resolved += 1;
                            res.learned += 1;
                            continue;
                        }
                    }
                }

                // Strategy 1: Receiver-aware resolution.
                if let Some(ref receiver) = rel.receiver {
                    let qualified = format!("{}::{}", receiver, target_name);
                    if let Some(matches) = class_method_map.get(&qualified) {
                        let best = if matches.len() == 1 {
                            Some(matches[0].0.clone())
                        } else {
                            let by_import = shortest_id2(matches.iter(), |(_, f)| {
                                let stem = std::path::Path::new(f)
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .map(|s| s.to_lowercase())
                                    .unwrap_or_default();
                                imported_stems.contains(&stem)
                            });
                            by_import.or_else(|| {
                                matches
                                    .iter()
                                    .min_by(|(a, _), (b, _)| {
                                        a.len().cmp(&b.len()).then_with(|| a.cmp(b))
                                    })
                                    .map(|(id, _)| id.clone())
                            })
                        };
                        if let Some(target_id) = best {
                            res.pairs.push((rel.source_id.clone(), target_id));
                            res.resolved += 1;
                            continue;
                        }
                    }
                }

                // Strategy 2: Enclosing-class preference.
                let caller_class = rel.source_id.rsplit("::").nth(1).map(|s| s.to_string());

                if let Some(candidates) = symbol_map.get(target_name) {
                    let cross_file: Vec<_> = candidates
                        .iter()
                        .filter(|(_, f, kind)| {
                            if *f == ext.file {
                                return false;
                            }
                            if source_is_sql && f.ends_with(".sql") && kind == "Function" {
                                return false;
                            }
                            true
                        })
                        .collect();

                    let resolved_id = if cross_file.len() == 1 {
                        Some(cross_file[0].0.clone())
                    } else if cross_file.len() > 1 {
                        let by_receiver: Option<String> = rel.receiver.as_ref().and_then(|recv| {
                            let pattern = format!("::{}::{}", recv, target_name);
                            shortest_id(cross_file.iter().copied(), |(id, _, _)| {
                                id.contains(&pattern)
                            })
                        });

                        if by_receiver.is_some() {
                            by_receiver
                        } else if let Some(ref cls) = caller_class {
                            let cls_pattern = format!("::{cls}::");
                            let same_class =
                                shortest_id(cross_file.iter().copied(), |(id, _, _)| {
                                    id.contains(&cls_pattern)
                                });
                            if same_class.is_some() {
                                same_class
                            } else {
                                import_scope_match(&cross_file, &imported_stems, source_is_sql)
                            }
                        } else {
                            import_scope_match(&cross_file, &imported_stems, source_is_sql)
                        }
                    } else {
                        None
                    };

                    if let Some(target_id) = resolved_id {
                        res.pairs.push((rel.source_id.clone(), target_id));
                        res.resolved += 1;
                    } else if let Some(ref receiver) = rel.receiver {
                        res.external_calls.push((
                            rel.source_id.clone(),
                            receiver.clone(),
                            target_name.to_string(),
                        ));
                        res.unresolved += 1;
                    } else {
                        res.unresolved += 1;
                    }
                } else if let Some(ref receiver) = rel.receiver {
                    res.external_calls.push((
                        rel.source_id.clone(),
                        receiver.clone(),
                        target_name.to_string(),
                    ));
                    res.unresolved += 1;
                } else {
                    res.unresolved += 1;
                }
            }

            res
        })
        .collect();

    // Merge parallel results
    for fr in &file_results {
        resolved += fr.resolved;
        unresolved += fr.unresolved;
        total_dangling += fr.dangling;
        learned_resolved += fr.learned;
    }
    let total_pairs: usize = file_results.iter().map(|fr| fr.pairs.len()).sum();
    resolved_pairs.reserve(total_pairs);
    let mut external_calls: Vec<(String, String, String)> = Vec::new();
    for fr in file_results {
        resolved_pairs.extend(fr.pairs);
        external_calls.extend(fr.external_calls);
    }

    // Batch insert resolved CALLS edges via COPY FROM parquet
    if !resolved_pairs.is_empty() {
        let mut known_ids: std::collections::HashSet<&str> = symbol_map
            .values()
            .flat_map(|v| v.iter().map(|(id, _, _)| id.as_str()))
            .collect();
        for ext in extractions {
            for sym in &ext.symbols {
                known_ids.insert(&sym.id);
            }
        }
        let mut file_name_to_ids: HashMap<(String, String), Vec<String>> = HashMap::new();
        for ext in extractions {
            for sym in &ext.symbols {
                file_name_to_ids
                    .entry((ext.file.clone(), sym.name.clone()))
                    .or_default()
                    .push(sym.id.clone());
            }
        }
        for candidates in symbol_map.values() {
            for (id, file, _kind) in candidates {
                let name = id.rsplit("::").next().unwrap_or(id);
                file_name_to_ids
                    .entry((file.clone(), name.to_string()))
                    .or_default()
                    .push(id.clone());
            }
        }

        let fixed_pairs: Vec<(String, String)> = resolved_pairs
            .iter()
            .flat_map(|(src, tgt)| {
                if known_ids.contains(src.as_str()) {
                    vec![(src.clone(), tgt.clone())]
                } else if let Some(sep) = src.rfind("::") {
                    let file_part = &src[..sep];
                    let name_part = &src[sep + 2..];
                    if let Some(ids) =
                        file_name_to_ids.get(&(file_part.to_string(), name_part.to_string()))
                    {
                        ids.iter()
                            .filter(|id| known_ids.contains(id.as_str()))
                            .map(|id| (id.clone(), tgt.clone()))
                            .collect::<Vec<_>>()
                    } else {
                        vec![(src.clone(), tgt.clone())]
                    }
                } else {
                    vec![(src.clone(), tgt.clone())]
                }
            })
            .collect();

        // file_name_to_ids can carry the same id twice for one (file, name) key
        // (populated once from extractions and once from symbol_map), which
        // would otherwise fan a single call site out into duplicate CALLS edges.
        let mut seen_pairs: std::collections::HashSet<&(String, String)> =
            std::collections::HashSet::new();
        let valid_pairs: Vec<&(String, String)> = fixed_pairs
            .iter()
            .filter(|(src, tgt)| {
                known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str())
            })
            .filter(|pair| seen_pairs.insert(pair))
            .collect();

        let pairs: Vec<(String, String)> = valid_pairs
            .into_iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        let pq_path = std::env::temp_dir().join("infigraph_resolve_calls.parquet");
        copy_edges_with_bad_record_retry(conn, "CALLS", pairs, "Symbol", "Symbol", &pq_path);
    }

    if !external_calls.is_empty() {
        write_external_calls(conn, &external_calls, symbol_map, extractions);
    }

    Ok(ResolveStats {
        total_calls: total_dangling,
        resolved,
        unresolved,
        learned_resolved,
        inherits_resolved: 0,
    })
}

/// Targeted re-resolution for a subset of files.
pub fn re_resolve_for_files(
    store: &GraphStore,
    files: &[String],
    extractions: &[FileExtraction],
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    if files.is_empty() || extractions.is_empty() {
        return Ok(ResolveStats {
            total_calls: 0,
            resolved: 0,
            unresolved: 0,
            learned_resolved: 0,
            inherits_resolved: 0,
        });
    }

    let _lock = store.write_lock()?;
    let conn = store.connection()?;

    for file in files {
        let escaped = escape(file);
        let _ = conn.query(&format!(
            "MATCH (a:Symbol)-[r:CALLS]->(b:Symbol) WHERE a.file = '{}' DELETE r",
            escaped
        ));
        let _ = conn.query(&format!(
            "MATCH (a:Symbol)-[r:INHERITS]->(b:Symbol) WHERE a.file = '{}' DELETE r",
            escaped
        ));
    }

    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    for (name, id, file, kind) in store.get_all_symbols()? {
        symbol_map.entry(name).or_default().push((id, file, kind));
    }

    let target_files: std::collections::HashSet<&str> = files.iter().map(|f| f.as_str()).collect();
    let filtered: Vec<&FileExtraction> = extractions
        .iter()
        .filter(|e| target_files.contains(e.file.as_str()))
        .collect();

    let filtered_owned: Vec<FileExtraction> = filtered.into_iter().cloned().collect();
    let mut stats = resolve_with_map(&conn, &filtered_owned, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, &filtered_owned, &symbol_map)?;
    Ok(stats)
}

fn import_scope_match(
    cross_file: &[&(String, String, String)],
    imported_stems: &std::collections::HashSet<String>,
    source_is_sql: bool,
) -> Option<String> {
    let in_scope: Vec<_> = if !imported_stems.is_empty() {
        cross_file
            .iter()
            .filter(|(_, f, _)| {
                let stem = std::path::Path::new(f)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                imported_stems.contains(&stem)
            })
            .collect()
    } else {
        vec![]
    };
    if !in_scope.is_empty() {
        in_scope
            .iter()
            .min_by(|(a, _, _), (b, _, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
            .map(|(id, _, _)| id.clone())
    } else if source_is_sql {
        shortest_id(cross_file.iter().copied(), |(_, _, k)| *k == "Class")
    } else {
        None
    }
}

fn shortest_id2<'a, I, F>(iter: I, pred: F) -> Option<String>
where
    I: Iterator<Item = &'a (String, String)>,
    F: Fn(&(String, String)) -> bool,
{
    iter.filter(|t| pred(t))
        .min_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
        .map(|(id, _)| id.clone())
}
