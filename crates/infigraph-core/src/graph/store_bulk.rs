use std::collections::HashMap;

use anyhow::{Context, Result};
use kuzu::Connection;

use super::schema::ensure_custom_edge_table;
use super::store::GraphStore;
use super::store_util::{escape, file_stem, import_stem, resolve_import_candidate};
use crate::model::{FileExtraction, RelationKind};

impl GraphStore {
    /// Bulk insert all extractions in minimal queries -- one UNWIND per node/edge type.
    /// Much faster than calling upsert_file_conn_no_delete per file.
    /// Caller must hold WriteLock.
    pub fn upsert_all_bulk(
        &self,
        conn: &Connection<'_>,
        extractions: &[FileExtraction],
    ) -> Result<()> {
        if extractions.is_empty() {
            return Ok(());
        }

        // 1. All Module nodes
        let module_rows: Vec<String> = extractions
            .iter()
            .map(|e| {
                let name = e.file.rsplit_once('/').map(|(_, f)| f).unwrap_or(&e.file);
                format!(
                    "{{id: '{}', name: '{}', file: '{}', language: '{}', content_hash: '{}'}}",
                    escape(&e.file),
                    escape(name),
                    escape(&e.file),
                    escape(&e.language),
                    escape(&e.content_hash)
                )
            })
            .collect();
        conn.query(&format!("UNWIND [{}] AS m CREATE (:Module {{id: m.id, name: m.name, file: m.file, language: m.language, content_hash: m.content_hash}})", module_rows.join(", ")))
            .context("bulk module insert")?;

        // 2. All File nodes
        let file_rows: Vec<String> = extractions
            .iter()
            .map(|e| {
                let name = e.file.rsplit_once('/').map(|(_, f)| f).unwrap_or(&e.file);
                format!(
                    "{{id: '{}', name: '{}', path: '{}', language: '{}', symbol_count: {}}}",
                    escape(&e.file),
                    escape(name),
                    escape(&e.file),
                    escape(&e.language),
                    e.symbols.len()
                )
            })
            .collect();
        conn.query(&format!("UNWIND [{}] AS f CREATE (:File {{id: f.id, name: f.name, path: f.path, language: f.language, symbol_count: f.symbol_count}})", file_rows.join(", ")))
            .context("bulk file insert")?;

        // 3. All Symbol nodes in chunks (query string size limit)
        const SYM_CHUNK: usize = 2000;
        let all_syms: Vec<String> = extractions.iter().flat_map(|e| {
            let cat = super::store_util::classify_file(&e.file);
            e.symbols.iter().map(move |sym| format!(
                "{{id: '{}', name: '{}', kind: '{}', file: '{}', start_line: {}, end_line: {}, signature_hash: '{}', language: '{}', visibility: '{}', parent: '{}', docstring: '{}', complexity: {}, parameters: '{}', return_type: '{}', category: '{}'}}",
                escape(&sym.id), escape(&sym.name), sym.kind.as_str(), escape(&e.file),
                sym.span.start_line, sym.span.end_line, escape(&sym.signature_hash),
                escape(&sym.language), escape(sym.visibility.as_deref().unwrap_or("")),
                escape(sym.parent.as_deref().unwrap_or("")),
                escape(sym.docstring.as_deref().unwrap_or("")), sym.complexity,
                escape(sym.parameters.as_deref().unwrap_or("")),
                escape(sym.return_type.as_deref().unwrap_or("")),
                cat
            ))
        }).collect();
        for chunk in all_syms.chunks(SYM_CHUNK) {
            conn.query(&format!(
                "UNWIND [{}] AS s CREATE (:Symbol {{id: s.id, name: s.name, kind: s.kind, file: s.file, start_line: s.start_line, end_line: s.end_line, signature_hash: s.signature_hash, language: s.language, visibility: s.visibility, parent: s.parent, docstring: s.docstring, complexity: s.complexity, parameters: s.parameters, return_type: s.return_type, category: s.category}})",
                chunk.join(", ")
            )).context("bulk symbol insert")?;
        }

        // 4. CONTAINS edges (module -> symbols) in chunks
        let contains_pairs: Vec<String> = extractions
            .iter()
            .flat_map(|e| {
                e.symbols.iter().map(move |sym| {
                    format!("{{m: '{}', s: '{}'}}", escape(&e.file), escape(&sym.id))
                })
            })
            .collect();
        for chunk in contains_pairs.chunks(SYM_CHUNK) {
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (m:Module), (s:Symbol) WHERE m.id = p.m AND s.id = p.s CREATE (m)-[:CONTAINS]->(s)",
                chunk.join(", ")
            ));
        }

        // 5. DEFINES edges (file -> symbols) in chunks
        let defines_pairs: Vec<String> = extractions
            .iter()
            .flat_map(|e| {
                e.symbols.iter().map(move |sym| {
                    format!("{{f: '{}', s: '{}'}}", escape(&e.file), escape(&sym.id))
                })
            })
            .collect();
        for chunk in defines_pairs.chunks(SYM_CHUNK) {
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (f:File), (s:Symbol) WHERE f.id = p.f AND s.id = p.s CREATE (f)-[:DEFINES]->(s)",
                chunk.join(", ")
            ));
        }

        // Map module-basename (no extension) -> Module file-id, so IMPORTS
        // targets (which are captured as bare module NAMES like "helpers", not
        // file paths) can be resolved to the actual Module node id (its file
        // path). Without this, the IMPORTS write below matched b.id against a
        // "{file}::{module}" string that no Module node ever has, so zero
        // Module->Module edges were created and get_file_deps was always empty
        // (AIF3X-331 #20b). Built from the current batch — on a full index that
        // is every file; incremental single-file re-index can't resolve a
        // cross-file import target here, which is acceptable (get_file_deps is
        // computed against the persisted full graph).
        let mut module_by_stem: HashMap<String, Vec<&str>> = HashMap::new();
        for e in extractions {
            module_by_stem
                .entry(file_stem(&e.file))
                .or_default()
                .push(e.file.as_str());
        }

        // 6. All relation edges grouped by type
        let mut calls_pairs: Vec<String> = Vec::new();
        let mut inherits_pairs: Vec<String> = Vec::new();
        let mut tested_by_pairs: Vec<String> = Vec::new();
        let mut imports_pairs: Vec<String> = Vec::new();
        let mut reads_pairs: Vec<String> = Vec::new();
        let mut writes_pairs: Vec<String> = Vec::new();
        let mut custom_pairs: HashMap<String, Vec<String>> = HashMap::new();
        for e in extractions {
            for rel in &e.relations {
                match &rel.kind {
                    RelationKind::Calls | RelationKind::CalledBy => calls_pairs.push(format!(
                        "{{a: '{}', b: '{}'}}",
                        escape(&rel.source_id),
                        escape(&rel.target_id)
                    )),
                    RelationKind::Inherits | RelationKind::InheritedBy => {
                        inherits_pairs.push(format!(
                            "{{a: '{}', b: '{}'}}",
                            escape(&rel.source_id),
                            escape(&rel.target_id)
                        ))
                    }
                    RelationKind::TestedBy | RelationKind::Tests => tested_by_pairs.push(format!(
                        "{{a: '{}', b: '{}'}}",
                        escape(&rel.source_id),
                        escape(&rel.target_id)
                    )),
                    RelationKind::Imports | RelationKind::ImportedBy => {
                        // target_id is "{file}::{module_name}"; resolve the bare
                        // module name to the imported file's Module id. Skip
                        // external/unresolvable imports (e.g. "fastapi") — they
                        // have no Module node in this project.
                        let module_name =
                            rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);
                        let stem = import_stem(module_name);
                        if let Some(candidates) = module_by_stem.get(stem.as_str()) {
                            if let Some(target_file) =
                                resolve_import_candidate(module_name, candidates)
                            {
                                if target_file != rel.source_id {
                                    imports_pairs.push(format!(
                                        "{{a: '{}', b: '{}'}}",
                                        escape(&rel.source_id),
                                        escape(target_file)
                                    ));
                                }
                            }
                        }
                    }
                    RelationKind::Reads => reads_pairs.push(format!(
                        "{{a: '{}', b: '{}'}}",
                        escape(&rel.source_id),
                        escape(&rel.target_id)
                    )),
                    RelationKind::Writes => writes_pairs.push(format!(
                        "{{a: '{}', b: '{}'}}",
                        escape(&rel.source_id),
                        escape(&rel.target_id)
                    )),
                    RelationKind::Custom(name) => {
                        custom_pairs.entry(name.clone()).or_default().push(format!(
                            "{{a: '{}', b: '{}'}}",
                            escape(&rel.source_id),
                            escape(&rel.target_id)
                        ));
                    }
                    _ => {}
                }
            }
        }
        for (pairs, rel_type) in [
            (&calls_pairs, "CALLS"),
            (&inherits_pairs, "INHERITS"),
            (&tested_by_pairs, "TESTED_BY"),
            (&reads_pairs, "READS"),
            (&writes_pairs, "WRITES"),
        ] {
            for chunk in pairs.chunks(SYM_CHUNK) {
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{rel_type}]->(b)",
                    chunk.join(", ")
                ));
            }
        }
        for chunk in imports_pairs.chunks(SYM_CHUNK) {
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Module), (b:Module) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:IMPORTS]->(b)",
                chunk.join(", ")
            ));
        }
        for (edge_name, pairs) in &custom_pairs {
            if pairs.is_empty() {
                continue;
            }
            let _ = ensure_custom_edge_table(conn, edge_name);
            for chunk in pairs.chunks(SYM_CHUNK) {
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS p MATCH (a:Symbol), (b:Symbol) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{}]->(b)",
                    chunk.join(", "),
                    edge_name
                ));
            }
        }

        // Statement nodes + HAS_STATEMENT edges
        let all_stmts: Vec<String> = extractions.iter().flat_map(|e| {
            e.statements.iter().map(|s| format!(
                "{{id: '{}', kind: '{}', condition: '{}', start_line: {}, end_line: {}, depth: {}, parent_symbol: '{}'}}",
                escape(&s.id), s.kind.as_str(), escape(&s.condition),
                s.start_line, s.end_line, s.depth, escape(&s.parent_symbol)
            ))
        }).collect();
        for chunk in all_stmts.chunks(SYM_CHUNK) {
            let _ = conn.query(&format!(
                "UNWIND [{}] AS s CREATE (:Statement {{id: s.id, kind: s.kind, condition: s.condition, start_line: s.start_line, end_line: s.end_line, depth: s.depth, parent_symbol: s.parent_symbol}})",
                chunk.join(", ")
            ));
        }
        let stmt_edges: Vec<String> = extractions
            .iter()
            .flat_map(|e| {
                e.statements.iter().map(|s| {
                    format!(
                        "{{a: '{}', b: '{}'}}",
                        escape(&s.parent_symbol),
                        escape(&s.id)
                    )
                })
            })
            .collect();
        for chunk in stmt_edges.chunks(SYM_CHUNK) {
            let _ = conn.query(&format!(
                "UNWIND [{}] AS p MATCH (a:Symbol), (b:Statement) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:HAS_STATEMENT]->(b)",
                chunk.join(", ")
            ));
        }

        Ok(())
    }
}
