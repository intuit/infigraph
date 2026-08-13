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
pub fn build_combined_graph(registry: &Registry, group_name: &str) -> Result<(usize, usize)> {
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
    let _lock = combined_store.write_lock()?;
    let combined_conn = combined_store.connection()?;
    let tmp_dir =
        tempfile::TempDir::new().context("failed to create temp dir for combined graph")?;
    let tmp = tmp_dir.path().to_path_buf();
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
        repo_conn
            .query(&format!(
                "COPY (MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, \
             s.start_line, s.end_line, s.signature_hash, s.language, \
             s.visibility, s.parent, s.docstring, s.complexity, s.category) TO '{}'",
                fwd(&sym_export)
            ))
            .map_err(|e| anyhow::anyhow!("COPY Symbol TO failed for {}: {}", repo_name, e))?;

        let sym_count = prefix_parquet_columns(
            &sym_export,
            &sym_out,
            &prefix,
            &[0, 3],            // id, file
            Some(&[(9, true)]), // parent (skip empty)
            &mut known_sym_ids,
            0,
        )?;
        total_symbols += sym_count;
        let _ = std::fs::remove_file(&sym_export);

        combined_conn.query(&format!(
            "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, category) FROM '{}'",
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
            &mod_export,
            &mod_out,
            &prefix,
            &[0, 2], // id, file
            None,
            &mut known_mod_ids,
            0,
        )?;
        let _ = std::fs::remove_file(&mod_export);

        combined_conn
            .query(&format!("COPY Module FROM '{}'", fwd(&mod_out)))
            .map_err(|e| anyhow::anyhow!("COPY Module FROM failed: {e}"))?;
        let _ = std::fs::remove_file(&mod_out);

        // Export File table
        let file_export = tmp.join(format!("{}_files.parquet", repo_name));
        let file_out = tmp.join(format!("{}_files_prefixed.parquet", repo_name));
        repo_conn
            .query(&format!(
            "COPY (MATCH (f:File) RETURN f.id, f.name, f.path, f.language, f.symbol_count) TO '{}'",
            fwd(&file_export)
        ))
            .map_err(|e| anyhow::anyhow!("COPY File TO failed for {}: {}", repo_name, e))?;

        prefix_parquet_columns(
            &file_export,
            &file_out,
            &prefix,
            &[0, 2], // id, path
            None,
            &mut HashSet::new(),
            0,
        )?;
        let _ = std::fs::remove_file(&file_export);

        combined_conn
            .query(&format!("COPY File FROM '{}'", fwd(&file_out)))
            .map_err(|e| anyhow::anyhow!("COPY File FROM failed: {e}"))?;
        let _ = std::fs::remove_file(&file_out);

        // Export edge tables
        let edge_names = [
            "CALLS",
            "INHERITS",
            "IMPORTS",
            "CONTAINS",
            "DEFINES",
            "TESTED_BY",
            "READS",
            "WRITES",
            "REGISTERS_MIDDLEWARE",
            "INJECTS_DEPENDENCY",
        ];
        for edge_name in &edge_names {
            let edge_export = tmp.join(format!(
                "{}_{}.parquet",
                repo_name,
                edge_name.to_lowercase()
            ));
            let edge_out = tmp.join(format!(
                "{}_{}_prefixed.parquet",
                repo_name,
                edge_name.to_lowercase()
            ));

            let export_result = repo_conn.query(&format!(
                "COPY (MATCH (a)-[:{}]->(b) RETURN a.id, b.id) TO '{}'",
                edge_name,
                fwd(&edge_export)
            ));
            if export_result.is_err() {
                continue; // Table may not exist or be empty
            }

            let edge_count = prefix_edge_parquet(&edge_export, &edge_out, &prefix, &[0, 1])?;
            let _ = std::fs::remove_file(&edge_export);

            if edge_count > 0 {
                if let Err(e) =
                    combined_conn.query(&format!("COPY {} FROM '{}'", edge_name, fwd(&edge_out)))
                {
                    eprintln!("  warn: COPY {} FROM failed: {}", edge_name, e);
                } else {
                    total_edges += edge_count;
                }
            }
            let _ = std::fs::remove_file(&edge_out);
        }

        // Export CALLS_SERVICE edges (has extra property columns)
        {
            let cs_export = tmp.join(format!("{}_calls_service.parquet", repo_name));
            let cs_out = tmp.join(format!("{}_calls_service_prefixed.parquet", repo_name));
            let export_ok = repo_conn.query(&format!(
                "COPY (MATCH (a)-[r:CALLS_SERVICE]->(b) RETURN a.id, b.id, r.method, r.path, r.target_service, r.protocol, r.qualifier) TO '{}'",
                fwd(&cs_export)
            ));
            if export_ok.is_ok() {
                let cs_count = prefix_edge_parquet(&cs_export, &cs_out, &prefix, &[0, 1])?;
                let _ = std::fs::remove_file(&cs_export);
                if cs_count > 0 {
                    if let Err(e) =
                        combined_conn.query(&format!("COPY CALLS_SERVICE FROM '{}'", fwd(&cs_out)))
                    {
                        eprintln!("  warn: COPY CALLS_SERVICE FROM failed: {}", e);
                    } else {
                        total_edges += cs_count;
                    }
                }
                let _ = std::fs::remove_file(&cs_out);
            }
        }

        eprintln!(
            "  [combined] {} — {} symbols in {:.1}s",
            repo_name,
            sym_count,
            t0.elapsed().as_secs_f64()
        );
    }

    // Phase 2: Cross-repo resolution on combined graph
    let contracts = group.contracts.clone();
    let cross_resolved = resolve_cross_repo(&combined_store, &contracts)?;
    total_edges += cross_resolved;

    drop(tmp_dir);

    eprintln!(
        "[combined] Done: {} symbols, {} edges ({} cross-repo)",
        total_symbols, total_edges, cross_resolved
    );

    Ok((total_symbols, total_edges))
}

/// Read a parquet file, prefix specified string columns, write back.
/// Tracks IDs in `id_set` (column at `id_col_idx`). Returns row count.
pub fn prefix_parquet_columns(
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

    let file = std::fs::File::open(input).with_context(|| format!("open {}", input.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

    let out_file = std::fs::File::create(output)?;
    let mut writer: Option<ArrowWriter<std::fs::File>> = None;
    let mut total_rows = 0usize;

    for batch_result in reader {
        let batch = batch_result?;
        let num_rows = batch.num_rows();
        if num_rows == 0 {
            continue;
        }
        total_rows += num_rows;

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for (i, col) in batch.columns().iter().enumerate() {
            let should_prefix = prefix_cols.contains(&i);
            let conditional =
                conditional_prefix_cols.and_then(|c| c.iter().find(|(idx, _)| *idx == i));

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
            let id_col = columns[id_col_idx]
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
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

/// Read edge parquet, prefix specified columns (ID columns), pass others through.
/// `prefix_cols` lists column indices to prefix (e.g. &[0, 1] for src, tgt).
pub fn prefix_edge_parquet(
    input: &Path,
    output: &Path,
    prefix: &str,
    prefix_cols: &[usize],
) -> Result<usize> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;

    let file = match std::fs::File::open(input) {
        Ok(f) => f,
        Err(_) => return Ok(0),
    };

    let meta = file.metadata()?;
    if meta.len() == 0 {
        return Ok(0);
    }

    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

    let out_file = std::fs::File::create(output)?;
    let mut writer: Option<ArrowWriter<std::fs::File>> = None;
    let mut total = 0usize;

    for batch_result in reader {
        let batch = batch_result?;
        let n = batch.num_rows();
        if n == 0 {
            continue;
        }
        total += n;

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for (i, col) in batch.columns().iter().enumerate() {
            if prefix_cols.contains(&i) {
                let str_arr = col.as_any().downcast_ref::<StringArray>().unwrap();
                let prefixed: StringArray = (0..n)
                    .map(|j| Some(format!("{}{}", prefix, str_arr.value(j))))
                    .collect();
                columns.push(Arc::new(prefixed));
            } else {
                columns.push(col.clone());
            }
        }

        let new_batch = arrow::record_batch::RecordBatch::try_new(batch.schema(), columns)?;
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
    }

    Ok(total)
}

/// Run cross-repo call and inheritance resolution on the combined graph.
fn resolve_cross_repo(store: &GraphStore, contracts: &[super::Contract]) -> Result<usize> {
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);

    // Build symbol map: qualified_key → [(id, file, kind)]
    // Key by module_stem::name to avoid false matches on bare names like "Settings"
    let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let rows = gq.raw_query("MATCH (s:Symbol) RETURN s.name, s.id, s.file, s.kind")?;
    for row in &rows {
        if row.len() >= 4 {
            let qualified_key = qualified_symbol_key(&row[0], &row[2]);
            symbol_map.entry(qualified_key).or_default().push((
                row[1].clone(),
                row[2].clone(),
                row[3].clone(),
            ));
        }
    }

    let mut new_calls = 0;
    let mut new_inherits = 0;

    // Cross-repo INHERITS: types with same qualified key in multiple repos
    let mut name_to_ids: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (qkey, entries) in &symbol_map {
        let type_entries: Vec<_> = entries
            .iter()
            .filter(|(_, _, k)| {
                matches!(
                    k.as_str(),
                    "Class" | "Interface" | "Struct" | "Trait" | "Enum"
                )
            })
            .collect();
        if type_entries.len() < 2 {
            continue;
        }
        // Only group types from different repos
        let mut repos_seen: HashSet<&str> = HashSet::new();
        for (id, _, _) in &type_entries {
            repos_seen.insert(extract_repo(id));
        }
        if repos_seen.len() < 2 {
            continue;
        }
        for (id, _, _) in &type_entries {
            let repo = extract_repo(id);
            name_to_ids
                .entry(qkey.clone())
                .or_default()
                .push((id.clone(), repo.to_string()));
        }
    }

    // Batch: collect all inheritor→target pairs to create
    let mut inh_pairs: Vec<(String, String)> = Vec::new();
    for id_repos in name_to_ids.values() {
        if id_repos.len() < 2 {
            continue;
        }
        for (target_id, _) in id_repos {
            let escaped = target_id.replace('\'', "\\'");
            let inheritors = gq
                .raw_query(&format!(
                    "MATCH (s:Symbol)-[:INHERITS]->(t:Symbol {{id: '{}'}}) RETURN s.id",
                    escaped
                ))
                .unwrap_or_default();

            for row in &inheritors {
                if row.is_empty() {
                    continue;
                }
                let inh_repo = extract_repo(&row[0]);
                for (other_id, other_repo) in id_repos {
                    if other_repo == inh_repo || other_id == target_id {
                        continue;
                    }
                    inh_pairs.push((row[0].clone(), other_id.clone()));
                }
            }
        }
    }
    // Deduplicate and write
    let inh_pairs: Vec<(String, String)> = {
        let mut seen = HashSet::new();
        inh_pairs
            .into_iter()
            .filter(|p| seen.insert((p.0.clone(), p.1.clone())))
            .collect()
    };
    if !inh_pairs.is_empty() {
        let refs: Vec<(&str, &str)> = inh_pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let pq = std::env::temp_dir().join("ig_combined_cross_inherits.parquet");
        parquet_loader::write_edge_parquet(&pq, &refs)?;
        if let Err(e) = conn.query(&format!(
            "COPY INHERITS FROM '{}'",
            pq.to_string_lossy().replace('\\', "/")
        )) {
            eprintln!("  warn: COPY cross-repo INHERITS failed ({e}), using UNWIND");
            for chunk in refs.chunks(500) {
                let items: Vec<String> = chunk
                    .iter()
                    .map(|(a, b)| {
                        format!(
                            "{{a:'{}',b:'{}'}}",
                            a.replace('\'', "\\'"),
                            b.replace('\'', "\\'")
                        )
                    })
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
    // Use bare name (after "::") for method matching across files
    let mut file_methods: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for (qkey, entries) in &symbol_map {
        let bare_name = qkey.split("::").last().unwrap_or(qkey);
        for (id, file, kind) in entries {
            if kind == "Class"
                || kind == "Interface"
                || kind == "Struct"
                || kind == "Trait"
                || kind == "Enum"
                || kind == "Module"
            {
                continue;
            }
            file_methods
                .entry(file.clone())
                .or_default()
                .entry(bare_name.to_string())
                .or_default()
                .push(id.clone());
        }
    }

    let mut call_pairs: Vec<(String, String)> = Vec::new();
    let mut processed_pairs: HashSet<(String, String)> = HashSet::new();

    for (child_id, parent_id) in &inh_pairs {
        let child_file = match type_file_map.get(child_id) {
            Some(f) => f,
            None => continue,
        };
        let parent_file = match type_file_map.get(parent_id) {
            Some(f) => f,
            None => continue,
        };
        let child_repo = extract_repo(child_id);
        let parent_repo = extract_repo(parent_id);
        if child_repo == parent_repo {
            continue;
        }

        let child_methods = match file_methods.get(child_file) {
            Some(m) => m,
            None => continue,
        };
        let parent_methods = match file_methods.get(parent_file) {
            Some(m) => m,
            None => continue,
        };

        for (method_name, child_method_ids) in child_methods {
            let parent_method_ids = match parent_methods.get(method_name) {
                Some(ids) => ids,
                None => continue,
            };
            // For each child method, find its callers → link to parent methods
            for cmid in child_method_ids {
                let pair_key = (cmid.clone(), parent_file.clone());
                if !processed_pairs.insert(pair_key) {
                    continue;
                }
                let escaped = cmid.replace('\'', "\\'");
                let callers = gq
                    .raw_query(&format!(
                        "MATCH (c:Symbol)-[:CALLS]->(t:Symbol {{id: '{}'}}) RETURN c.id",
                        escaped
                    ))
                    .unwrap_or_default();
                for crow in &callers {
                    if crow.is_empty() {
                        continue;
                    }
                    for pid in parent_method_ids {
                        if extract_repo(&crow[0]) != parent_repo {
                            call_pairs.push((crow[0].clone(), pid.clone()));
                        }
                    }
                }
            }
        }
    }

    eprintln!(
        "  [combined] Type-directed: {} INHERITS pairs → {} raw CALLS candidates",
        inh_pairs.len(),
        call_pairs.len()
    );
    let call_pairs: Vec<(String, String)> = {
        let mut seen = HashSet::new();
        call_pairs
            .into_iter()
            .filter(|p| seen.insert((p.0.clone(), p.1.clone())))
            .collect()
    };
    if !call_pairs.is_empty() {
        let refs: Vec<(&str, &str)> = call_pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let pq = std::env::temp_dir().join("ig_combined_cross_calls.parquet");
        parquet_loader::write_edge_parquet(&pq, &refs)?;
        if let Err(e) = conn.query(&format!(
            "COPY CALLS FROM '{}'",
            pq.to_string_lossy().replace('\\', "/")
        )) {
            eprintln!("  warn: COPY cross-repo CALLS failed ({e}), using UNWIND");
            for chunk in refs.chunks(500) {
                let items: Vec<String> = chunk
                    .iter()
                    .map(|(a, b)| {
                        format!(
                            "{{a:'{}',b:'{}'}}",
                            a.replace('\'', "\\'"),
                            b.replace('\'', "\\'")
                        )
                    })
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

    // Phase 3: Resolve xsvc virtual nodes → real endpoint symbols via contracts.
    // For each CALLS_SERVICE edge caller→xsvc::svc::METHOD::path, look up the
    // contract (service, method, path) → symbol_id, then create a CALLS edge
    // from caller → [target_repo]::symbol_id (the real function).
    let mut xsvc_resolved = 0;
    if !contracts.is_empty() {
        use super::cross_service::normalize_route_path;
        use super::ContractKind;

        // Build contract lookup: (service, method, normalized_path) → symbol_id
        let mut contract_map: HashMap<(String, String, String), String> = HashMap::new();
        for c in contracts {
            match c.kind {
                ContractKind::HttpRoute => {
                    let norm = normalize_route_path(&c.path);
                    contract_map.insert(
                        (c.service.clone(), c.method.to_ascii_uppercase(), norm),
                        c.symbol_id.clone(),
                    );
                }
                // gRPC (AIF3X-331 #21b): the CALLS_SERVICE edge is SERVICE-level
                // (method "GRPC", path "/ServiceName"), while a GrpcService
                // contract is RPC-level (path "/ServiceName/Rpc"). Key the map at
                // service granularity ("/ServiceName") so the edge resolves; the
                // value is a concrete RPC symbol of that service, so the promoted
                // CALLS edge lands on a real node. first-RPC-wins is fine — any
                // RPC symbol anchors the caller→service link.
                ContractKind::GrpcService => {
                    let svc_path = {
                        let p = c.path.trim_start_matches('/');
                        match p.split_once('/') {
                            Some((svc, _rpc)) => format!("/{svc}"),
                            None => format!("/{p}"),
                        }
                    };
                    contract_map
                        .entry((c.service.clone(), "GRPC".to_string(), svc_path))
                        .or_insert_with(|| c.symbol_id.clone());
                }
                _ => {}
            }
        }

        // Query all CALLS_SERVICE edges from combined graph
        let cs_rows = gq
            .raw_query(
                "MATCH (a)-[r:CALLS_SERVICE]->(b) \
             WHERE b.kind = 'ExternalService' \
             RETURN a.id, r.target_service, r.method, r.path",
            )
            .unwrap_or_default();

        let mut xsvc_pairs: Vec<(String, String)> = Vec::new();
        let mut xsvc_seen: HashSet<(String, String)> = HashSet::new();

        for row in &cs_rows {
            if row.len() < 4 {
                continue;
            }
            let caller_id = &row[0];
            let target_svc = &row[1];
            let method = row[2].to_ascii_uppercase();
            let path = &row[3];
            let norm_path = normalize_route_path(path);

            let key = (target_svc.clone(), method.clone(), norm_path);
            if let Some(real_sym) = contract_map.get(&key) {
                let real_id = format!("[{}]::{}", target_svc, real_sym);
                // Verify target node exists in combined graph
                let pair = (caller_id.clone(), real_id);
                if xsvc_seen.insert(pair.clone()) {
                    xsvc_pairs.push(pair);
                }
            }
        }

        // Verify target symbols exist and batch-create CALLS edges
        let mut valid_pairs: Vec<(String, String)> = Vec::new();
        for (caller, target) in &xsvc_pairs {
            let escaped = target.replace('\'', "\\'");
            let exists = gq
                .raw_query(&format!(
                    "MATCH (s:Symbol {{id: '{}'}}) RETURN s.id",
                    escaped
                ))
                .unwrap_or_default();
            if !exists.is_empty() {
                valid_pairs.push((caller.clone(), target.clone()));
            }
        }

        if !valid_pairs.is_empty() {
            let refs: Vec<(&str, &str)> = valid_pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            let pq = std::env::temp_dir().join("ig_combined_xsvc_calls.parquet");
            parquet_loader::write_edge_parquet(&pq, &refs)?;
            if let Err(e) = conn.query(&format!(
                "COPY CALLS FROM '{}'",
                pq.to_string_lossy().replace('\\', "/")
            )) {
                eprintln!("  warn: COPY xsvc CALLS failed ({e}), using UNWIND");
                for chunk in refs.chunks(500) {
                    let items: Vec<String> = chunk
                        .iter()
                        .map(|(a, b)| {
                            format!(
                                "{{a:'{}',b:'{}'}}",
                                a.replace('\'', "\\'"),
                                b.replace('\'', "\\'")
                            )
                        })
                        .collect();
                    let _ = conn.query(&format!(
                        "UNWIND [{}] AS p MATCH (a:Symbol),(b:Symbol) WHERE a.id=p.a AND b.id=p.b CREATE (a)-[:CALLS]->(b)",
                        items.join(",")
                    ));
                }
            }
            let _ = std::fs::remove_file(&pq);
            xsvc_resolved = valid_pairs.len();
        }

        // Resolve SharedPackage xsvc nodes → real exported symbols.
        // For CALLS_SERVICE with method='package', find the imported symbol names
        // by looking at caller's file for import lines, then match to publisher's symbols.
        let pkg_rows = gq
            .raw_query(
                "MATCH (a)-[r:CALLS_SERVICE]->(b) \
             WHERE r.method = 'package' \
             RETURN a.id, a.file, r.target_service, r.path",
            )
            .unwrap_or_default();

        if !pkg_rows.is_empty() {
            // Build name→id index for all symbols in publisher repos
            let mut publisher_symbols: HashMap<String, HashMap<String, String>> = HashMap::new();
            for row in &rows {
                if row.len() >= 4 {
                    let id = &row[1];
                    let name = &row[0];
                    let repo = extract_repo(id);
                    if !repo.is_empty() {
                        publisher_symbols
                            .entry(repo.to_string())
                            .or_default()
                            .insert(name.clone(), id.clone());
                    }
                }
            }

            let mut pkg_pairs: Vec<(String, String)> = Vec::new();
            let mut pkg_seen: HashSet<(String, String)> = HashSet::new();

            for row in &pkg_rows {
                if row.len() < 4 {
                    continue;
                }
                let caller_id = &row[0];
                let caller_file = &row[1];
                let target_svc = &row[2];

                let pub_syms = match publisher_symbols.get(target_svc.as_str()) {
                    Some(s) => s,
                    None => continue,
                };

                // Find all symbols in the caller's file — they're the potential import sites
                let caller_repo = extract_repo(caller_id);
                let escaped_file = caller_file.replace('\'', "\\'");
                let file_syms = gq
                    .raw_query(&format!(
                        "MATCH (s:Symbol) WHERE s.file = '{}' RETURN s.id, s.name, s.kind",
                        escaped_file
                    ))
                    .unwrap_or_default();

                // For each symbol in the caller's file, check if same name exists
                // in publisher repo — if so, create a CALLS edge
                for fsym in &file_syms {
                    if fsym.len() < 3 {
                        continue;
                    }
                    let sym_name = &fsym[1];
                    let sym_kind = &fsym[2];
                    // Skip common names that would create false matches
                    if matches!(sym_kind.as_str(), "Variable" | "Constant")
                        && matches!(
                            sym_name.as_str(),
                            "logger" | "log" | "LOG" | "app" | "router" | "__all__"
                        )
                    {
                        continue;
                    }
                    let matched = pub_syms.get(sym_name).or_else(|| {
                        let camel = snake_to_camel(sym_name);
                        if camel != *sym_name {
                            pub_syms.get(&camel)
                        } else {
                            None
                        }
                    });
                    if let Some(target_id) = matched {
                        if extract_repo(target_id) != caller_repo {
                            let pair = (fsym[0].clone(), target_id.clone());
                            if pkg_seen.insert(pair.clone()) {
                                pkg_pairs.push(pair);
                            }
                        }
                    }
                }
            }

            if !pkg_pairs.is_empty() {
                let refs: Vec<(&str, &str)> = pkg_pairs
                    .iter()
                    .map(|(a, b)| (a.as_str(), b.as_str()))
                    .collect();
                let pq = std::env::temp_dir().join("ig_combined_pkg_calls.parquet");
                parquet_loader::write_edge_parquet(&pq, &refs)?;
                if let Err(e) = conn.query(&format!(
                    "COPY CALLS FROM '{}'",
                    pq.to_string_lossy().replace('\\', "/")
                )) {
                    eprintln!("  warn: COPY pkg CALLS failed ({e}), using UNWIND");
                    for chunk in refs.chunks(500) {
                        let items: Vec<String> = chunk
                            .iter()
                            .map(|(a, b)| {
                                format!(
                                    "{{a:'{}',b:'{}'}}",
                                    a.replace('\'', "\\'"),
                                    b.replace('\'', "\\'")
                                )
                            })
                            .collect();
                        let _ = conn.query(&format!(
                            "UNWIND [{}] AS p MATCH (a:Symbol),(b:Symbol) WHERE a.id=p.a AND b.id=p.b CREATE (a)-[:CALLS]->(b)",
                            items.join(",")
                        ));
                    }
                }
                let _ = std::fs::remove_file(&pq);
                xsvc_resolved += pkg_pairs.len();
                eprintln!(
                    "  [combined] pkg→symbol: {} cross-repo import edges",
                    pkg_pairs.len()
                );
            }
        }

        eprintln!(
            "  [combined] xsvc→real: {} total resolved ({} candidates, {} contracts)",
            xsvc_resolved,
            cs_rows.len(),
            contract_map.len()
        );
    }

    eprintln!(
        "  [combined] Cross-repo: {} CALLS, {} INHERITS, {} xsvc→real",
        new_calls, new_inherits, xsvc_resolved
    );

    Ok(new_calls + new_inherits + xsvc_resolved)
}

/// Open the combined graph store for a group.
pub fn open_combined_graph(group_name: &str) -> Result<GraphStore> {
    let path = combined_graph_path(group_name)?;
    if !path.exists() {
        anyhow::bail!(
            "Combined graph not found for group '{}'. Run 'infigraph group combined {}' first.",
            group_name,
            group_name
        );
    }
    match GraphStore::open(&path) {
        Ok(store) => Ok(store),
        Err(first_err) => {
            eprintln!(
                "[combined-graph] open failed for group '{group_name}' ({first_err}), \
                 wiping corrupt graph and scheduling rebuild..."
            );
            wipe_combined_graph(group_name);
            Err(first_err.context(format!(
                "combined graph corrupt for group '{group_name}'; wiped, rebuild needed"
            )))
        }
    }
}

fn wipe_combined_graph(group_name: &str) {
    if let Ok(path) = combined_graph_path(group_name) {
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_file(&path);
        let wal = path.with_extension("wal");
        let _ = std::fs::remove_file(&wal);
    }
}

/// Query the combined graph with a single Cypher query.
pub fn combined_query(group_name: &str, cypher: &str) -> Result<Vec<Vec<String>>> {
    let store = open_combined_graph(group_name)?;
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);
    gq.raw_query(cypher)
}

pub fn combined_graph_path(group_name: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
        .context("cannot determine home directory")?;
    Ok(home
        .join(".infigraph")
        .join("groups")
        .join(group_name)
        .join(".infigraph")
        .join("graph"))
}

pub fn has_combined_graph(group_name: &str) -> bool {
    combined_graph_path(group_name)
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn strip_prefix(id: &str) -> &str {
    if let Some(idx) = id.find("]::") {
        &id[idx + 3..]
    } else {
        id
    }
}

/// Build a qualified key from symbol name and file path.
/// Strips the repo prefix from the file, extracts the module stem,
/// and produces "module_stem::name". This prevents false cross-repo
/// matches on common names like "Settings" or "Config" that appear
/// in unrelated modules across repos.
fn qualified_symbol_key(name: &str, file: &str) -> String {
    // Strip repo prefix: "[repo-name]::path/to/file.py" → "path/to/file.py"
    let file_path = if let Some(idx) = file.find("]::") {
        &file[idx + 3..]
    } else {
        file
    };
    // Extract module stem: "path/to/settings.py" → "settings"
    let stem = std::path::Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    format!("{}::{}", stem, name)
}

fn category_weight(cat: &str) -> f32 {
    match cat {
        "impl" => 1.0,
        "test" => 0.5,
        "config" => 0.3,
        "docs" => 0.5,
        _ => 1.0,
    }
}

fn snake_to_camel(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

pub fn extract_repo(id: &str) -> &str {
    if id.starts_with('[') {
        if let Some(idx) = id.find("]::") {
            return &id[1..idx];
        }
    }
    ""
}

/// Hybrid BM25+vector search over the combined graph.
/// Builds/caches BM25 index and embeddings at the combined graph's directory.
pub fn combined_search(
    group_name: &str,
    query: &str,
    limit: usize,
    alpha: f32,
) -> Result<Vec<crate::search::SearchResult>> {
    use crate::embed;
    use crate::search::BM25Index;

    let store = open_combined_graph(group_name)?;
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.category",
    )?;

    if rows.is_empty() {
        return Ok(vec![]);
    }

    let docs: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let name = &row[1];
            let kind = &row[2];
            let file = row.get(3).map(|s| s.as_str()).unwrap_or("");
            let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
            let file_context = crate::embed::path_to_context(strip_prefix(file));
            let text = if doc.is_empty() {
                format!("{} {} in {}", kind, name, file_context)
            } else {
                format!("{} {} in {}: {}", kind, name, file_context, doc)
            };
            (id, text)
        })
        .collect();

    let base = combined_graph_path(group_name)?
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let emb_path = base.join("embeddings.bin");
    let bm25_cache_path = base.join("bm25_cache.bin");

    let emb_mtime = std::fs::metadata(&emb_path).and_then(|m| m.modified()).ok();
    let cache_mtime = std::fs::metadata(&bm25_cache_path)
        .and_then(|m| m.modified())
        .ok();
    let cache_fresh = match (emb_mtime, cache_mtime) {
        (Some(e), Some(c)) => c >= e,
        _ => false,
    };
    let bm25_index = if cache_fresh {
        BM25Index::load(&bm25_cache_path).unwrap_or_else(|_| {
            let idx = BM25Index::build(docs.clone());
            let _ = idx.save(&bm25_cache_path);
            idx
        })
    } else {
        let idx = BM25Index::build(docs.clone());
        let _ = idx.save(&bm25_cache_path);
        idx
    };

    let embedder = embed::best_embedder();

    let symbol_embeddings: Vec<(String, Vec<f32>)> = if emb_path.exists() {
        embed::load_embeddings_cached(&emb_path)?
    } else {
        eprintln!(
            "Computing embeddings for {} symbols in combined graph...",
            docs.len()
        );
        let texts: Vec<&str> = docs.iter().map(|(_, t)| t.as_str()).collect();
        let vecs = embedder.embed_batch(&texts)?;
        let embs: Vec<(String, Vec<f32>)> = docs
            .iter()
            .zip(vecs)
            .map(|((id, _), v)| (id.clone(), v))
            .collect();
        embed::save_embeddings(&emb_path, &embs)?;
        embs
    };

    let hnsw_path = base.join("hnsw_index.usearch");
    let fetch_limit = (limit * 5).max(100);

    let mut results = crate::search::hybrid_search(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        fetch_limit,
        alpha,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    // De-prioritize non-implementation symbols using persisted category.
    // Build (id → category) and (id → file) maps. If category is missing/default,
    // fall back to classifying from file path (handles pre-migration graphs).
    let cat_map: HashMap<&str, (&str, &str)> = rows
        .iter()
        .filter_map(|row| {
            if row.len() >= 6 {
                Some((row[0].as_str(), (row[5].as_str(), row[3].as_str())))
            } else if row.len() >= 4 {
                Some((row[0].as_str(), ("", row[3].as_str())))
            } else {
                None
            }
        })
        .collect();
    for r in &mut results {
        let (cat, file) = cat_map
            .get(r.symbol_id.as_str())
            .copied()
            .unwrap_or(("impl", ""));
        let effective_cat = if cat.is_empty() || cat == "impl" {
            crate::graph::store_util::classify_file(file)
        } else {
            cat
        };
        let penalty = category_weight(effective_cat);
        if penalty < 1.0 {
            r.score *= penalty;
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Graph-boosted re-ranking + cross-repo injection.
    // 1-hop walk from top results: boost in-pool neighbors, inject cross-repo
    // neighbors not in pool (impl-category only) at 70% of triggering score.
    if results.len() > 1 {
        let top_n = limit.min(results.len()).min(15);
        let result_set: HashSet<String> = results.iter().map(|r| r.symbol_id.clone()).collect();
        let top_ids: Vec<(String, f32)> = results[..top_n]
            .iter()
            .map(|r| (r.symbol_id.clone(), r.score))
            .collect();
        let top_set: HashSet<&str> = top_ids.iter().map(|(s, _)| s.as_str()).collect();

        let mut neighbor_boost: HashMap<String, f32> = HashMap::new();
        let mut injections: Vec<crate::search::SearchResult> = Vec::new();
        let mut injected_ids: HashSet<String> = HashSet::new();

        for (top_id, top_score) in &top_ids {
            let top_repo = extract_repo(top_id);
            let escaped = top_id.replace('\'', "\\'");

            let out = gq
                .raw_query(&format!(
                    "MATCH (a:Symbol {{id: '{}'}})-[:CALLS|CALLS_SERVICE]->(b:Symbol) \
                     WHERE b.kind <> 'ExternalService' \
                     RETURN b.id, b.name, b.kind, b.file",
                    escaped
                ))
                .unwrap_or_default();
            for row in &out {
                if row.len() < 4 {
                    continue;
                }
                let nid = &row[0];
                if top_set.contains(nid.as_str()) {
                    continue;
                }
                if result_set.contains(nid) {
                    let e = neighbor_boost.entry(nid.clone()).or_insert(0.0);
                    *e += 0.15;
                } else if extract_repo(nid) != top_repo && !injected_ids.contains(nid) {
                    let file = &row[3];
                    let cat_from_map = cat_map
                        .get(nid.as_str())
                        .map(|(c, f)| {
                            if c.is_empty() || *c == "impl" {
                                crate::graph::store_util::classify_file(f)
                            } else {
                                *c
                            }
                        })
                        .unwrap_or_else(|| crate::graph::store_util::classify_file(file));
                    if cat_from_map == "impl" {
                        injected_ids.insert(nid.clone());
                        injections.push(crate::search::SearchResult {
                            symbol_id: nid.clone(),
                            name: row[1].clone(),
                            kind: row[2].clone(),
                            file: row[3].clone(),
                            score: top_score * 0.70,
                            bm25_score: 0.0,
                            vector_score: 0.0,
                            docstring: None,
                        });
                    }
                }
            }

            let inc = gq
                .raw_query(&format!(
                    "MATCH (a:Symbol)-[:CALLS|CALLS_SERVICE]->(b:Symbol {{id: '{}'}}) \
                     WHERE a.kind <> 'ExternalService' \
                     RETURN a.id, a.name, a.kind, a.file",
                    escaped
                ))
                .unwrap_or_default();
            for row in &inc {
                if row.len() < 4 {
                    continue;
                }
                let nid = &row[0];
                if top_set.contains(nid.as_str()) {
                    continue;
                }
                let nid_repo = extract_repo(nid);
                if result_set.contains(nid) {
                    let e = neighbor_boost.entry(nid.clone()).or_insert(0.0);
                    *e += 0.10;
                } else if nid_repo != top_repo && !injected_ids.contains(nid) {
                    let file = &row[3];
                    let cat_from_map = cat_map
                        .get(nid.as_str())
                        .map(|(c, f)| {
                            if c.is_empty() || *c == "impl" {
                                crate::graph::store_util::classify_file(f)
                            } else {
                                *c
                            }
                        })
                        .unwrap_or_else(|| crate::graph::store_util::classify_file(file));
                    if cat_from_map == "impl" {
                        injected_ids.insert(nid.clone());
                        injections.push(crate::search::SearchResult {
                            symbol_id: nid.clone(),
                            name: row[1].clone(),
                            kind: row[2].clone(),
                            file: row[3].clone(),
                            score: top_score * 0.70,
                            bm25_score: 0.0,
                            vector_score: 0.0,
                            docstring: None,
                        });
                    }
                }
                // 2-hop: same-repo caller not in results → follow its cross-repo CALLS
                if nid_repo == top_repo
                    && !result_set.contains(nid)
                    && !top_set.contains(nid.as_str())
                {
                    let esc2 = nid.replace('\'', "\\'");
                    let hop2 = gq
                        .raw_query(&format!(
                            "MATCH (a:Symbol {{id: '{}'}})-[:CALLS]->(b:Symbol) \
                             WHERE b.kind <> 'ExternalService' \
                             RETURN b.id, b.name, b.kind, b.file",
                            esc2
                        ))
                        .unwrap_or_default();
                    for r2 in &hop2 {
                        if r2.len() < 4 {
                            continue;
                        }
                        let tid = &r2[0];
                        if extract_repo(tid) == nid_repo
                            || injected_ids.contains(tid)
                            || result_set.contains(tid)
                        {
                            continue;
                        }
                        let cat2 = cat_map
                            .get(tid.as_str())
                            .map(|(c, f)| {
                                if c.is_empty() || *c == "impl" {
                                    crate::graph::store_util::classify_file(f)
                                } else {
                                    *c
                                }
                            })
                            .unwrap_or_else(|| crate::graph::store_util::classify_file(&r2[3]));
                        if cat2 == "impl" {
                            injected_ids.insert(tid.clone());
                            injections.push(crate::search::SearchResult {
                                symbol_id: tid.clone(),
                                name: r2[1].clone(),
                                kind: r2[2].clone(),
                                file: r2[3].clone(),
                                score: top_score * 0.55,
                                bm25_score: 0.0,
                                vector_score: 0.0,
                                docstring: None,
                            });
                        }
                    }
                }
            }
        }

        if !neighbor_boost.is_empty() {
            for r in &mut results {
                if let Some(boost) = neighbor_boost.get(&r.symbol_id) {
                    r.score += boost;
                }
            }
        }
        results.extend(injections);
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Filter out ExternalService stubs — return real code only
    results.retain(|r| !r.symbol_id.contains("::xsvc::"));

    // Repo-diversity: promote best result from each repo not represented
    // in the top-N, evicting lowest-scored duplicates from over-represented repos.
    if results.len() > limit {
        let top_n = limit.min(results.len());
        let mut top_repos: HashMap<String, usize> = HashMap::new();
        for r in &results[..top_n] {
            *top_repos
                .entry(extract_repo(&r.symbol_id).to_string())
                .or_insert(0) += 1;
        }
        let mut promotions: Vec<usize> = Vec::new();
        let max_promote = (limit / 4).max(1);
        let mut seen_repos: HashSet<String> = HashSet::new();
        for (i, r) in results.iter().enumerate().skip(top_n) {
            let repo = extract_repo(&r.symbol_id).to_string();
            if !top_repos.contains_key(&repo) && seen_repos.insert(repo) {
                promotions.push(i);
                if promotions.len() >= max_promote {
                    break;
                }
            }
        }
        if !promotions.is_empty() {
            let promote_set: HashSet<usize> = promotions.iter().cloned().collect();
            let mut evict_set: HashSet<usize> = HashSet::new();
            let mut sorted_counts: Vec<(String, usize)> = top_repos.into_iter().collect();
            sorted_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
            'evict: for (repo, count) in &sorted_counts {
                if *count <= 1 {
                    break;
                }
                for i in (0..top_n).rev() {
                    if extract_repo(&results[i].symbol_id) == repo && !evict_set.contains(&i) {
                        evict_set.insert(i);
                        if evict_set.len() >= promotions.len() {
                            break 'evict;
                        }
                    }
                }
            }
            let mut selected: Vec<crate::search::SearchResult> = Vec::with_capacity(limit);
            for (i, r) in results.iter().enumerate() {
                if evict_set.contains(&i) {
                    continue;
                }
                if i >= top_n && !promote_set.contains(&i) {
                    continue;
                }
                selected.push(r.clone());
                if selected.len() >= limit {
                    break;
                }
            }
            selected.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            results = selected;
        }
    }
    results.truncate(limit);
    Ok(results)
}

/// Deep search: run combined_search then enrich top results with graph context
/// (cross-repo CALLS/CALLS_SERVICE neighbors). Returns formatted string for LLM consumption.
pub fn combined_search_deep(
    group_name: &str,
    query: &str,
    limit: usize,
    alpha: f32,
) -> Result<String> {
    let results = combined_search(group_name, query, limit, alpha)?;

    if results.is_empty() {
        return Ok(format!(
            "No results for '{}' in group '{}'",
            query, group_name
        ));
    }

    let store = open_combined_graph(group_name)?;
    let conn = store.connection()?;
    let gq = GraphQuery::new(&conn);

    let mut out = format!(
        "Deep search for '{}' in group '{}' ({} hits):\n\n",
        query,
        group_name,
        results.len()
    );

    // Show results with cross-repo graph context
    let top_n = limit.min(results.len()).min(15);
    for (i, r) in results[..top_n].iter().enumerate() {
        let repo = extract_repo(&r.symbol_id);
        out.push_str(&format!(
            "{}. [{:.3}] [{}] {}\n",
            i + 1,
            r.score,
            repo,
            r.symbol_id
        ));
        if let Some(doc) = &r.docstring {
            if !doc.is_empty() {
                let preview: String = doc.chars().take(120).collect();
                out.push_str(&format!("   {}\n", preview));
            }
        }

        // Cross-repo edges from this symbol
        let escaped = r.symbol_id.replace('\'', "\\'");
        let caller_repo = repo;

        // Outgoing CALLS to other repos
        let out_calls = gq
            .raw_query(&format!(
                "MATCH (a:Symbol {{id: '{}'}})-[:CALLS]->(b:Symbol) \
             WHERE b.kind <> 'ExternalService' \
             RETURN b.id, b.kind, b.file LIMIT 10",
                escaped
            ))
            .unwrap_or_default();
        let cross_out: Vec<_> = out_calls
            .iter()
            .filter(|row| row.len() >= 3 && extract_repo(&row[0]) != caller_repo)
            .collect();
        if !cross_out.is_empty() {
            out.push_str("   → calls across repos:\n");
            for row in &cross_out {
                out.push_str(&format!(
                    "     → [{}] {} ({})\n",
                    extract_repo(&row[0]),
                    row[0],
                    row[1]
                ));
            }
        }

        // Incoming CALLS from other repos
        let in_calls = gq
            .raw_query(&format!(
                "MATCH (a:Symbol)-[:CALLS]->(b:Symbol {{id: '{}'}}) \
             WHERE a.kind <> 'ExternalService' \
             RETURN a.id, a.kind, a.file LIMIT 10",
                escaped
            ))
            .unwrap_or_default();
        let cross_in: Vec<_> = in_calls
            .iter()
            .filter(|row| row.len() >= 3 && extract_repo(&row[0]) != caller_repo)
            .collect();
        if !cross_in.is_empty() {
            out.push_str("   ← called by across repos:\n");
            for row in &cross_in {
                out.push_str(&format!(
                    "     ← [{}] {} ({})\n",
                    extract_repo(&row[0]),
                    row[0],
                    row[1]
                ));
            }
        }

        // CALLS_SERVICE edges (to external services)
        let svc_calls = gq
            .raw_query(&format!(
                "MATCH (a:Symbol {{id: '{}'}})-[r:CALLS_SERVICE]->(b:Symbol) \
             RETURN r.target_service, r.method, r.path LIMIT 10",
                escaped
            ))
            .unwrap_or_default();
        if !svc_calls.is_empty() {
            out.push_str("   → calls service:\n");
            for row in &svc_calls {
                if row.len() >= 3 {
                    out.push_str(&format!("     → {} {} {}\n", row[0], row[1], row[2]));
                }
            }
        }

        out.push('\n');
    }

    // Remaining results (no graph context)
    if results.len() > top_n {
        out.push_str(&format!("--- {} more results ---\n", results.len() - top_n));
        for r in &results[top_n..] {
            let repo = extract_repo(&r.symbol_id);
            out.push_str(&format!("  [{:.3}] [{}] {}\n", r.score, repo, r.symbol_id));
        }
    }

    Ok(out)
}
