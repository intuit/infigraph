use std::collections::HashMap;

use anyhow::Result;

use std::path::Path;

use crate::learned::LearnedStore;
use crate::model::FileExtraction;
use crate::resolve::ResolveStats;
use crate::scip::ImportStats;
use crate::structured::{IngestResult, SchemaMeta};

use super::{
    ApiSymbol, ArchitectureStats, BranchInfo, ComplexityRow, DeadCodeRow, FileDeps, GraphStats,
    ImpactRow, ReferenceRow, SymbolDetail, SymbolMeta, SymbolRow, SymbolWithDocstring, TestContext,
    TestCoverage, TypeHierarchy,
};

/// Backend-agnostic graph storage interface.
///
/// KuzuBackend wraps the existing embedded Kùzu store (local mode).
/// Neo4jBackend (Phase 2) connects to a sidecar via Bolt (remote mode).
/// All methods are synchronous — async backends use internal `block_on`.
pub trait GraphBackend: Send + Sync {
    // ── Lifecycle / metadata ─────────────────────────────────────────

    fn stats(&self) -> Result<GraphStats>;
    fn get_file_hashes(&self) -> Result<HashMap<String, String>>;
    fn get_all_symbols(&self) -> Result<Vec<(String, String, String, String)>>;

    // ── Read: symbol queries ─────────────────────────────────────────

    fn symbols_in_file(&self, file: &str) -> Result<Vec<SymbolRow>>;
    fn find_symbol_by_id(&self, id: &str) -> Result<Option<SymbolDetail>>;
    fn symbols_in_range(&self, file: &str, start: u32, end: u32) -> Result<Vec<SymbolDetail>>;
    fn skeleton(&self, file: &str) -> Result<String>;

    // ── Read: graph traversal ────────────────────────────────────────

    fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>>;
    fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>>;
    fn branches_of(&self, symbol_id: &str) -> Result<Vec<BranchInfo>>;
    fn transitive_impact(&self, id: &str, max_depth: u32) -> Result<Vec<ImpactRow>>;
    fn find_all_references(&self, id: &str) -> Result<Vec<ReferenceRow>>;
    fn cross_cutting_for(&self, id: &str) -> Result<Vec<(String, String)>>;

    // ── Read: aggregate queries ──────────────────────────────────────

    fn get_api_surface(&self) -> Result<Vec<ApiSymbol>>;
    fn get_file_deps(&self, file: &str) -> Result<FileDeps>;
    fn get_type_hierarchy(&self, id: &str, max_depth: u32) -> Result<TypeHierarchy>;
    fn get_test_coverage(&self) -> Result<TestCoverage>;
    fn generate_test_context(
        &self,
        file_filter: Option<&str>,
        limit: usize,
        test_type: Option<&str>,
    ) -> Result<TestContext>;

    // ── Read: raw query ──────────────────────────────────────────────

    fn raw_query(&self, query: &str) -> Result<Vec<Vec<String>>>;

    /// Return all symbols with 7 columns in fixed order:
    /// [id, name, kind, file, docstring, start_line, end_line].
    /// Used by search to build BM25 index + display results.
    /// Default impl uses raw_query (safe for Kuzu where column order matches RETURN order).
    fn get_symbols_for_search(&self) -> Result<Vec<Vec<String>>> {
        self.raw_query(
            "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.start_line, s.end_line",
        )
    }

    // ── Phase-2: backend-agnostic query methods ──────────────────────

    fn symbol_metadata(&self, id: &str) -> Result<Option<SymbolMeta>>;
    fn get_complexity_ranking(&self, file_filter: Option<&str>) -> Result<Vec<ComplexityRow>>;
    fn list_indexed_files(&self) -> Result<Vec<String>>;
    fn find_uncalled_symbols(&self) -> Result<Vec<DeadCodeRow>>;
    fn get_architecture_stats(&self) -> Result<ArchitectureStats>;
    fn symbols_with_docstring(
        &self,
        kind_filter: Option<&[&str]>,
    ) -> Result<Vec<SymbolWithDocstring>>;
    fn upsert_similar_edge(&self, id_a: &str, id_b: &str, score: f32) -> Result<()>;

    // ── Write ────────────────────────────────────────────────────────

    /// Insert a single file extraction (delete existing + insert).
    fn upsert_file(&self, extraction: &FileExtraction) -> Result<()>;

    /// Bulk write: delete stale data for given files, bulk-load all
    /// extractions, and upsert folder hierarchy. Owns the full
    /// delete-stale → bulk-insert → folders pipeline.
    /// `existing_hashes` being empty signals a fresh index (no deletes needed).
    fn upsert_files_bulk(
        &self,
        extractions: &[FileExtraction],
        existing_hashes_empty: bool,
    ) -> Result<()>;

    /// Remove a single file and all its symbols/edges from the graph.
    fn remove_file(&self, file: &str) -> Result<()>;

    /// Derive TESTED_BY edges from naming conventions.
    /// When `changed_files` is provided, only derives edges where at least one
    /// endpoint (test or target) is in the changed set — needed because
    /// DETACH DELETE on target files destroys existing TESTED_BY edges.
    fn derive_tested_by_edges(&self, changed_files: Option<&[&str]>) -> Result<usize>;

    /// Delete all data from the graph (used by `--full` reindex in remote mode).
    /// Default: no-op (local backends wipe `~/.infigraph/` on disk instead).
    fn clear_all_data(&self) -> Result<()> {
        Ok(())
    }

    /// Create a Repo node and link all File nodes to it via BELONGS_TO.
    /// Sets `repo` property on File nodes for scoped queries.
    /// Default: no-op (only meaningful for Neo4j multi-repo graphs).
    fn upsert_repo(&self, _repo_name: &str) -> Result<()> {
        Ok(())
    }

    // ── Resolve ──────────────────────────────────────────────────────

    /// Run call/inheritance resolution for the given extractions.
    /// Backend owns the raw Cypher — callers don't need a Connection.
    fn resolve_calls(
        &self,
        extractions: &[FileExtraction],
        learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats>;

    /// Re-resolve CALLS/INHERITS edges for specific files only.
    /// Deletes existing edges for the given files, then re-resolves
    /// using the full symbol map from the graph.
    fn re_resolve_for_files(
        &self,
        files: &[String],
        extractions: &[FileExtraction],
        learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats>;

    // ── SCIP import ──────────────────────────────────────────────────

    fn import_scip_index(
        &self,
        index_path: &Path,
        project_root: Option<&Path>,
    ) -> Result<ImportStats>;

    // ── Structured ingestion ────────────────────────────────────────

    fn ingest_structured_data(
        &self,
        schema: &SchemaMeta,
        data: &[serde_json::Value],
    ) -> Result<IngestResult>;

    fn ingest_structured_file(&self, schema: &SchemaMeta, path: &Path) -> Result<IngestResult>;

    fn ingest_structured_directory(&self, schema: &SchemaMeta, dir: &Path) -> Result<IngestResult>;
}
