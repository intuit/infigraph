use std::collections::HashMap;

use anyhow::Result;
use rayon::prelude::*;

use crate::graph::store::GraphStore;
use crate::graph::store_util::{copy_edges_with_bad_record_retry, unique_tmp_dir};
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

    let mut stats = write_resolved_calls(store, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(store, extractions, &symbol_map)?;
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

    let mut stats = write_resolved_calls(store, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(store, extractions, &symbol_map)?;
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

/// Pure decision result of [`resolve_pairs`] — no backend write yet.
pub(crate) struct ResolvedCalls {
    pub(crate) pairs: Vec<(String, String)>,
    pub(crate) external_calls: Vec<(String, String, String)>,
    pub(crate) stats: ResolveStats,
}

/// Backend-agnostic call-resolution decision loop, shared by Kuzu and Neo4j
/// so the two backends can never drift into two hand-synced copies of the
/// same disambiguation logic again (see git history on
/// `Neo4jBackend::resolve_calls` for what that drift costs: a missing
/// same-class fast path, a missing bare-source-id reconciliation, and a
/// cruder tiebreak, each found and ported separately over several passes).
/// Touches no `Connection`/backend handle — callers do their own write
/// (Kuzu: parquet bulk copy; Neo4j: batched Cypher UNWIND/MERGE) using the
/// `pairs`/`external_calls` this returns.
pub(crate) fn resolve_pairs(
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
    learned_store: Option<&LearnedStore>,
) -> ResolvedCalls {
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

            // Keeps every candidate per bare name, same reason as
            // local_callables below: a file can have two distinct symbols
            // sharing a bare name (e.g. a class and a same-named free
            // function/method), and collapsing them to one id keyed by
            // insertion order made both the source-id fixup and the caller's
            // enclosing-class lookup depend on extraction order.
            let mut local_symbols: HashMap<&str, Vec<&str>> = HashMap::new();
            for s in &ext.symbols {
                local_symbols
                    .entry(s.name.as_str())
                    .or_default()
                    .push(s.id.as_str());
            }

            // Callable-only view of local_symbols, keyed the same way, used to
            // gate the same-class fast path below: a call target must resolve
            // to something invocable (Method/Function), never a field/variable
            // that happens to share the name (e.g. a `builder` field beside a
            // `builder()` method). Keeps every candidate per bare name (not
            // just one) — a file can legally have both a free function and a
            // same-named method (e.g. a static helper `DoOneBatchStream()`
            // alongside `SanitizerApp::DoOneBatchStream`), and collapsing them
            // to a single id keyed by insertion order made the fast path's
            // pick depend on extraction order, which isn't stable run-to-run.
            let mut local_callables: HashMap<&str, Vec<&str>> = HashMap::new();
            for s in ext.symbols.iter().filter(|s| {
                matches!(
                    s.kind,
                    crate::model::SymbolKind::Method | crate::model::SymbolKind::Function
                )
            }) {
                local_callables
                    .entry(s.name.as_str())
                    .or_default()
                    .push(s.id.as_str());
            }

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

            // rel.source_id is routinely bare (extraction's
            // find_enclosing_function only ever returns an unqualified name,
            // see the comment further below) — derive the caller's real
            // qualified id from local_symbols so a bare source_id like
            // "widget.py::render" still resolves its own enclosing class
            // ("Widget") instead of looking classless just because the raw
            // source_id had no class segment. If the bare source name is
            // itself ambiguous (two symbols in this file share it), there's
            // no way to know which one made the call — fall back to the raw
            // source_id as-is rather than guessing. Shared by the fast path
            // and Strategy 2 below so both derive the caller's class the
            // same way instead of one reading it off the raw (often-bare)
            // source_id directly.
            let qualified_source_id = |source_id: &str| -> String {
                let source_name = source_id.rsplit("::").next().unwrap_or(source_id);
                match local_symbols.get(source_name) {
                    Some(ids) if ids.len() == 1 => ids[0].to_string(),
                    _ => source_id.to_string(),
                }
            };

            for rel in &ext.relations {
                if rel.kind != RelationKind::Calls {
                    continue;
                }

                let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);

                // Same-class fast path only applies when the call is unqualified
                // (`method()`) or explicitly self-referential (`this.method()`,
                // `self.method()`) — a receiver like `chain` or `exchange` means
                // the name match is coincidental (e.g. an override calling the
                // delegate's same-named method, `chain.filter(x)` inside a
                // `filter()` override) and must fall through to the
                // receiver-aware strategies below instead of self-looping.
                let is_self_receiver = matches!(
                    rel.receiver.as_deref().map(str::trim),
                    None | Some("this") | Some("self")
                );

                if is_self_receiver {
                    if let Some(candidates) = local_callables.get(target_name) {
                        // Multiple candidates share this bare name in the same
                        // file (free function + same-named method, or
                        // overloads). Resolve using real scoping semantics: an
                        // unqualified/self call inside a method is shadowed by
                        // that method's own class first; only if the caller
                        // isn't in a matching class does the free-standing
                        // candidate apply. A candidate is free-standing iff
                        // its id is exactly "{file}::{name}" (no class
                        // segment) — checked structurally, not by counting
                        // "::" occurrences, since file paths can contain them.
                        let target_id = if candidates.len() == 1 {
                            Some(candidates[0])
                        } else {
                            let qualified_source = qualified_source_id(&rel.source_id);
                            let caller_class = qualified_source.rsplit("::").nth(1);

                            let method_match = caller_class.and_then(|cls| {
                                let pattern = format!("::{cls}::{target_name}");
                                let mut it = candidates
                                    .iter()
                                    .copied()
                                    .filter(|id| id.ends_with(&pattern));
                                let first = it.next();
                                if first.is_some() && it.next().is_none() {
                                    first
                                } else {
                                    None
                                }
                            });

                            if method_match.is_some() {
                                method_match
                            } else {
                                let free_standing_id = format!("{}::{target_name}", ext.file);
                                let free_standing: Vec<&str> = candidates
                                    .iter()
                                    .copied()
                                    .filter(|id| *id == free_standing_id)
                                    .collect();
                                if free_standing.len() == 1 {
                                    Some(free_standing[0])
                                } else {
                                    // Ambiguous (multiple overloads, or no
                                    // scoping signal to break the tie) —
                                    // abstain rather than guess.
                                    None
                                }
                            }
                        };

                        let Some(target_id) = target_id else {
                            continue;
                        };

                        // Target resolves locally by bare name. Normally the
                        // initial bulk write (store_bulk.rs) already created this
                        // edge using rel.source_id/rel.target_id verbatim, so the
                        // common case is a no-op continue. But when rel.source_id
                        // is itself bare — extraction's find_enclosing_function
                        // only ever returns an unqualified name (see relations.rs),
                        // e.g. "DebugViewModel.cs::ExecuteCrashManagedBackground"
                        // rather than the real qualified
                        // "...::DebugViewModel::ExecuteCrashManagedBackground" —
                        // that bulk write's MATCH never finds a node and the edge
                        // silently never gets created (dropping every local
                        // same-class call, e.g. a WPF event handler's body calling
                        // another method on the same class). Detect that case by
                        // checking whether fixing up the source actually changes
                        // it; only then push a pair, so an already-correct
                        // source_id (the common case) stays a plain continue with
                        // no double-counted resolution.
                        let source_name =
                            rel.source_id.rsplit("::").next().unwrap_or(&rel.source_id);
                        // Ambiguous bare source name (two symbols in this
                        // file share it) means there's no way to tell which
                        // one made the call — leave rel.source_id as-is
                        // rather than guessing, same as the unqualified
                        // target-side ambiguity above.
                        if let Some([fixed_source_id]) =
                            local_symbols.get(source_name).map(Vec::as_slice)
                        {
                            let fixed_source_id = *fixed_source_id;
                            if fixed_source_id != rel.source_id {
                                res.pairs
                                    .push((fixed_source_id.to_string(), target_id.to_string()));
                                res.resolved += 1;
                            }
                        }
                        continue;
                    }
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

                // Strategy 2: Enclosing-class preference. Derive caller_class
                // from the qualified source id (via qualified_source_id), not
                // the raw rel.source_id directly — a bare source_id's
                // rsplit("::").nth(1) resolves to the file path segment, not
                // a real class name, which silently defeated the same-class
                // match below for the common bare-source case (falling
                // through to import_scope_match instead — deterministic, but
                // missing a real same-class candidate whenever no import
                // covers it).
                let qualified_source = qualified_source_id(&rel.source_id);
                let caller_class = qualified_source.rsplit("::").nth(1).map(|s| s.to_string());

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
                            // A call target must be invocable — never resolve
                            // `builder()` to a same-named field/variable
                            // (e.g. a builder-chain argument mis-resolving to
                            // an unrelated same-named field).
                            if kind != "Method" && kind != "Function" {
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

    // Reconcile bare source ids before returning. `rel.source_id` comes from
    // extraction's find_enclosing_function, which can return an unqualified
    // name (e.g. "{file}::ParseFIRegistryDetailResponse" instead of the real
    // "{file}::Import::ParseFIRegistryDetailResponse") — a caller's write-time
    // MATCH on the bare id would silently match zero rows, dropping an
    // otherwise-correctly-resolved edge with no trace. Pure logic, shared by
    // both backends' writers.
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

    let fix_source = |src: &String| -> Vec<String> {
        if known_ids.contains(src.as_str()) {
            return vec![src.clone()];
        }
        if let Some(sep) = src.rfind("::") {
            let file_part = &src[..sep];
            let name_part = &src[sep + 2..];
            if let Some(ids) = file_name_to_ids.get(&(file_part.to_string(), name_part.to_string()))
            {
                return ids
                    .iter()
                    .filter(|id| known_ids.contains(id.as_str()))
                    .cloned()
                    .collect();
            }
        }
        vec![src.clone()]
    };

    let fixed_pairs: Vec<(String, String)> = resolved_pairs
        .iter()
        .flat_map(|(src, tgt)| {
            fix_source(src)
                .into_iter()
                .map(|fixed_src| (fixed_src, tgt.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    // file_name_to_ids can carry the same id twice for one (file, name) key
    // (populated once from extractions and once from symbol_map), which
    // would otherwise fan a single call site out into duplicate CALLS edges.
    let mut seen_pairs: std::collections::HashSet<&(String, String)> =
        std::collections::HashSet::new();
    let pairs: Vec<(String, String)> = fixed_pairs
        .iter()
        .filter(|(src, tgt)| known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str()))
        .filter(|pair| seen_pairs.insert(pair))
        .cloned()
        .collect();

    let fixed_external: Vec<(String, String, String)> = external_calls
        .iter()
        .flat_map(|(caller, receiver, method)| {
            fix_source(caller)
                .into_iter()
                .map(|fixed_caller| (fixed_caller, receiver.clone(), method.clone()))
                .collect::<Vec<_>>()
        })
        .filter(|(caller, _, _)| known_ids.contains(caller.as_str()))
        .collect();
    let mut seen_external: std::collections::HashSet<&(String, String, String)> =
        std::collections::HashSet::new();
    let external_calls: Vec<(String, String, String)> = fixed_external
        .iter()
        .filter(|triple| seen_external.insert(triple))
        .cloned()
        .collect();

    ResolvedCalls {
        pairs,
        external_calls,
        stats: ResolveStats {
            total_calls: total_dangling,
            resolved,
            unresolved,
            learned_resolved,
            inherits_resolved: 0,
        },
    }
}

/// Kuzu's write wrapper around [`resolve_pairs`]: runs the shared decision
/// loop, then does the parquet-bulk-copy write and ExternalRef/EXTERNAL_CALL
/// write that only Kuzu's embedded `Connection` can do.
/// Caller must hold WriteLock.
/// Writes the resolved edges. Takes `&GraphStore` rather than a borrowed
/// `Connection` because `copy_edges_with_bad_record_retry` mints a fresh
/// connection per retry: a caught COPY failure can leave the connection it
/// ran on wedged, and the next COPY on that same connection then fails with
/// Kuzu's internal "Invalid transaction type to rollback." None of these
/// bulk loads share a transaction, so there is nothing to lose by not
/// sharing the connection.
fn write_resolved_calls(
    store: &GraphStore,
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    let ResolvedCalls {
        pairs,
        external_calls,
        stats,
    } = resolve_pairs(extractions, symbol_map, learned_store);

    if !pairs.is_empty() {
        let pq_path = unique_tmp_dir().join("infigraph_resolve_calls.parquet");
        copy_edges_with_bad_record_retry(store, "CALLS", pairs, "Symbol", "Symbol", &pq_path)?;
    }

    if !external_calls.is_empty() {
        let conn = store.connection()?;
        write_external_calls(&conn, &external_calls, symbol_map, extractions);
    }

    Ok(stats)
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
    let mut stats = write_resolved_calls(store, &filtered_owned, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(store, &filtered_owned, &symbol_map)?;
    Ok(stats)
}

pub(crate) fn import_scope_match(
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

pub(crate) fn shortest_id2<'a, I, F>(iter: I, pred: F) -> Option<String>
where
    I: Iterator<Item = &'a (String, String)>,
    F: Fn(&(String, String)) -> bool,
{
    iter.filter(|t| pred(t))
        .min_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
        .map(|(id, _)| id.clone())
}
