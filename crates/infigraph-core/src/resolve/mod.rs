use std::collections::HashMap;

use anyhow::Result;

use crate::graph::store::GraphStore;
use crate::learned::LearnedStore;
use crate::model::{FileExtraction, RelationKind};

/// Post-indexing pass that resolves call edges using cross-file symbol lookup.
/// Builds symbol map from the full graph (not just re-indexed files) so
/// incremental indexing doesn't lose cross-file resolution.
pub fn resolve_calls_incremental(
    store: &GraphStore,
    extractions: &[FileExtraction],
    learned_store: Option<&LearnedStore>,
) -> Result<ResolveStats> {
    if extractions.is_empty() {
        return Ok(ResolveStats { total_calls: 0, resolved: 0, unresolved: 0, learned_resolved: 0, inherits_resolved: 0 });
    }

    let conn = store.connection()?;

    // Build global symbol table from full graph: name -> [(id, file, kind)]
    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    for (name, id, file, kind) in store.get_all_symbols()? {
        symbol_map.entry(name).or_default().push((id, file, kind));
    }

    let mut stats = resolve_with_map(&conn, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, extractions, &symbol_map)?;
    Ok(stats)
}

/// Post-indexing pass that resolves call edges using cross-file symbol lookup.
///
/// Problem: During extraction, `authenticate()` called in `main.py` creates
/// a CALLS relation targeting `main.py::authenticate`. But the real symbol
/// is `auth.py::authenticate`. This pass:
///
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
            symbol_map
                .entry(sym.name.clone())
                .or_default()
                .push((sym.id.clone(), ext.file.clone(), sym.kind.as_str().to_string()));
        }
    }

    let mut stats = resolve_with_map(&conn, extractions, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, extractions, &symbol_map)?;
    Ok(stats)
}

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
                    class_method_map.entry(key).or_default().push((id.clone(), _file.clone()));
                }
            }
        }
    }

    for ext in extractions {
        let local_symbols: HashMap<&str, &str> = ext
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.id.as_str()))
            .collect();

        let imported_stems: std::collections::HashSet<String> = ext.relations.iter()
            .filter(|r| r.kind == RelationKind::Imports)
            .map(|r| {
                let raw = r.target_id
                    .rsplit(['/', '\\', '.'])
                    .next()
                    .unwrap_or(&r.target_id);
                raw.to_lowercase()
            })
            .collect();

        let source_is_sql = ext.file.ends_with(".sql");

        for rel in &ext.relations {
            if rel.kind != RelationKind::Calls {
                continue;
            }

            let target_name = rel
                .target_id
                .rsplit("::")
                .next()
                .unwrap_or(&rel.target_id);

            if local_symbols.contains_key(target_name) {
                continue;
            }

            total_dangling += 1;

            // Layer 3: Learned pattern lookup (from prior SCIP corrections).
            if let Some(ls) = learned_store {
                if let Some(pattern) = ls.lookup(&ext.file, target_name) {
                    let target_exists = symbol_map.values().any(|candidates| {
                        candidates.iter().any(|(id, _, _)| *id == pattern.resolved_to_symbol)
                    });
                    if target_exists {
                        resolved_pairs.push((rel.source_id.clone(), pattern.resolved_to_symbol.clone()));
                        resolved += 1;
                        learned_resolved += 1;
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
                        matches.iter()
                            .find(|(_, f)| {
                                let stem = std::path::Path::new(f)
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .map(|s| s.to_lowercase())
                                    .unwrap_or_default();
                                imported_stems.contains(&stem)
                            })
                            .or(matches.first())
                            .map(|(id, _)| id.clone())
                    };
                    if let Some(target_id) = best {
                        resolved_pairs.push((rel.source_id.clone(), target_id));
                        resolved += 1;
                        continue;
                    }
                }
            }

            // Strategy 2: Enclosing-class preference.
            let caller_class = rel.source_id
                .rsplit("::")
                .nth(1)
                .map(|s| s.to_string());

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
                        cross_file.iter()
                            .find(|(id, _, _)| {
                                id.contains(&format!("::{}::{}", recv, target_name))
                            })
                            .map(|(id, _, _)| id.clone())
                    });

                    if by_receiver.is_some() {
                        by_receiver
                    } else if let Some(ref cls) = caller_class {
                        let same_class = cross_file.iter()
                            .find(|(id, _, _)| id.contains(&format!("::{cls}::")))
                            .map(|(id, _, _)| id.clone());
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
                    resolved_pairs.push((rel.source_id.clone(), target_id));
                    resolved += 1;
                } else {
                    unresolved += 1;
                }
            } else {
                unresolved += 1;
            }
        }
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
                file_name_to_ids.entry((ext.file.clone(), sym.name.clone()))
                    .or_default().push(sym.id.clone());
            }
        }
        for candidates in symbol_map.values() {
            for (id, file, _kind) in candidates {
                let name = id.rsplit("::").next().unwrap_or(id);
                file_name_to_ids.entry((file.clone(), name.to_string()))
                    .or_default().push(id.clone());
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
                    if let Some(ids) = file_name_to_ids.get(&(file_part.to_string(), name_part.to_string())) {
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

        let valid_pairs: Vec<&(String, String)> = fixed_pairs
            .iter()
            .filter(|(src, tgt)| known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str()))
            .collect();

        let refs: Vec<(&str, &str)> = valid_pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let pq_path = std::env::temp_dir().join("infigraph_resolve_calls.parquet");
        crate::graph::parquet_loader::write_edge_parquet(&pq_path, &refs)?;
        let copy_result = conn.query(&format!(
            "COPY CALLS FROM '{}'",
            pq_path.to_string_lossy().replace('\\', "/")
        ));
        if let Err(e) = copy_result {
            eprintln!("[resolve] COPY FROM parquet failed ({e}), falling back to UNWIND");
            const CHUNK_SIZE: usize = 500;
            for chunk in refs.chunks(CHUNK_SIZE) {
                let pair_list: Vec<String> = chunk.iter().map(|(a, b)| {
                    format!("{{a: '{}', b: '{}'}}", escape(a), escape(b))
                }).collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:CALLS]->(b)",
                    pair_list.join(", ")
                ));
            }
        }
        let _ = std::fs::remove_file(&pq_path);
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
        return Ok(ResolveStats { total_calls: 0, resolved: 0, unresolved: 0, learned_resolved: 0, inherits_resolved: 0 });
    }

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
    let filtered: Vec<&FileExtraction> = extractions.iter()
        .filter(|e| target_files.contains(e.file.as_str()))
        .collect();

    let filtered_owned: Vec<FileExtraction> = filtered.into_iter().cloned().collect();
    let mut stats = resolve_with_map(&conn, &filtered_owned, &symbol_map, learned_store)?;
    stats.inherits_resolved = resolve_inherits(&conn, &filtered_owned, &symbol_map)?;
    Ok(stats)
}

#[derive(Debug)]
pub struct ResolveStats {
    pub total_calls: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub learned_resolved: usize,
    pub inherits_resolved: usize,
}

impl std::fmt::Display for ResolveStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.learned_resolved > 0 {
            write!(
                f,
                "Call resolution: {} cross-file calls, {} resolved ({} from learned patterns), {} unresolved (builtins/externals)",
                self.total_calls, self.resolved, self.learned_resolved, self.unresolved
            )?;
        } else {
            write!(
                f,
                "Call resolution: {} cross-file calls, {} resolved, {} unresolved (builtins/externals)",
                self.total_calls, self.resolved, self.unresolved
            )?;
        }
        if self.inherits_resolved > 0 {
            write!(f, ", {} inheritance edges resolved", self.inherits_resolved)?;
        }
        Ok(())
    }
}

const TYPE_KINDS: &[&str] = &["Class", "Interface", "Struct", "Trait", "Enum"];

fn resolve_inherits(
    conn: &kuzu::Connection<'_>,
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
) -> Result<usize> {
    let mut resolved_pairs: Vec<(String, String)> = Vec::new();

    for ext in extractions {
        let local_symbols: std::collections::HashSet<&str> = ext
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect();

        let imported_stems: std::collections::HashSet<String> = ext.relations.iter()
            .filter(|r| r.kind == RelationKind::Imports)
            .map(|r| {
                let raw = r.target_id
                    .rsplit(['/', '\\', '.'])
                    .next()
                    .unwrap_or(&r.target_id);
                raw.to_lowercase()
            })
            .collect();

        for rel in &ext.relations {
            if rel.kind != RelationKind::Inherits {
                continue;
            }

            let target_name = rel
                .target_id
                .rsplit("::")
                .next()
                .unwrap_or(&rel.target_id);

            if local_symbols.contains(target_name) {
                continue;
            }

            if let Some(candidates) = symbol_map.get(target_name) {
                let cross_file: Vec<_> = candidates
                    .iter()
                    .filter(|(_, f, kind)| {
                        *f != ext.file && TYPE_KINDS.contains(&kind.as_str())
                    })
                    .collect();

                let resolved_id = if cross_file.len() == 1 {
                    Some(cross_file[0].0.clone())
                } else if cross_file.len() > 1 {
                    let in_scope = cross_file.iter().find(|(_, f, _)| {
                        let stem = std::path::Path::new(f)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_lowercase())
                            .unwrap_or_default();
                        imported_stems.contains(&stem)
                    });
                    let by_kind = cross_file.iter().find(|(_, _, k)| k == "Interface");
                    in_scope.or(by_kind).or(cross_file.first())
                        .map(|(id, _, _)| id.clone())
                } else {
                    None
                };

                if let Some(target_id) = resolved_id {
                    resolved_pairs.push((rel.source_id.clone(), target_id));
                }
            }
        }
    }

    if resolved_pairs.is_empty() {
        return Ok(0);
    }

    let count = resolved_pairs.len();

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
            file_name_to_ids.entry((ext.file.clone(), sym.name.clone()))
                .or_default().push(sym.id.clone());
        }
    }
    for candidates in symbol_map.values() {
        for (id, file, _) in candidates {
            let name = id.rsplit("::").next().unwrap_or(id);
            file_name_to_ids.entry((file.clone(), name.to_string()))
                .or_default().push(id.clone());
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
                if let Some(ids) = file_name_to_ids.get(&(file_part.to_string(), name_part.to_string())) {
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

    let valid_pairs: Vec<&(String, String)> = fixed_pairs
        .iter()
        .filter(|(src, tgt)| known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str()))
        .collect();

    if valid_pairs.is_empty() {
        return Ok(0);
    }

    let refs: Vec<(&str, &str)> = valid_pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let pq_path = std::env::temp_dir().join("infigraph_resolve_inherits.parquet");
    crate::graph::parquet_loader::write_edge_parquet(&pq_path, &refs)?;
    let copy_result = conn.query(&format!(
        "COPY INHERITS FROM '{}'",
        pq_path.to_string_lossy().replace('\\', "/")
    ));
    if let Err(e) = copy_result {
        eprintln!("[resolve] COPY INHERITS FROM parquet failed ({e}), falling back to UNWIND");
        const CHUNK_SIZE: usize = 500;
        for chunk in refs.chunks(CHUNK_SIZE) {
            let pair_list: Vec<String> = chunk.iter().map(|(a, b)| {
                format!("{{a: '{}', b: '{}'}}", escape(a), escape(b))
            }).collect();
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:INHERITS]->(b)",
                pair_list.join(", ")
            ));
        }
    }
    let _ = std::fs::remove_file(&pq_path);

    Ok(count)
}

fn import_scope_match(
    cross_file: &[&(String, String, String)],
    imported_stems: &std::collections::HashSet<String>,
    source_is_sql: bool,
) -> Option<String> {
    let in_scope: Vec<_> = if !imported_stems.is_empty() {
        cross_file.iter()
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
        Some(in_scope[0].0.clone())
    } else if source_is_sql {
        cross_file.iter()
            .find(|(_, _, k)| *k == "Class")
            .map(|(id, _, _)| id.clone())
    } else {
        None
    }
}

fn escape(s: &str) -> String {
    s.replace('\'', "\\'")
}
