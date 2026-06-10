//! Combined graph: merge per-repo Kuzu DBs into a single graph for cross-repo queries.
//!
//! Uses Kuzu COPY TO → arrow transform (prefix IDs) → COPY FROM pipeline.
//! No row-by-row string serialization — pure bulk parquet I/O.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Array, StringArray};
use arrow::datatypes::DataType;

use crate::graph::parquet_loader;
use crate::graph::store::GraphStore;
use crate::graph::GraphQuery;

use super::Registry;

/// Build (or rebuild) a combined graph for a group.
/// Returns (symbol_count, edge_count) in the combined graph.
pub fn build_combined_graph(
    registry: &Registry,
    group_name: &str,
) -> Result<(usize, usize)> {
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;

    let combined_path = combined_graph_path(group_name)?;
    if combined_path.exists() {
        if combined_path.is_dir() {
            std::fs::remove_dir_all(&combined_path)?;
        } else {
            std::fs::remove_file(&combined_path)?;
        }
    }
    // Also clean WAL file if present
    let wal_path = combined_path.with_extension("wal");
    if wal_path.exists() {
        let _ = std::fs::remove_file(&wal_path);
    }

    let combined_store = GraphStore::open(&combined_path)?;
    let combined_conn = combined_store.connection()?;
    let tmp = std::env::temp_dir().join("ig_combined");
    std::fs::create_dir_all(&tmp)?;
    let fwd = |p: &Path| p.to_string_lossy().replace('\\', "/");

    let mut total_symbols = 0usize;
    let mut total_edges = 0usize;
    let mut known_sym_ids: HashSet<String> = HashSet::new();
    let mut known_mod_ids: HashSet<String> = HashSet::new();

    // Phase 1: Export each repo via COPY TO, prefix IDs, import into combined
    for repo_name in &group.repos {
        let entry = registry
            .repos
            .get(repo_name)
            .context(format!("repo '{}' not in registry", repo_name))?;

        let repo_db_path = entry.path.join(".infigraph").join("graph");
        if !repo_db_path.exists() {
            eprintln!("  [skip] {} — not indexed", repo_name);
            continue;
        }

        let t0 = std::time::Instant::now();
        let repo_store = GraphStore::open(&repo_db_path)?;
        let repo_conn = repo_store.connection()?;
        let prefix = format!("[{}]::", repo_name);

        // Export Symbol table to parquet
        let sym_export = tmp.join(format!("{}_symbols.parquet", repo_name));
        let sym_out = tmp.join(format!("{}_symbols_prefixed.parquet", repo_name));
        repo_conn.query(&format!(
            "COPY (MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, \
             s.start_line, s.end_line, s.signature_hash, s.language, \
             s.visibility, s.parent, s.docstring, s.complexity) TO '{}'",
            fwd(&sym_export)
        )).map_err(|e| anyhow::anyhow!("COPY Symbol TO failed for {}: {}", repo_name, e))?;

        let sym_count = prefix_parquet_columns(
            &sym_export, &sym_out, &prefix,
            &[0, 3],  // id, file
            Some(&[(9, true)]),  // parent (skip empty)
            &mut known_sym_ids, 0,
        )?;
        total_symbols += sym_count;
        let _ = std::fs::remove_file(&sym_export);

        combined_conn.query(&format!(
            "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity) FROM '{}'",
            fwd(&sym_out)
        )).map_err(|e| anyhow::anyhow!("COPY Symbol FROM failed: {e}"))?;
        let _ = std::fs::remove_file(&sym_out);

        // Export Module table
        let mod_export = tmp.join(format!("{}_modules.parquet", repo_name));
        let mod_out = tmp.join(format!("{}_modules_prefixed.parquet", repo_name));
        repo_conn.query(&format!(
            "COPY (MATCH (m:Module) RETURN m.id, m.name, m.file, m.language, m.content_hash, m.summary) TO '{}'",
            fwd(&mod_export)
        )).map_err(|e| anyhow::anyhow!("COPY Module TO failed for {}: {}", repo_name, e))?;

        prefix_parquet_columns(
            &mod_export, &mod_out, &prefix,
            &[0, 2],  // id, file
            None, &mut known_mod_ids, 0,
        )?;
        let _ = std::fs::remove_file(&mod_export);

        combined_conn.query(&format!("COPY Module FROM '{}'", fwd(&mod_out)))
            .map_err(|e| anyhow::anyhow!("COPY Module FROM failed: {e}"))?;
        let _ = std::fs::remove_file(&mod_out);

        // Export File table
        let file_export = tmp.join(format!("{}_files.parquet", repo_name));
        let file_out = tmp.join(format!("{}_files_prefixed.parquet", repo_name));
        repo_conn.query(&format!(
            "COPY (MATCH (f:File) RETURN f.id, f.name, f.path, f.language, f.symbol_count) TO '{}'",
            fwd(&file_export)
        )).map_err(|e| anyhow::anyhow!("COPY File TO failed for {}: {}", repo_name, e))?;

        prefix_parquet_columns(
            &file_export, &file_out, &prefix,
            &[0, 2],  // id, path
            None, &mut HashSet::new(), 0,
        )?;
        let _ = std::fs::remove_file(&file_export);

        combined_conn.query(&format!("COPY File FROM '{}'", fwd(&file_out)))
            .map_err(|e| anyhow::anyhow!("COPY File FROM failed: {e}"))?;
        let _ = std::fs::remove_file(&file_out);

        // Export edge tables
        let edge_names = ["CALLS", "INHERITS", "IMPORTS", "CONTAINS", "DEFINES", "TESTED_BY", "READS", "WRITES"];
        for edge_name in &edge_names {
            let edge_export = tmp.join(format!("{}_{}.parquet", repo_name, edge_name.to_lowercase()));
            let edge_out = tmp.join(format!("{}_{}_prefixed.parquet", repo_name, edge_name.to_lowercase()));

            let export_result = repo_conn.query(&format!(
                "COPY (MATCH (a)-[:{}]->(b) RETURN a.id, b.id) TO '{}'",
                edge_name, fwd(&edge_export)
            ));
            if export_result.is_err() {
                continue; // Table may not exist or be empty
            }

            let edge_count = prefix_edge_parquet(&edge_export, &edge_out, &prefix)?;
            let _ = std::fs::remove_file(&edge_export);

            if edge_count > 0 {
                if let Err(e) = combined_conn.query(&format!("COPY {} FROM '{}'", edge_name, fwd(&edge_out))) {
                    eprintln!("  warn: COPY {} FROM failed: {}", edge_name, e);
                } else {
                    total_edges += edge_count;
                }
            }
            let _ = std::fs::remove_file(&edge_out);
        }

        eprintln!(
            "  [combined] {} — {} symbols in {:.1}s",
            repo_name, sym_count, t0.elapsed().as_secs_f64()
        );
    }

    // Phase 2: Cross-repo resolution on combined graph
    let cross_resolved = resolve_cross_repo(&combined_store)?;
    total_edges += cross_resolved;

    let _ = std::fs::remove_dir_all(&tmp);

    eprintln!(
        "[combined] Done: {} symbols, {} edges ({} cross-repo)",
        total_symbols, total_edges, cross_resolved
    );

    Ok((total_symbols, total_edges))
}

/// Read a parquet file, prefix specified string columns, write back.
/// Tracks IDs in `id_set` (column at `id_col_idx`). Returns row count.
fn prefix_parquet_columns(
    input: &Path,
    output: &Path,
    prefix: &str,
    prefix_cols: &[usize],
    conditional_prefix_cols: Option<&[(usize, bool)]>, // (col_idx, skip_empty)
    id_set: &mut HashSet<String>,
    id_col_idx: usize,
) -> Result<usize> {
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;

    let file = std::fs::File::open(input)
        .with_context(|| format!("open {}", input.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?
        .build()?;

    let out_file = std::fs::File::create(output)?;
    let mut writer: Option<ArrowWriter<std::fs::File>> = None;
    let mut total_rows = 0usize;

    for batch_result in reader {
        let batch = batch_result?;
        let num_rows = batch.num_rows();
        if num_rows == 0 { continue; }
        total_rows += num_rows;

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for (i, col) in batch.columns().iter().enumerate() {
            let should_prefix = prefix_cols.contains(&i);
            let conditional = conditional_prefix_cols
                .and_then(|c| c.iter().find(|(idx, _)| *idx == i));

            if should_prefix || conditional.is_some() {
                let skip_empty = conditional.map(|(_, skip)| *skip).unwrap_or(false);
                let str_arr = col.as_any().downcast_ref::<StringArray>().unwrap();
                let prefixed: StringArray = (0..num_rows)
                    .map(|j| {
                        let val = str_arr.value(j);
                        if skip_empty && val.is_empty() {
                            Some(String::new())
                        } else {
                            Some(format!("{}{}", prefix, val))
                        }
                    })
                    .collect();
                columns.push(Arc::new(prefixed));
            } else {
                columns.push(col.clone());
            }
        }

        // Track IDs
        if id_col_idx < columns.len() {
            let id_col = columns[id_col_idx].as_any().downcast_ref::<StringArray>().unwrap();
            for j in 0..num_rows {
                id_set.insert(id_col.value(j).to_string());
            }
        }

        let new_batch = RecordBatch::try_new(batch.schema(), columns)?;
        if writer.is_none() {
            writer = Some(ArrowWriter::try_new(
                out_file.try_clone()?,
                batch.schema(),
                None,
            )?);
        }
        writer.as_mut().unwrap().write(&new_batch)?;
    }

    if let Some(w) = writer {
        w.close()?;
    } else if total_rows == 0 {
        // Write empty parquet with minimal schema
        let schema = Arc::new(arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("id", DataType::Utf8, false),
        ]));
        let w = ArrowWriter::try_new(out_file, schema, None)?;
        w.close()?;
    }

    Ok(total_rows)
}

/// Read edge parquet (2 string columns: src, tgt), prefix both, write back.
fn prefix_edge_parquet(input: &Path, output: &Path, prefix: &str) -> Result<usize> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;

    let file = match std::fs::File::open(input) {
        Ok(f) => f,
        Err(_) => return Ok(0),
    };

    let meta = file.metadata()?;
    if meta.len() == 0 { return Ok(0); }

    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?
        .build()?;

    let out_file = std::fs::File::create(output)?;
    let mut writer: Option<ArrowWriter<std::fs::File>> = None;
    let mut total = 0usize;

    for batch_result in reader {
        let batch = batch_result?;
        let n = batch.num_rows();
        if n == 0 { continue; }
        total += n;

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for col in batch.columns() {
            let str_arr = col.as_any().downcast_ref::<StringArray>().unwrap();
            let prefixed: StringArray = (0..n)
                .map(|j| Some(format!("{}{}", prefix, str_arr.value(j))))
                .collect();
            columns.push(Arc::new(prefixed));
        }

        let new_batch = arrow::record_batch::RecordBatch::try_new(batch.schema(), columns)?;
        if writer.is_none() {
            writer = Some(ArrowWriter::try_new(out_file.try_clone()?, batch.schema(), None)?);
        }
        writer.as_mut().unwrap().write(&new_batch)?;
    }

    if let Some(w) = writer {
        w.close()?;
    }

    Ok(total)
}

/// Run cross-repo call and inheritance resolution on the combined graph.
fn resolve_cross_repo(store: &GraphStore) -> Result<usize> {
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);

    // Build symbol map: name → [(id, file, kind)]
    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.name, s.id, s.file, s.kind"
    )?;
    for row in &rows {
        if row.len() >= 4 {
            symbol_map
                .entry(row[0].clone())
                .or_default()
                .push((row[1].clone(), row[2].clone(), row[3].clone()));
        }
    }

    let mut new_calls = 0;
    let mut new_inherits = 0;

    // Cross-repo INHERITS: types with same name in multiple repos
    let mut name_to_ids: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (name, entries) in &symbol_map {
        let type_entries: Vec<_> = entries.iter()
            .filter(|(_, _, k)| matches!(k.as_str(), "Class" | "Interface" | "Struct" | "Trait" | "Enum"))
            .collect();
        if type_entries.len() >= 2 {
            for (id, _, _) in &type_entries {
                let repo = extract_repo(id);
                name_to_ids.entry(name.clone()).or_default().push((id.clone(), repo.to_string()));
            }
        }
    }

    // Batch: collect all inheritor→target pairs to create
    let mut inh_pairs: Vec<(String, String)> = Vec::new();
    for id_repos in name_to_ids.values() {
        if id_repos.len() < 2 { continue; }
        for (target_id, _) in id_repos {
            let escaped = target_id.replace('\'', "\\'");
            let inheritors = gq.raw_query(&format!(
                "MATCH (s:Symbol)-[:INHERITS]->(t:Symbol {{id: '{}'}}) RETURN s.id", escaped
            )).unwrap_or_default();

            for row in &inheritors {
                if row.is_empty() { continue; }
                let inh_repo = extract_repo(&row[0]);
                for (other_id, other_repo) in id_repos {
                    if other_repo == inh_repo || other_id == target_id { continue; }
                    inh_pairs.push((row[0].clone(), other_id.clone()));
                }
            }
        }
    }
    // Deduplicate and write
    let inh_pairs: Vec<(String, String)> = {
        let mut seen = HashSet::new();
        inh_pairs.into_iter().filter(|p| seen.insert((p.0.clone(), p.1.clone()))).collect()
    };
    if !inh_pairs.is_empty() {
        let refs: Vec<(&str, &str)> = inh_pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let pq = std::env::temp_dir().join("ig_combined_cross_inherits.parquet");
        parquet_loader::write_edge_parquet(&pq, &refs)?;
        if let Err(e) = conn.query(&format!(
            "COPY INHERITS FROM '{}'", pq.to_string_lossy().replace('\\', "/")
        )) {
            eprintln!("  warn: COPY cross-repo INHERITS failed ({e}), using UNWIND");
            for chunk in refs.chunks(500) {
                let items: Vec<String> = chunk.iter()
                    .map(|(a, b)| format!("{{a:'{}',b:'{}'}}", a.replace('\'', "\\'"), b.replace('\'', "\\'")))
                    .collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS p MATCH (a:Symbol),(b:Symbol) WHERE a.id=p.a AND b.id=p.b CREATE (a)-[:INHERITS]->(b)",
                    items.join(",")
                ));
            }
        }
        let _ = std::fs::remove_file(&pq);
        new_inherits = inh_pairs.len();
    }

    // Cross-repo CALLS: type-directed. For each INHERITS pair (child→parent across repos),
    // find methods in child's file, match to same-named methods in parent's file,
    // then link callers of child methods to parent methods. Avoids cartesian products.
    let mut type_file_map: HashMap<String, String> = HashMap::new();
    for entries in symbol_map.values() {
        for (id, file, _) in entries {
            type_file_map.insert(id.clone(), file.clone());
        }
    }

    // Build file→methods index for INHERITS-chain type files
    let mut file_methods: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for (name, entries) in &symbol_map {
        for (id, file, kind) in entries {
            if kind == "Class" || kind == "Interface" || kind == "Struct"
                || kind == "Trait" || kind == "Enum" || kind == "Module" { continue; }
            file_methods.entry(file.clone())
                .or_default()
                .entry(name.clone())
                .or_default()
                .push(id.clone());
        }
    }

    let mut call_pairs: Vec<(String, String)> = Vec::new();
    let mut processed_pairs: HashSet<(String, String)> = HashSet::new();

    for (child_id, parent_id) in &inh_pairs {
        let child_file = match type_file_map.get(child_id) { Some(f) => f, None => continue };
        let parent_file = match type_file_map.get(parent_id) { Some(f) => f, None => continue };
        let child_repo = extract_repo(child_id);
        let parent_repo = extract_repo(parent_id);
        if child_repo == parent_repo { continue; }

        let child_methods = match file_methods.get(child_file) { Some(m) => m, None => continue };
        let parent_methods = match file_methods.get(parent_file) { Some(m) => m, None => continue };

        for (method_name, child_method_ids) in child_methods {
            let parent_method_ids = match parent_methods.get(method_name) {
                Some(ids) => ids,
                None => continue,
            };
            // For each child method, find its callers → link to parent methods
            for cmid in child_method_ids {
                let pair_key = (cmid.clone(), parent_file.clone());
                if !processed_pairs.insert(pair_key) { continue; }
                let escaped = cmid.replace('\'', "\\'");
                let callers = gq.raw_query(&format!(
                    "MATCH (c:Symbol)-[:CALLS]->(t:Symbol {{id: '{}'}}) RETURN c.id", escaped
                )).unwrap_or_default();
                for crow in &callers {
                    if crow.is_empty() { continue; }
                    for pid in parent_method_ids {
                        if extract_repo(&crow[0]) != parent_repo {
                            call_pairs.push((crow[0].clone(), pid.clone()));
                        }
                    }
                }
            }
        }
    }

    eprintln!("  [combined] Type-directed: {} INHERITS pairs → {} raw CALLS candidates",
        inh_pairs.len(), call_pairs.len());
    let call_pairs: Vec<(String, String)> = {
        let mut seen = HashSet::new();
        call_pairs.into_iter().filter(|p| seen.insert((p.0.clone(), p.1.clone()))).collect()
    };
    if !call_pairs.is_empty() {
        let refs: Vec<(&str, &str)> = call_pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let pq = std::env::temp_dir().join("ig_combined_cross_calls.parquet");
        parquet_loader::write_edge_parquet(&pq, &refs)?;
        if let Err(e) = conn.query(&format!(
            "COPY CALLS FROM '{}'", pq.to_string_lossy().replace('\\', "/")
        )) {
            eprintln!("  warn: COPY cross-repo CALLS failed ({e}), using UNWIND");
            for chunk in refs.chunks(500) {
                let items: Vec<String> = chunk.iter()
                    .map(|(a, b)| format!("{{a:'{}',b:'{}'}}", a.replace('\'', "\\'"), b.replace('\'', "\\'")))
                    .collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS p MATCH (a:Symbol),(b:Symbol) WHERE a.id=p.a AND b.id=p.b CREATE (a)-[:CALLS]->(b)",
                    items.join(",")
                ));
            }
        }
        let _ = std::fs::remove_file(&pq);
        new_calls = call_pairs.len();
    }

    eprintln!(
        "  [combined] Cross-repo: {} CALLS, {} INHERITS",
        new_calls, new_inherits
    );

    Ok(new_calls + new_inherits)
}

/// Open the combined graph store for a group.
pub fn open_combined_graph(group_name: &str) -> Result<GraphStore> {
    let path = combined_graph_path(group_name)?;
    if !path.exists() {
        anyhow::bail!(
            "Combined graph not found for group '{}'. Run 'infigraph group combined {}' first.",
            group_name, group_name
        );
    }
    GraphStore::open(&path)
}

/// Query the combined graph with a single Cypher query.
pub fn combined_query(group_name: &str, cypher: &str) -> Result<Vec<Vec<String>>> {
    let store = open_combined_graph(group_name)?;
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);
    gq.raw_query(cypher)
}

pub fn combined_graph_path(group_name: &str) -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".infigraph").join("groups").join(group_name).join(".infigraph").join("graph"))
}

pub fn has_combined_graph(group_name: &str) -> bool {
    combined_graph_path(group_name).map(|p| p.exists()).unwrap_or(false)
}

pub fn strip_prefix(id: &str) -> &str {
    if let Some(idx) = id.find("]::") { &id[idx + 3..] } else { id }
}

pub fn extract_repo(id: &str) -> &str {
    if id.starts_with('[') {
        if let Some(idx) = id.find("]::") { return &id[1..idx]; }
    }
    ""
}
