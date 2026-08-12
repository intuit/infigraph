use std::collections::HashMap;

use anyhow::Result;

use crate::graph::store_util::copy_edges_with_bad_record_retry;
use crate::model::{FileExtraction, RelationKind};

use super::shortest_id;

const TYPE_KINDS: &[&str] = &["Class", "Interface", "Struct", "Trait", "Enum"];

/// Caller must hold WriteLock.
pub(crate) fn resolve_inherits(
    conn: &kuzu::Connection<'_>,
    extractions: &[FileExtraction],
    symbol_map: &HashMap<String, Vec<(String, String, String)>>,
) -> Result<usize> {
    let mut resolved_pairs: Vec<(String, String)> = Vec::new();

    // Span size per symbol id, used to prefer a real definition over a
    // same-named forward-declaration stub (e.g. `class ITpsContext;`) when
    // multiple cross-file candidates remain after import/kind disambiguation.
    let span_by_id: HashMap<&str, u32> = extractions
        .iter()
        .flat_map(|ext| ext.symbols.iter())
        .map(|sym| {
            (
                sym.id.as_str(),
                sym.span.end_line.saturating_sub(sym.span.start_line),
            )
        })
        .collect();

    for ext in extractions {
        let local_symbols: std::collections::HashSet<&str> =
            ext.symbols.iter().map(|s| s.name.as_str()).collect();

        let imported_stems: std::collections::HashSet<String> = ext
            .relations
            .iter()
            .filter(|r| r.kind == RelationKind::Imports)
            .map(|r| {
                let raw = r
                    .target_id
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

            let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);

            if local_symbols.contains(target_name) {
                continue;
            }

            if let Some(candidates) = symbol_map.get(target_name) {
                let cross_file: Vec<_> = candidates
                    .iter()
                    .filter(|(_, f, kind)| *f != ext.file && TYPE_KINDS.contains(&kind.as_str()))
                    .collect();

                let resolved_id = if cross_file.len() == 1 {
                    Some(cross_file[0].0.clone())
                } else if cross_file.len() > 1 {
                    let in_scope = shortest_id(cross_file.iter().copied(), |(_, f, _)| {
                        let stem = std::path::Path::new(f)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_lowercase())
                            .unwrap_or_default();
                        imported_stems.contains(&stem)
                    });
                    let by_kind = in_scope
                        .is_none()
                        .then(|| {
                            shortest_id(cross_file.iter().copied(), |(_, _, k)| k == "Interface")
                        })
                        .flatten();
                    // Prefer the candidate with the largest body — a real definition
                    // over a same-named forward-declaration stub (e.g. `class Foo;`,
                    // which spans 0 lines and has no members).
                    let by_definition_size = cross_file
                        .iter()
                        .max_by_key(|(id, _, _)| span_by_id.get(id.as_str()).copied().unwrap_or(0))
                        .filter(|(id, _, _)| span_by_id.get(id.as_str()).copied().unwrap_or(0) > 0)
                        .map(|(id, _, _)| id.clone());
                    in_scope.or(by_kind).or(by_definition_size).or_else(|| {
                        cross_file
                            .iter()
                            .min_by(|(a, _, _), (b, _, _)| {
                                a.len().cmp(&b.len()).then_with(|| a.cmp(b))
                            })
                            .map(|(id, _, _)| id.clone())
                    })
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
            file_name_to_ids
                .entry((ext.file.clone(), sym.name.clone()))
                .or_default()
                .push(sym.id.clone());
        }
    }
    for candidates in symbol_map.values() {
        for (id, file, _) in candidates {
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

    let valid_pairs: Vec<&(String, String)> = fixed_pairs
        .iter()
        .filter(|(src, tgt)| known_ids.contains(src.as_str()) && known_ids.contains(tgt.as_str()))
        .collect();

    if valid_pairs.is_empty() {
        return Ok(0);
    }

    let pairs: Vec<(String, String)> = valid_pairs
        .into_iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect();
    let pq_path = std::env::temp_dir().join("infigraph_resolve_inherits.parquet");
    copy_edges_with_bad_record_retry(conn, "INHERITS", pairs, "Symbol", "Symbol", &pq_path);

    Ok(count)
}
