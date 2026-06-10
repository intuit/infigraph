use std::collections::HashMap;

use anyhow::{Context, Result};
use kuzu::Connection;

use super::schema::ensure_custom_edge_table;
use super::store::GraphStore;
use super::store_util::escape;
use crate::model::{FileExtraction, RelationKind};

impl GraphStore {
    /// Insert a file extraction into the graph.
    /// Removes old data for the file first (incremental update).
    pub fn upsert_file(&self, extraction: &FileExtraction) -> Result<()> {
        let conn = self.connection()?;
        self.upsert_file_conn(&conn, extraction)
    }

    pub fn upsert_file_conn(
        &self,
        conn: &Connection<'_>,
        extraction: &FileExtraction,
    ) -> Result<()> {
        // Remove old symbols for this file
        let _ = conn.query(&format!(
            "MATCH (s:Symbol) WHERE s.file = '{}' DETACH DELETE s",
            escape(&extraction.file)
        ));
        let _ = conn.query(&format!(
            "MATCH (m:Module) WHERE m.file = '{}' DETACH DELETE m",
            escape(&extraction.file)
        ));
        let _ = conn.query(&format!(
            "MATCH (f:File) WHERE f.id = '{}' DETACH DELETE f",
            escape(&extraction.file)
        ));
        self.upsert_file_conn_no_delete(conn, extraction)
    }

    pub fn upsert_file_conn_no_delete(
        &self,
        conn: &Connection<'_>,
        extraction: &FileExtraction,
    ) -> Result<()> {
        // Insert module node
        let module_id = &extraction.file;
        let module_name = extraction
            .file
            .rsplit_once('/')
            .map(|(_, f)| f)
            .unwrap_or(&extraction.file);
        let insert_module = format!(
            "CREATE (m:Module {{id: '{}', name: '{}', file: '{}', language: '{}', content_hash: '{}'}})",
            escape(module_id),
            escape(module_name),
            escape(&extraction.file),
            escape(&extraction.language),
            escape(&extraction.content_hash),
        );
        conn.query(&insert_module)
            .context("failed to insert module")?;

        // Insert File node
        let file_name = extraction
            .file
            .rsplit_once('/')
            .map(|(_, f)| f)
            .unwrap_or(&extraction.file);
        let symbol_count = extraction.symbols.len() as i32;
        let insert_file = format!(
            "CREATE (f:File {{id: '{}', name: '{}', path: '{}', language: '{}', symbol_count: {}}})",
            escape(&extraction.file),
            escape(file_name),
            escape(&extraction.file),
            escape(&extraction.language),
            symbol_count,
        );
        conn.query(&insert_file)
            .context("failed to insert file node")?;

        // Folder hierarchy is handled in bulk by upsert_folders_bulk — skip per-file here

        // Batch insert symbols via UNWIND
        if !extraction.symbols.is_empty() {
            let sym_rows: Vec<String> = extraction.symbols.iter().map(|sym| {
                format!(
                    "{{id: '{}', name: '{}', kind: '{}', file: '{}', start_line: {}, end_line: {}, signature_hash: '{}', language: '{}', visibility: '{}', parent: '{}', docstring: '{}', complexity: {}, parameters: '{}', return_type: '{}'}}",
                    escape(&sym.id),
                    escape(&sym.name),
                    sym.kind.as_str(),
                    escape(&extraction.file),
                    sym.span.start_line,
                    sym.span.end_line,
                    escape(&sym.signature_hash),
                    escape(&sym.language),
                    escape(sym.visibility.as_deref().unwrap_or("")),
                    escape(sym.parent.as_deref().unwrap_or("")),
                    escape(sym.docstring.as_deref().unwrap_or("")),
                    sym.complexity,
                    escape(sym.parameters.as_deref().unwrap_or("")),
                    escape(sym.return_type.as_deref().unwrap_or("")),
                )
            }).collect();
            let batch_insert = format!(
                "UNWIND [{}] AS s CREATE (:Symbol {{id: s.id, name: s.name, kind: s.kind, file: s.file, start_line: s.start_line, end_line: s.end_line, signature_hash: s.signature_hash, language: s.language, visibility: s.visibility, parent: s.parent, docstring: s.docstring, complexity: s.complexity, parameters: s.parameters, return_type: s.return_type}})",
                sym_rows.join(", ")
            );
            conn.query(&batch_insert)
                .context("failed to batch insert symbols")?;

            // Batch CONTAINS edges: module -> symbols
            let sym_ids: Vec<String> = extraction
                .symbols
                .iter()
                .map(|s| format!("'{}'", escape(&s.id)))
                .collect();
            let contains_batch = format!(
                "MATCH (m:Module), (s:Symbol) WHERE m.id = '{}' AND s.id IN [{}] CREATE (m)-[:CONTAINS]->(s)",
                escape(module_id),
                sym_ids.join(", ")
            );
            let _ = conn.query(&contains_batch);

            // Batch DEFINES edges: file -> symbols
            let defines_batch = format!(
                "MATCH (f:File), (s:Symbol) WHERE f.id = '{}' AND s.id IN [{}] CREATE (f)-[:DEFINES]->(s)",
                escape(&extraction.file),
                sym_ids.join(", ")
            );
            let _ = conn.query(&defines_batch);
        }

        // Batch insert relationships grouped by type
        let mut calls_pairs: Vec<(&str, &str)> = Vec::new();
        let mut inherits_pairs: Vec<(&str, &str)> = Vec::new();
        let mut tested_by_pairs: Vec<(&str, &str)> = Vec::new();
        let mut imports_pairs: Vec<(&str, &str)> = Vec::new();
        let mut reads_pairs: Vec<(&str, &str)> = Vec::new();
        let mut writes_pairs: Vec<(&str, &str)> = Vec::new();
        let mut custom_pairs: HashMap<String, Vec<(&str, &str)>> = HashMap::new();
        for rel in &extraction.relations {
            match &rel.kind {
                RelationKind::Calls | RelationKind::CalledBy => {
                    calls_pairs.push((&rel.source_id, &rel.target_id))
                }
                RelationKind::Inherits | RelationKind::InheritedBy => {
                    inherits_pairs.push((&rel.source_id, &rel.target_id))
                }
                RelationKind::TestedBy | RelationKind::Tests => {
                    tested_by_pairs.push((&rel.source_id, &rel.target_id))
                }
                RelationKind::Imports | RelationKind::ImportedBy => {
                    imports_pairs.push((&rel.source_id, &rel.target_id))
                }
                RelationKind::Reads => reads_pairs.push((&rel.source_id, &rel.target_id)),
                RelationKind::Writes => writes_pairs.push((&rel.source_id, &rel.target_id)),
                RelationKind::Custom(name) => {
                    custom_pairs
                        .entry(name.clone())
                        .or_default()
                        .push((&rel.source_id, &rel.target_id));
                }
                _ => {}
            }
        }
        for (pairs, rel_type) in [
            (&calls_pairs, "CALLS"),
            (&inherits_pairs, "INHERITS"),
            (&tested_by_pairs, "TESTED_BY"),
            (&reads_pairs, "READS"),
            (&writes_pairs, "WRITES"),
        ] {
            if pairs.is_empty() {
                continue;
            }
            let pair_list: Vec<String> = pairs
                .iter()
                .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
                .collect();
            let batch_rel = format!(
                "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{}]->(b)",
                pair_list.join(", "),
                rel_type
            );
            let _ = conn.query(&batch_rel);
        }
        if !imports_pairs.is_empty() {
            let pair_list: Vec<String> = imports_pairs
                .iter()
                .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
                .collect();
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Module), (b:Module) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:IMPORTS]->(b)",
                pair_list.join(", ")
            ));
        }
        for (edge_name, pairs) in &custom_pairs {
            if pairs.is_empty() { continue; }
            let _ = ensure_custom_edge_table(conn, edge_name);
            let pair_list: Vec<String> = pairs.iter()
                .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
                .collect();
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{}]->(b)",
                pair_list.join(", "),
                edge_name
            ));
        }

        Ok(())
    }

    /// Create Folder nodes for each ancestor directory and wire up
    /// CONTAINS_FOLDER (parent -> child) and CONTAINS_FILE (leaf folder -> file) edges.
    #[allow(dead_code)]
    fn upsert_folder_hierarchy(&self, conn: &Connection<'_>, file_path: &str) -> Result<()> {
        // Split the file path into components: "src/graph/store.rs" -> ["src", "graph"]
        let parts: Vec<&str> = file_path.rsplitn(2, '/').collect();
        let dir_path = if parts.len() == 2 {
            parts[1]
        } else {
            return Ok(());
        };

        // Collect all ancestor folders: "src/graph" -> ["src", "src/graph"]
        let segments: Vec<&str> = dir_path.split('/').collect();
        let mut folder_paths: Vec<String> = Vec::with_capacity(segments.len());
        for i in 0..segments.len() {
            let path = segments[..=i].join("/");
            folder_paths.push(path);
        }

        // Create Folder nodes (MERGE-style: only create if not exists)
        for folder_path in &folder_paths {
            let folder_name = folder_path
                .rsplit_once('/')
                .map(|(_, n)| n)
                .unwrap_or(folder_path);
            let merge_folder = format!("MERGE (d:Folder {{id: '{}'}})", escape(folder_path),);
            // Try MERGE first; if Kuzu doesn't support MERGE, fall back to conditional create
            if conn.query(&merge_folder).is_err() {
                // Check if it already exists
                let check = format!(
                    "MATCH (d:Folder) WHERE d.id = '{}' RETURN d.id",
                    escape(folder_path)
                );
                let mut result = conn
                    .query(&check)
                    .map_err(|e| anyhow::anyhow!("folder check failed: {e}"))?;
                if result.next().is_none() {
                    let create = format!(
                        "CREATE (d:Folder {{id: '{}', name: '{}', path: '{}'}})",
                        escape(folder_path),
                        escape(folder_name),
                        escape(folder_path),
                    );
                    let _ = conn.query(&create);
                }
            } else {
                // MERGE succeeded but may not have set name/path; update them
                let update = format!(
                    "MATCH (d:Folder) WHERE d.id = '{}' SET d.name = '{}', d.path = '{}'",
                    escape(folder_path),
                    escape(folder_name),
                    escape(folder_path),
                );
                let _ = conn.query(&update);
            }
        }

        // Create CONTAINS_FOLDER edges between consecutive folders
        for i in 1..folder_paths.len() {
            let parent = &folder_paths[i - 1];
            let child = &folder_paths[i];
            // Check if edge already exists
            let check_edge = format!(
                "MATCH (p:Folder)-[:CONTAINS_FOLDER]->(c:Folder) WHERE p.id = '{}' AND c.id = '{}' RETURN p.id",
                escape(parent),
                escape(child),
            );
            let mut result = conn
                .query(&check_edge)
                .map_err(|e| anyhow::anyhow!("edge check failed: {e}"))?;
            if result.next().is_none() {
                let create_edge = format!(
                    "MATCH (p:Folder), (c:Folder) WHERE p.id = '{}' AND c.id = '{}' CREATE (p)-[:CONTAINS_FOLDER]->(c)",
                    escape(parent),
                    escape(child),
                );
                let _ = conn.query(&create_edge);
            }
        }

        // Create CONTAINS_FILE edge from leaf folder to File node
        if let Some(leaf_folder) = folder_paths.last() {
            let check_edge = format!(
                "MATCH (d:Folder)-[:CONTAINS_FILE]->(f:File) WHERE d.id = '{}' AND f.id = '{}' RETURN d.id",
                escape(leaf_folder),
                escape(file_path),
            );
            let mut result = conn
                .query(&check_edge)
                .map_err(|e| anyhow::anyhow!("edge check failed: {e}"))?;
            if result.next().is_none() {
                let create_edge = format!(
                    "MATCH (d:Folder), (f:File) WHERE d.id = '{}' AND f.id = '{}' CREATE (d)-[:CONTAINS_FILE]->(f)",
                    escape(leaf_folder),
                    escape(file_path),
                );
                let _ = conn.query(&create_edge);
            }
        }

        Ok(())
    }
}
