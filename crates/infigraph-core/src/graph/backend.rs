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

/// A single detected dynamic-URL/route match, to be written as a
/// `CALLS_SERVICE` edge from the calling symbol to the matched route
/// handler.
#[derive(Debug, Clone)]
pub struct CallsServiceEdge {
    pub symbol_id: String,
    pub target_id: String,
    pub method: String,
    pub path: String,
}

/// A code-smell/concern match, to be written as a `Concern` node linked to
/// its symbol via `HAS_CONCERN`.
#[derive(Debug, Clone)]
pub struct Concern {
    pub symbol_id: String,
    pub kind: String,
    pub detail: String,
}

/// A resolved dynamic-dispatch/reflection site, to be written as a
/// `RESOLVES_TO` edge from the calling symbol to its resolved target.
#[derive(Debug, Clone)]
pub struct ResolvesToEdge {
    pub caller_symbol: String,
    pub target: String,
    pub mechanism: String,
    pub config_source: String,
}

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

    /// Callers of `symbol_id`, optionally excluding test symbols.
    /// When `include_tests` is true this is equivalent to `callers_of`.
    /// Test symbols are those extracted with `SymbolKind::Test` (see
    /// `extract::entities::is_test_by_name_and_path` / `is_test_by_docstring`),
    /// stored as `kind = 'Test'` on the node. Built on `raw_query` so both the
    /// Kuzu and Neo4j backends inherit it without a bespoke impl — the `kind`
    /// filter is plain Cypher supported by both.
    fn callers_of_filtered(&self, symbol_id: &str, include_tests: bool) -> Result<Vec<String>> {
        if include_tests {
            return self.callers_of(symbol_id);
        }
        let query = format!(
            "MATCH (caller:Symbol)-[:CALLS|INJECTS_DEPENDENCY|REGISTERS_MIDDLEWARE]->(target:Symbol) \
             WHERE target.id = '{}' AND caller.kind <> 'Test' RETURN caller.id",
            symbol_id.replace('\'', "\\'")
        );
        Ok(self
            .raw_query(&query)?
            .into_iter()
            .filter_map(|row| row.into_iter().next())
            .collect())
    }

    /// Callees of `symbol_id`, optionally excluding test symbols.
    /// When `include_tests` is true this is equivalent to `callees_of`.
    fn callees_of_filtered(&self, symbol_id: &str, include_tests: bool) -> Result<Vec<String>> {
        if include_tests {
            return self.callees_of(symbol_id);
        }
        let query = format!(
            "MATCH (source:Symbol)-[:CALLS]->(callee:Symbol) \
             WHERE source.id = '{}' AND callee.kind <> 'Test' RETURN callee.id",
            symbol_id.replace('\'', "\\'")
        );
        Ok(self
            .raw_query(&query)?
            .into_iter()
            .filter_map(|row| row.into_iter().next())
            .collect())
    }
    fn branches_of(&self, symbol_id: &str) -> Result<Vec<BranchInfo>>;
    fn transitive_impact(&self, id: &str, max_depth: u32) -> Result<Vec<ImpactRow>>;
    fn find_all_references(&self, id: &str) -> Result<Vec<ReferenceRow>>;
    fn cross_cutting_for(&self, id: &str) -> Result<Vec<(String, String)>>;

    /// Other Method symbols declared on the same class/interface as `symbol_id`
    /// (siblings), excluding `symbol_id` itself. Method ids are class-scoped
    /// (`file::Class::method`, see extract/entities.rs's find_parent_class) —
    /// this derives the `file::Class::` prefix from `symbol_id` and matches
    /// other Method ids sharing it, entirely from the id string, no extra
    /// Symbol.parent lookup needed. Returns an empty vec (not an error) when
    /// `symbol_id` isn't class-scoped (only one `::`, e.g. a free function) —
    /// there is no "interface" to expand in that case.
    ///
    /// Used to warn callers of `transitive_impact`/`trace_callers` when a
    /// single-method query would silently under-report the true blast radius
    /// of changing the whole interface: querying one method's callers misses
    /// every caller that goes through a sibling method on the same
    /// class/interface (see docs/DESIGN-interface-blast-radius.md).
    fn sibling_methods_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        let Some(prefix_end) = symbol_id.rfind("::") else {
            return Ok(Vec::new());
        };
        let prefix = &symbol_id[..=prefix_end + 1]; // include trailing "::"
        if prefix.matches("::").count() < 2 {
            // Only "file::method" — not class-scoped, nothing to expand.
            return Ok(Vec::new());
        }
        let query = format!(
            "MATCH (m:Symbol) WHERE m.id STARTS WITH '{}' AND m.kind = 'Method' \
             AND m.id <> '{}' RETURN m.id",
            prefix.replace('\'', "\\'"),
            symbol_id.replace('\'', "\\'")
        );
        Ok(self
            .raw_query(&query)?
            .into_iter()
            .filter_map(|row| row.into_iter().next())
            .collect())
    }

    // ── Read: aggregate queries ──────────────────────────────────────

    fn get_api_surface(&self) -> Result<Vec<ApiSymbol>>;

    /// `get_api_surface`, optionally excluding test symbols.
    /// When `include_tests` is true this is equivalent to `get_api_surface`.
    ///
    /// For languages where `visibility` is derived from naming convention
    /// rather than a real access modifier (e.g. Python: any non-underscore
    /// name is "public"), every non-underscore test helper and e2e fixture
    /// otherwise qualifies as "public" too, swamping the real API surface
    /// (observed: 3795/5718 symbols in a FastAPI repo, almost all under
    /// app/test/). Filtered here in Rust rather than duplicating a `kind <>
    /// 'Test'` clause across every backend's own query dialect.
    fn get_api_surface_filtered(&self, include_tests: bool) -> Result<Vec<ApiSymbol>> {
        let surface = self.get_api_surface()?;
        if include_tests {
            return Ok(surface);
        }
        Ok(surface.into_iter().filter(|s| s.kind != "Test").collect())
    }
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

    /// The `org/repo` namespace this backend is scoped to, if any.
    /// Lets backend-agnostic analysis passes (manifest deps, clusters) scope their
    /// own Cypher in shared-graph mode. Default: `None` (Kuzu is single-repo).
    fn repo_filter(&self) -> Option<&str> {
        None
    }

    /// Write a batch of `CALLS_SERVICE` edges as a single atomic operation.
    /// Backend owns the transaction — callers don't manage connections
    /// directly (same design as `upsert_files_bulk`/`resolve_calls`). A
    /// no-op for an empty slice.
    fn write_calls_service_edges(&self, edges: &[CallsServiceEdge]) -> Result<()>;

    /// Replace every recorded `Concern` with `concerns` as a single atomic
    /// operation. Backend owns the transaction — callers don't manage
    /// connections directly (same design as `write_calls_service_edges`).
    ///
    /// Previously implemented by issuing `BEGIN TRANSACTION`/`COMMIT`
    /// through `raw_query` -- both backends' `raw_query` deliberately no-op
    /// those (Kùzu: fresh connection per call, so control statements can't
    /// span calls; Neo4j: transactions are driver-level, not Cypher), so
    /// the delete-then-recreate was never actually atomic. A crash mid-loop
    /// could delete all concerns and only recreate some of them.
    fn replace_concerns(&self, concerns: &[Concern]) -> Result<()>;

    /// Replace every recorded `RESOLVES_TO` edge with `edges` as a single
    /// atomic operation. Same design and same prior non-atomicity bug as
    /// `replace_concerns`.
    fn replace_resolves_to(&self, edges: &[ResolvesToEdge]) -> Result<()>;

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

/// File-path/name fragments that mark a symbol as vendored/third-party
/// rather than app code the user actually owns. `find_uncalled_symbols`
/// has no concept of "vendored" — a minified library or a checked-in
/// diagnostic tool subtree lights up as 100% dead code simply because
/// nothing in the app calls into it, drowning out real findings (observed:
/// 700+ of 12.8k WinEngine candidates were jQuery/d3/modernizr/TraceEvent).
/// Matched case-insensitively against the row's `file` path.
const VENDOR_PATH_FRAGMENTS: &[&str] = &[
    "node_modules/",
    "/vendor/",
    "/vendored/",
    "/third_party/",
    "/thirdparty/",
    "/packages/",
    ".min.js",
    "jquery",
    "modernizr",
    "microsoftajax",
    "d3.v3",
    "d3.min",
    "/traceevent/",
    "/perfview/",
];

fn is_vendor_path(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    VENDOR_PATH_FRAGMENTS
        .iter()
        .any(|frag| lower.contains(frag))
}

/// Filter raw `find_uncalled_symbols` output down to candidates that are
/// actually worth a human's attention:
///
/// 1. Drops vendored/third-party paths (see `VENDOR_PATH_FRAGMENTS`) — dead
///    by definition, never something the user should "clean up".
/// 2. Collapses interface/implementation splits — when a dead `Method` has
///    sibling methods on the same class (via `sibling_methods_of`) and at
///    least one sibling is NOT in the dead set, this row is the classic
///    "interface declaration + every impl flagged separately, 0 callers
///    each" false positive (each individual symbol looks dead but the
///    *behavior* is reachable through a sibling — most commonly an
///    interface member whose callers only ever go through the concrete
///    type). Dropped rather than merged into one entry: the surviving
///    sibling already represents the reachable behavior in the graph, so
///    keeping a placeholder for the dropped ones would just re-introduce
///    noise under a different name.
///
/// Does NOT attempt markup/XAML reachability or any other language-specific
/// reachability signal — that needs project-root filesystem access this
/// backend-agnostic helper doesn't have, and lives at the tool-call layer
/// instead (see `tool_detect_dead_code`).
pub fn filter_dead_code_candidates(
    backend: &dyn GraphBackend,
    rows: Vec<DeadCodeRow>,
) -> Vec<DeadCodeRow> {
    let non_vendor: Vec<DeadCodeRow> = rows
        .into_iter()
        .filter(|r| !is_vendor_path(&r.file))
        .collect();

    let dead_ids: std::collections::HashSet<String> =
        non_vendor.iter().map(|r| r.id.clone()).collect();

    non_vendor
        .into_iter()
        .filter(|row| {
            if row.kind != "Method" {
                return true;
            }
            let siblings = backend.sibling_methods_of(&row.id).unwrap_or_default();
            // Keep only if every sibling is also dead (or there are none) —
            // a live sibling means this row is an interface/impl split, not
            // real dead code.
            siblings.iter().all(|s| dead_ids.contains(s))
        })
        .collect()
}
