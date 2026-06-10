use std::sync::Arc;

use anyhow::Result;
use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::DataType;
use kuzu::Connection;

use super::parquet_loader;
use super::store::GraphStore;
use super::store_util::{escape, fwd_slash_path, unwind_edges_from_pairs};
use crate::model::{FileExtraction, RelationKind};

impl GraphStore {
    /// Create Folder nodes and edges for a set of file paths in bulk.
    /// More efficient than per-file upsert_folder_hierarchy calls.
    pub fn upsert_folders_bulk(&self, file_paths: &[&str]) -> Result<()> {
        let conn = self.connection()?;
        self.upsert_folders_bulk_conn(&conn, file_paths)
    }

    pub fn upsert_folders_bulk_conn(
        &self,
        conn: &Connection<'_>,
        file_paths: &[&str],
    ) -> Result<()> {
        let mut all_folders: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for file_path in file_paths {
            let parts: Vec<&str> = file_path.rsplitn(2, '/').collect();
            if parts.len() < 2 {
                continue;
            }
            let dir_path = parts[1];
            let segments: Vec<&str> = dir_path.split('/').collect();
            for i in 0..segments.len() {
                all_folders.insert(segments[..=i].join("/"));
            }
        }

        if all_folders.is_empty() {
            return Ok(());
        }

        // Write Folder nodes to parquet
        let folder_pq = std::env::temp_dir().join("infigraph_folders.parquet");
        {
            let ids: Vec<&str> = all_folders.iter().map(|s| s.as_str()).collect();
            let names: Vec<&str> = all_folders
                .iter()
                .map(|fp| fp.rsplit_once('/').map(|(_, n)| n).unwrap_or(fp.as_str()))
                .collect();
            let paths: Vec<&str> = all_folders.iter().map(|s| s.as_str()).collect();
            parquet_loader::write_node_parquet(
                &folder_pq,
                &[
                    ("id", DataType::Utf8),
                    ("name", DataType::Utf8),
                    ("path", DataType::Utf8),
                ],
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(names)),
                    Arc::new(StringArray::from(paths)),
                ],
            )?;
        }

        // Collect edge pairs in memory
        let cf_pairs: Vec<(String, String)> = all_folders
            .iter()
            .filter_map(|child| {
                child
                    .rsplit_once('/')
                    .map(|(p, _)| p)
                    .and_then(|parent_path| {
                        if all_folders.contains(parent_path) {
                            Some((parent_path.to_string(), child.clone()))
                        } else {
                            None
                        }
                    })
            })
            .collect();

        let cfile_pairs: Vec<(String, String)> = file_paths
            .iter()
            .filter_map(|fp| {
                let parts: Vec<&str> = fp.rsplitn(2, '/').collect();
                if parts.len() < 2 {
                    return None;
                }
                Some((parts[1].to_string(), fp.to_string()))
            })
            .collect();

        let copy_ok = conn
            .query(&format!(
                "COPY Folder FROM '{}'",
                fwd_slash_path(&folder_pq)
            ))
            .is_ok();

        if copy_ok {
            // Write edge parquet files and COPY FROM
            let cf_pq = std::env::temp_dir().join("infigraph_contains_folder.parquet");
            let cf_refs: Vec<(&str, &str)> = cf_pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            parquet_loader::write_edge_parquet(&cf_pq, &cf_refs)?;
            if let Err(e) = conn.query(&format!(
                "COPY CONTAINS_FOLDER FROM '{}'",
                fwd_slash_path(&cf_pq)
            )) {
                eprintln!("warn: COPY CONTAINS_FOLDER failed ({e}), using UNWIND fallback");
                unwind_edges_from_pairs(conn, &cf_refs, "CONTAINS_FOLDER", "Folder", "Folder");
            }
            let _ = std::fs::remove_file(&cf_pq);

            let cfile_pq = std::env::temp_dir().join("infigraph_contains_file.parquet");
            let cfile_refs: Vec<(&str, &str)> = cfile_pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            parquet_loader::write_edge_parquet(&cfile_pq, &cfile_refs)?;
            if let Err(e) = conn.query(&format!(
                "COPY CONTAINS_FILE FROM '{}'",
                fwd_slash_path(&cfile_pq)
            )) {
                eprintln!("warn: COPY CONTAINS_FILE failed ({e}), using UNWIND fallback");
                unwind_edges_from_pairs(conn, &cfile_refs, "CONTAINS_FILE", "Folder", "File");
            }
            let _ = std::fs::remove_file(&cfile_pq);
        } else {
            // Incremental path: some folders may already exist. Use UNWIND with MERGE semantics.
            const CHUNK: usize = 500;
            for chunk in all_folders.iter().collect::<Vec<_>>().chunks(CHUNK) {
                let items: Vec<String> = chunk
                    .iter()
                    .map(|fp| {
                        let name = fp.rsplit_once('/').map(|(_, n)| n).unwrap_or(fp);
                        format!(
                            "{{id: '{}', name: '{}', path: '{}'}}",
                            escape(fp),
                            escape(name),
                            escape(fp)
                        )
                    })
                    .collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS f MERGE (d:Folder {{id: f.id}}) ON CREATE SET d.name = f.name, d.path = f.path ON MATCH SET d.name = f.name, d.path = f.path",
                    items.join(", ")
                ));
            }
            let cf_refs: Vec<(&str, &str)> = cf_pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            unwind_edges_from_pairs(conn, &cf_refs, "CONTAINS_FOLDER", "Folder", "Folder");
            let cfile_refs: Vec<(&str, &str)> = cfile_pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            unwind_edges_from_pairs(conn, &cfile_refs, "CONTAINS_FILE", "Folder", "File");
        }

        let _ = std::fs::remove_file(&folder_pq);
        Ok(())
    }

    /// Bulk write all extractions using COPY FROM Parquet -- binary format eliminates escaping issues.
    /// Used for --full index. Incremental index still uses upsert_file_conn_no_delete.
    pub fn upsert_all_parquet(&self, extractions: &[FileExtraction]) -> Result<()> {
        if extractions.is_empty() {
            return Ok(());
        }

        let conn = self.connection()?;
        let tmp = std::env::temp_dir();

        let mut known_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in extractions {
            for sym in &e.symbols {
                known_ids.insert(sym.id.clone());
            }
        }
        let mut sym_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let known_module_ids: std::collections::HashSet<String> =
            extractions.iter().map(|e| e.file.clone()).collect();

        // Collect all data into vecs
        let mut mod_ids = Vec::new();
        let mut mod_names = Vec::new();
        let mut mod_files = Vec::new();
        let mut mod_langs = Vec::new();
        let mut mod_hashes = Vec::new();
        let mut mod_summaries = Vec::new();
        let mut file_ids = Vec::new();
        let mut file_names = Vec::new();
        let mut file_paths = Vec::new();
        let mut file_langs = Vec::new();
        let mut file_symcounts: Vec<i64> = Vec::new();
        let mut sym_ids = Vec::new();
        let mut sym_names = Vec::new();
        let mut sym_kinds = Vec::new();
        let mut sym_files = Vec::new();
        let mut sym_slines: Vec<i64> = Vec::new();
        let mut sym_elines: Vec<i64> = Vec::new();
        let mut sym_sighashes = Vec::new();
        let mut sym_languages = Vec::new();
        let mut sym_visibilities = Vec::new();
        let mut sym_parents = Vec::new();
        let mut sym_docstrings = Vec::new();
        let mut sym_complexities: Vec<i64> = Vec::new();
        let mut sym_parameters = Vec::new();
        let mut sym_return_types = Vec::new();
        let mut contains_pairs: Vec<(String, String)> = Vec::new();
        let mut defines_pairs: Vec<(String, String)> = Vec::new();

        let mut calls_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut inh_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut test_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut imp_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut reads_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut writes_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut calls_pairs: Vec<(String, String)> = Vec::new();
        let mut inh_pairs: Vec<(String, String)> = Vec::new();
        let mut test_pairs: Vec<(String, String)> = Vec::new();
        let mut imp_pairs: Vec<(String, String)> = Vec::new();
        let mut reads_pairs: Vec<(String, String)> = Vec::new();
        let mut writes_pairs: Vec<(String, String)> = Vec::new();
        let mut custom_seen: std::collections::HashMap<String, std::collections::HashSet<(String, String)>> =
            std::collections::HashMap::new();
        let mut custom_pairs: std::collections::HashMap<String, Vec<(String, String)>> =
            std::collections::HashMap::new();

        for e in extractions {
            let mod_name = e.file.rsplit_once('/').map(|(_, f)| f).unwrap_or(&e.file);
            mod_ids.push(e.file.clone());
            mod_names.push(mod_name.to_string());
            mod_files.push(e.file.clone());
            mod_langs.push(e.language.clone());
            mod_hashes.push(e.content_hash.clone());
            mod_summaries.push(String::new());

            file_ids.push(e.file.clone());
            file_names.push(mod_name.to_string());
            file_paths.push(e.file.clone());
            file_langs.push(e.language.clone());
            file_symcounts.push(e.symbols.len() as i64);

            for sym in &e.symbols {
                if sym_seen.insert(sym.id.clone()) {
                    sym_ids.push(sym.id.clone());
                    sym_names.push(sym.name.clone());
                    sym_kinds.push(sym.kind.as_str().to_string());
                    sym_files.push(e.file.clone());
                    sym_slines.push(sym.span.start_line as i64);
                    sym_elines.push(sym.span.end_line as i64);
                    sym_sighashes.push(sym.signature_hash.clone());
                    sym_languages.push(sym.language.clone());
                    sym_visibilities.push(sym.visibility.as_deref().unwrap_or("").to_string());
                    sym_parents.push(sym.parent.as_deref().unwrap_or("").to_string());
                    sym_docstrings.push(sym.docstring.as_deref().unwrap_or("").to_string());
                    sym_complexities.push(sym.complexity as i64);
                    sym_parameters.push(sym.parameters.as_deref().unwrap_or("").to_string());
                    sym_return_types.push(sym.return_type.as_deref().unwrap_or("").to_string());
                    contains_pairs.push((e.file.clone(), sym.id.clone()));
                    defines_pairs.push((e.file.clone(), sym.id.clone()));
                }
            }

            for rel in &e.relations {
                let src = rel.source_id.clone();
                let tgt = rel.target_id.clone();
                match &rel.kind {
                    RelationKind::Imports | RelationKind::ImportedBy => {
                        if known_module_ids.contains(&src)
                            && known_module_ids.contains(&tgt)
                            && imp_seen.insert((src.clone(), tgt.clone()))
                        {
                            imp_pairs.push((src, tgt));
                        }
                    }
                    RelationKind::Custom(name) => {
                        if known_ids.contains(&src) && known_ids.contains(&tgt)
                            && custom_seen.entry(name.clone()).or_default().insert((src.clone(), tgt.clone()))
                        {
                            custom_pairs.entry(name.clone()).or_default().push((src, tgt));
                        }
                    }
                    _ => {
                        if !known_ids.contains(&src) || !known_ids.contains(&tgt) {
                            continue;
                        }
                        match &rel.kind {
                            RelationKind::Calls | RelationKind::CalledBy
                                if calls_seen.insert((src.clone(), tgt.clone())) =>
                            {
                                calls_pairs.push((src, tgt));
                            }
                            RelationKind::Inherits | RelationKind::InheritedBy
                                if inh_seen.insert((src.clone(), tgt.clone())) =>
                            {
                                inh_pairs.push((src, tgt));
                            }
                            RelationKind::TestedBy | RelationKind::Tests
                                if test_seen.insert((src.clone(), tgt.clone())) =>
                            {
                                test_pairs.push((src, tgt));
                            }
                            RelationKind::Reads
                                if reads_seen.insert((src.clone(), tgt.clone())) =>
                            {
                                reads_pairs.push((src, tgt));
                            }
                            RelationKind::Writes
                                if writes_seen.insert((src.clone(), tgt.clone())) =>
                            {
                                writes_pairs.push((src, tgt));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Write node parquet files
        let mod_pq = tmp.join("infigraph_index_modules.parquet");
        parquet_loader::write_node_parquet(
            &mod_pq,
            &[
                ("id", DataType::Utf8),
                ("name", DataType::Utf8),
                ("file", DataType::Utf8),
                ("language", DataType::Utf8),
                ("content_hash", DataType::Utf8),
                ("summary", DataType::Utf8),
            ],
            vec![
                Arc::new(StringArray::from(mod_ids)),
                Arc::new(StringArray::from(mod_names)),
                Arc::new(StringArray::from(mod_files)),
                Arc::new(StringArray::from(mod_langs)),
                Arc::new(StringArray::from(mod_hashes)),
                Arc::new(StringArray::from(mod_summaries)),
            ],
        )?;

        let file_pq = tmp.join("infigraph_index_files.parquet");
        parquet_loader::write_node_parquet(
            &file_pq,
            &[
                ("id", DataType::Utf8),
                ("name", DataType::Utf8),
                ("path", DataType::Utf8),
                ("language", DataType::Utf8),
                ("symbol_count", DataType::Int64),
            ],
            vec![
                Arc::new(StringArray::from(file_ids)),
                Arc::new(StringArray::from(file_names)),
                Arc::new(StringArray::from(file_paths)),
                Arc::new(StringArray::from(file_langs)),
                Arc::new(Int64Array::from(file_symcounts)),
            ],
        )?;

        let sym_pq = tmp.join("infigraph_index_symbols.parquet");
        parquet_loader::write_node_parquet(
            &sym_pq,
            &[
                ("id", DataType::Utf8),
                ("name", DataType::Utf8),
                ("kind", DataType::Utf8),
                ("file", DataType::Utf8),
                ("start_line", DataType::Int64),
                ("end_line", DataType::Int64),
                ("signature_hash", DataType::Utf8),
                ("language", DataType::Utf8),
                ("visibility", DataType::Utf8),
                ("parent", DataType::Utf8),
                ("docstring", DataType::Utf8),
                ("complexity", DataType::Int64),
                ("parameters", DataType::Utf8),
                ("return_type", DataType::Utf8),
            ],
            vec![
                Arc::new(StringArray::from(sym_ids)),
                Arc::new(StringArray::from(sym_names)),
                Arc::new(StringArray::from(sym_kinds)),
                Arc::new(StringArray::from(sym_files)),
                Arc::new(Int64Array::from(sym_slines)),
                Arc::new(Int64Array::from(sym_elines)),
                Arc::new(StringArray::from(sym_sighashes)),
                Arc::new(StringArray::from(sym_languages)),
                Arc::new(StringArray::from(sym_visibilities)),
                Arc::new(StringArray::from(sym_parents)),
                Arc::new(StringArray::from(sym_docstrings)),
                Arc::new(Int64Array::from(sym_complexities)),
                Arc::new(StringArray::from(sym_parameters)),
                Arc::new(StringArray::from(sym_return_types)),
            ],
        )?;

        // COPY FROM parquet -- node tables first
        conn.query(&format!("COPY Module FROM '{}'", fwd_slash_path(&mod_pq)))
            .map_err(|e| anyhow::anyhow!("COPY Module failed: {e}"))?;
        conn.query(&format!("COPY File FROM '{}'", fwd_slash_path(&file_pq)))
            .map_err(|e| anyhow::anyhow!("COPY File failed: {e}"))?;
        conn.query(&format!(
            "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, parameters, return_type) FROM '{}'",
            fwd_slash_path(&sym_pq)
        )).map_err(|e| anyhow::anyhow!("COPY Symbol failed: {e}"))?;

        // Edge tables -- write parquet and COPY FROM with in-memory UNWIND fallback
        #[allow(clippy::type_complexity)]
        let edge_tables: Vec<(&str, &[(String, String)], &str, &str)> = vec![
            ("CONTAINS", &contains_pairs, "Module", "Symbol"),
            ("DEFINES", &defines_pairs, "File", "Symbol"),
            ("CALLS", &calls_pairs, "Symbol", "Symbol"),
            ("INHERITS", &inh_pairs, "Symbol", "Symbol"),
            ("TESTED_BY", &test_pairs, "Symbol", "Symbol"),
            ("IMPORTS", &imp_pairs, "Module", "Module"),
            ("READS", &reads_pairs, "Symbol", "Symbol"),
            ("WRITES", &writes_pairs, "Symbol", "Symbol"),
        ];

        for (table, pairs, src_label, dst_label) in &edge_tables {
            if pairs.is_empty() {
                continue;
            }
            let edge_pq = tmp.join(format!("infigraph_index_{}.parquet", table.to_lowercase()));
            let refs: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            parquet_loader::write_edge_parquet(&edge_pq, &refs)?;
            if let Err(e) = conn.query(&format!("COPY {table} FROM '{}'", fwd_slash_path(&edge_pq)))
            {
                eprintln!("warn: COPY {table} via parquet failed ({e}), falling back to UNWIND");
                unwind_edges_from_pairs(&conn, &refs, table, src_label, dst_label);
            }
            let _ = std::fs::remove_file(&edge_pq);
        }

        // Custom edge tables
        for (edge_name, pairs) in &custom_pairs {
            if pairs.is_empty() { continue; }
            let _ = super::schema::ensure_custom_edge_table(&conn, edge_name);
            let edge_pq = tmp.join(format!("infigraph_index_{}.parquet", edge_name.to_lowercase()));
            let refs: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            parquet_loader::write_edge_parquet(&edge_pq, &refs)?;
            if let Err(e) = conn.query(&format!("COPY {} FROM '{}'", edge_name, fwd_slash_path(&edge_pq)))
            {
                eprintln!("warn: COPY {} via parquet failed ({e}), falling back to UNWIND", edge_name);
                unwind_edges_from_pairs(&conn, &refs, edge_name, "Symbol", "Symbol");
            }
            let _ = std::fs::remove_file(&edge_pq);
        }

        // Cleanup node parquet files
        let _ = std::fs::remove_file(&mod_pq);
        let _ = std::fs::remove_file(&file_pq);
        let _ = std::fs::remove_file(&sym_pq);

        Ok(())
    }
}
