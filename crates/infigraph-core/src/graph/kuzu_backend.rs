use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::learned::LearnedStore;
use crate::model::FileExtraction;
use crate::resolve::ResolveStats;

use super::backend::GraphBackend;
use super::queries::GraphQuery;
use super::store::GraphStore;
use super::{
    ApiSymbol, ArchitectureStats, BranchInfo, ComplexityRow, DeadCodeRow, FileDeps, FileHotspot,
    GraphStats, HubFunction, ImpactRow, KindCount, LanguageCount, ReferenceRow, SymbolDetail,
    SymbolMeta, SymbolRow, SymbolWithDocstring, TestContext, TestCoverage, TypeHierarchy,
};

/// Kùzu-backed graph storage (embedded, local mode).
///
/// Wraps existing `GraphStore` + `GraphQuery`. All write methods acquire
/// the write lock internally. Single-writer — concurrent `upsert_files_bulk`
/// calls will serialize on the lock.
pub struct KuzuBackend {
    store: GraphStore,
}

impl KuzuBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let store = GraphStore::open(path)?;
        Ok(Self { store })
    }

    pub fn open_read_only(path: &Path) -> Result<Self> {
        let store = GraphStore::open_read_only(path)?;
        Ok(Self { store })
    }

    /// Wrap an already-opened GraphStore (avoids double-open).
    pub fn from_store(store: GraphStore) -> Self {
        Self { store }
    }

    /// Access underlying GraphStore (escape hatch for callers that
    /// still need raw Kùzu access during migration).
    pub fn inner(&self) -> &GraphStore {
        &self.store
    }
}

fn escape(s: &str) -> String {
    s.replace('\'', "\\'")
}

impl GraphBackend for KuzuBackend {
    // ── Lifecycle / metadata ─────────────────────────────────────────

    fn stats(&self) -> Result<GraphStats> {
        self.store.stats()
    }

    fn get_file_hashes(&self) -> Result<HashMap<String, String>> {
        self.store.get_file_hashes()
    }

    fn get_all_symbols(&self) -> Result<Vec<(String, String, String, String)>> {
        self.store.get_all_symbols()
    }

    // ── Read: symbol queries ─────────────────────────────────────────

    fn symbols_in_file(&self, file: &str) -> Result<Vec<SymbolRow>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.symbols_in_file(file)
    }

    fn find_symbol_by_id(&self, id: &str) -> Result<Option<SymbolDetail>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.find_symbol_by_id(id)
    }

    fn symbols_in_range(&self, file: &str, start: u32, end: u32) -> Result<Vec<SymbolDetail>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.symbols_in_range(file, start, end)
    }

    fn skeleton(&self, file: &str) -> Result<String> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.skeleton(file)
    }

    // ── Read: graph traversal ────────────────────────────────────────

    fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.callers_of(symbol_id)
    }

    fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.callees_of(symbol_id)
    }

    fn branches_of(&self, symbol_id: &str) -> Result<Vec<BranchInfo>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.branches_of(symbol_id)
    }

    fn transitive_impact(&self, id: &str, max_depth: u32) -> Result<Vec<ImpactRow>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.transitive_impact(id, max_depth)
    }

    fn find_all_references(&self, id: &str) -> Result<Vec<ReferenceRow>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.find_all_references(id)
    }

    fn cross_cutting_for(&self, id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.cross_cutting_for(id)
    }

    // ── Read: aggregate queries ──────────────────────────────────────

    fn get_api_surface(&self) -> Result<Vec<ApiSymbol>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.get_api_surface()
    }

    fn get_file_deps(&self, file: &str) -> Result<FileDeps> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.get_file_deps(file)
    }

    fn get_type_hierarchy(&self, id: &str, max_depth: u32) -> Result<TypeHierarchy> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.get_type_hierarchy(id, max_depth)
    }

    fn get_test_coverage(&self) -> Result<TestCoverage> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.get_test_coverage()
    }

    fn generate_test_context(
        &self,
        file_filter: Option<&str>,
        limit: usize,
        test_type: Option<&str>,
    ) -> Result<TestContext> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.generate_test_context(file_filter, limit, test_type)
    }

    // ── Read: raw query ──────────────────────────────────────────────

    fn raw_query(&self, query: &str) -> Result<Vec<Vec<String>>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        q.raw_query(query)
    }

    // ── Phase-2: backend-agnostic query methods ──────────────────────

    fn symbol_metadata(&self, id: &str) -> Result<Option<SymbolMeta>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        let eid = escape(id);
        let meta_rows = q.raw_query(&format!(
            "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.docstring, s.complexity",
            eid
        ))?;
        if meta_rows.is_empty() {
            return Ok(None);
        }
        let row = &meta_rows[0];
        let docstring = row.first().cloned().unwrap_or_default();
        let complexity: u32 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        let parent_rows = q.raw_query(&format!(
            "MATCH (parent)-[:CONTAINS]->(s:Symbol) WHERE s.id = '{}' RETURN parent.id, parent.name",
            eid
        ))?;
        let (parent_id, parent_name) = if let Some(pr) = parent_rows.first() {
            (pr.first().cloned(), pr.get(1).cloned())
        } else {
            (None, None)
        };

        Ok(Some(SymbolMeta {
            docstring,
            complexity,
            parent_id,
            parent_name,
        }))
    }

    fn get_complexity_ranking(&self, file_filter: Option<&str>) -> Result<Vec<ComplexityRow>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        let cypher = if let Some(f) = file_filter {
            format!(
                "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') \
                 AND s.file CONTAINS '{}' RETURN s.name, s.file, s.start_line, s.complexity \
                 ORDER BY s.complexity DESC",
                escape(f)
            )
        } else {
            "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') \
             RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC"
                .to_string()
        };
        let rows = q.raw_query(&cypher)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(ComplexityRow {
                    name: r.first()?.clone(),
                    file: r.get(1)?.clone(),
                    start_line: r.get(2)?.parse().unwrap_or(0),
                    complexity: r.get(3)?.parse().unwrap_or(0),
                })
            })
            .collect())
    }

    fn list_indexed_files(&self) -> Result<Vec<String>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        let rows = q.raw_query("MATCH (s:Symbol) RETURN DISTINCT s.file ORDER BY s.file")?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.into_iter().next())
            .collect())
    }

    fn find_uncalled_symbols(&self) -> Result<Vec<DeadCodeRow>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        let rows = q.raw_query(
            "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
             AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } \
             RETURN s.name, s.kind, s.file ORDER BY s.file, s.name",
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(DeadCodeRow {
                    name: r.first()?.clone(),
                    kind: r.get(1)?.clone(),
                    file: r.get(2)?.clone(),
                })
            })
            .collect())
    }

    fn get_architecture_stats(&self) -> Result<ArchitectureStats> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);

        let lang_rows =
            q.raw_query("MATCH (m:Module) RETURN m.language, count(m) ORDER BY count(m) DESC")?;
        let languages: Vec<LanguageCount> = lang_rows
            .into_iter()
            .filter_map(|r| {
                Some(LanguageCount {
                    language: r.first()?.clone(),
                    count: r.get(1)?.parse().unwrap_or(0),
                })
            })
            .collect();

        let kind_rows =
            q.raw_query("MATCH (s:Symbol) RETURN s.kind, count(s) ORDER BY count(s) DESC")?;
        let kind_counts: Vec<KindCount> = kind_rows
            .into_iter()
            .filter_map(|r| {
                Some(KindCount {
                    kind: r.first()?.clone(),
                    count: r.get(1)?.parse().unwrap_or(0),
                })
            })
            .collect();

        let hotspot_rows = q.raw_query(
            "MATCH (s:Symbol) RETURN s.file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10",
        )?;
        let hotspot_files: Vec<FileHotspot> = hotspot_rows
            .into_iter()
            .filter_map(|r| {
                Some(FileHotspot {
                    file: r.first()?.clone(),
                    count: r.get(1)?.parse().unwrap_or(0),
                })
            })
            .collect();

        let hub_rows = q.raw_query(
            "MATCH ()-[r:CALLS]->(s:Symbol) RETURN s.name, s.file, count(r) AS calls \
             ORDER BY calls DESC LIMIT 10",
        )?;
        let hub_functions: Vec<HubFunction> = hub_rows
            .into_iter()
            .filter_map(|r| {
                Some(HubFunction {
                    name: r.first()?.clone(),
                    file: r.get(1)?.clone(),
                    calls: r.get(2)?.parse().unwrap_or(0),
                })
            })
            .collect();

        let entry_rows = q.raw_query(
            "MATCH (s:Symbol)-[:CALLS]->() WHERE s.kind IN ['Function', 'Method'] \
             AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } \
             RETURN DISTINCT s.name, s.kind, s.file ORDER BY s.file, s.name LIMIT 20",
        )?;
        let entry_points: Vec<DeadCodeRow> = entry_rows
            .into_iter()
            .filter_map(|r| {
                Some(DeadCodeRow {
                    name: r.first()?.clone(),
                    kind: r.get(1)?.clone(),
                    file: r.get(2)?.clone(),
                })
            })
            .collect();

        Ok(ArchitectureStats {
            languages,
            kind_counts,
            hotspot_files,
            hub_functions,
            entry_points,
        })
    }

    fn symbols_with_docstring(
        &self,
        kind_filter: Option<&[&str]>,
    ) -> Result<Vec<SymbolWithDocstring>> {
        let conn = self.store.connection()?;
        let q = GraphQuery::new(&conn);
        let cypher = if let Some(kinds) = kind_filter {
            let cond: Vec<String> = kinds
                .iter()
                .map(|k| format!("s.kind = '{}'", escape(k)))
                .collect();
            format!(
                "MATCH (s:Symbol) WHERE ({}) RETURN s.id, s.name, s.kind, s.file, s.docstring",
                cond.join(" OR ")
            )
        } else {
            "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring".to_string()
        };
        let rows = q.raw_query(&cypher)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(SymbolWithDocstring {
                    id: r.first()?.clone(),
                    name: r.get(1)?.clone(),
                    kind: r.get(2)?.clone(),
                    file: r.get(3)?.clone(),
                    docstring: r.get(4).cloned().unwrap_or_default(),
                })
            })
            .collect())
    }

    fn upsert_similar_edge(&self, id_a: &str, id_b: &str, score: f32) -> Result<()> {
        let conn = self.store.connection()?;
        conn.query(&format!(
            "MATCH (a:Symbol), (b:Symbol) WHERE a.id = '{}' AND b.id = '{}' \
             MERGE (a)-[r:SIMILAR_TO]->(b) SET r.score = {}",
            escape(id_a),
            escape(id_b),
            score
        ))
        .map_err(|e| anyhow::anyhow!("upsert_similar_edge failed: {}", e))?;
        Ok(())
    }

    // ── Write ────────────────────────────────────────────────────────

    fn upsert_file(&self, extraction: &FileExtraction) -> Result<()> {
        self.store.upsert_file(extraction)
    }

    fn upsert_files_bulk(
        &self,
        extractions: &[FileExtraction],
        existing_hashes_empty: bool,
    ) -> Result<()> {
        if extractions.is_empty() {
            return Ok(());
        }

        let _write_lock = self.store.write_lock()?;

        let use_csv = existing_hashes_empty || extractions.len() > 100;

        if use_csv {
            // Parquet bulk path: delete stale → COPY FROM → folders
            if !existing_hashes_empty {
                let conn = self.store.connection()?;
                conn.query("BEGIN TRANSACTION")
                    .context("failed to begin delete transaction")?;
                self.delete_files_data(&conn, extractions)?;
                conn.query("COMMIT")
                    .context("failed to commit delete transaction")?;
            }
            let conn = self.store.connection()?;
            self.store.upsert_all_parquet_conn(&conn, extractions)?;
        } else {
            // Per-file UNWIND path for small incremental updates
            let conn = self.store.connection()?;
            conn.query("BEGIN TRANSACTION")
                .context("failed to begin index transaction")?;
            self.delete_files_data(&conn, extractions)?;
            for extraction in extractions {
                self.store.upsert_file_conn_no_delete(&conn, extraction)?;
            }
            conn.query("COMMIT")
                .context("failed to commit index transaction")?;
        }

        // Upsert folder hierarchy
        let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
        let conn = self.store.connection()?;
        self.store.upsert_folders_bulk_conn(&conn, &file_paths)?;

        Ok(())
    }

    fn remove_file(&self, file: &str) -> Result<()> {
        self.store.remove_file(file)
    }

    fn derive_tested_by_edges(&self, _changed_files: Option<&[&str]>) -> Result<usize> {
        self.store.derive_tested_by_edges()
    }

    // ── Resolve ──────────────────────────────────────────────────────

    fn resolve_calls(
        &self,
        extractions: &[FileExtraction],
        learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats> {
        crate::resolve::resolve_calls_incremental(&self.store, extractions, learned)
    }

    fn re_resolve_for_files(
        &self,
        files: &[String],
        extractions: &[FileExtraction],
        learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats> {
        crate::resolve::re_resolve_for_files(&self.store, files, extractions, learned)
    }

    fn import_scip_index(
        &self,
        index_path: &std::path::Path,
        project_root: Option<&std::path::Path>,
    ) -> Result<crate::scip::ImportStats> {
        crate::scip::import_scip_index(index_path, &self.store, project_root)
    }

    fn ingest_structured_data(
        &self,
        schema: &crate::structured::SchemaMeta,
        data: &[serde_json::Value],
    ) -> Result<crate::structured::IngestResult> {
        let _lock = self.store.write_lock()?;
        let conn = self.store.connection()?;
        crate::structured::ingest_data(&conn, schema, data)
    }

    fn ingest_structured_file(
        &self,
        schema: &crate::structured::SchemaMeta,
        path: &std::path::Path,
    ) -> Result<crate::structured::IngestResult> {
        let _lock = self.store.write_lock()?;
        let conn = self.store.connection()?;
        crate::structured::ingest_file(&conn, schema, path)
    }

    fn ingest_structured_directory(
        &self,
        schema: &crate::structured::SchemaMeta,
        dir: &std::path::Path,
    ) -> Result<crate::structured::IngestResult> {
        let _lock = self.store.write_lock()?;
        let conn = self.store.connection()?;
        crate::structured::ingest_directory(&conn, schema, dir)
    }
}

impl KuzuBackend {
    /// Delete all graph data for the given files. Caller manages the transaction.
    fn delete_files_data(
        &self,
        conn: &kuzu::Connection<'_>,
        extractions: &[FileExtraction],
    ) -> Result<()> {
        let file_list: Vec<String> = extractions
            .iter()
            .map(|e| format!("'{}'", escape(&e.file)))
            .collect();
        let files_in = file_list.join(", ");

        let _ = conn.query(&format!(
            "MATCH (f:File)-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) WHERE f.id IN [{}] DETACH DELETE st",
            files_in
        ));
        let _ = conn.query(&format!(
            "MATCH (s:Symbol) WHERE s.file IN [{}] DETACH DELETE s",
            files_in
        ));
        let _ = conn.query(&format!(
            "MATCH (m:Module) WHERE m.file IN [{}] DETACH DELETE m",
            files_in
        ));
        let _ = conn.query(&format!(
            "MATCH (f:File) WHERE f.id IN [{}] DETACH DELETE f",
            files_in
        ));

        Ok(())
    }
}
