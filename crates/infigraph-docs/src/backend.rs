use std::collections::HashMap;

use anyhow::Result;

use crate::chunk::Chunk;
use crate::extract::ExtractedDoc;
use crate::store::{ChunkDetail, DocStoreStats, ImpactResult, PipelineCoreRecord};

/// Trait abstracting document graph storage.
///
/// `DocStore` (Kùzu) is the local implementation.
/// `Neo4jDocStore` is the remote implementation (behind `feature = "remote"`).
pub trait DocBackend {
    // ── Document CRUD ────────────────────────────────────────────────────

    fn get_doc_hashes(&self) -> Result<HashMap<String, String>>;

    fn upsert_docs(&self, docs: &[&ExtractedDoc], chunks: &[&Chunk]) -> Result<()>;

    fn delete_docs_by_ids(&self, doc_ids: &[&str]) -> Result<()>;

    fn ensure_document_node(&self, doc_id: &str) -> Result<()>;

    // ── Source management ────────────────────────────────────────────────

    fn upsert_source(
        &self,
        id: &str,
        source_type: &str,
        base_url: &str,
        space_key: &str,
    ) -> Result<()>;

    fn link_doc_to_source(&self, doc_id: &str, source_id: &str) -> Result<()>;

    fn get_docs_by_source(&self, source_id: &str) -> Result<Vec<String>>;

    // ── Links ────────────────────────────────────────────────────────────

    fn create_link(
        &self,
        from_doc_id: &str,
        to_doc_id: &str,
        url: &str,
        link_type: &str,
    ) -> Result<()>;

    fn delete_links_from(&self, doc_id: &str) -> Result<()>;

    // ── Chunks ───────────────────────────────────────────────────────────

    fn get_all_chunks(&self) -> Result<Vec<(String, String)>>;

    fn get_chunk_ids(&self) -> Result<std::collections::HashSet<String>>;

    fn get_chunk_details(&self, chunk_ids: &[&str]) -> Result<Vec<ChunkDetail>>;

    // ── Stats ────────────────────────────────────────────────────────────

    fn stats(&self) -> Result<DocStoreStats>;

    // ── Pipeline CRUD ────────────────────────────────────────────────────

    fn ensure_plugin_table(&self, plugin_id: &str, columns: &[(String, String)]) -> Result<()>;

    fn upsert_pipeline_core(&self, record: &PipelineCoreRecord) -> Result<()>;

    fn upsert_plugin_properties(
        &self,
        pipeline_id: &str,
        plugin_id: &str,
        properties: &serde_json::Map<String, serde_json::Value>,
        schema: &[(String, String)],
    ) -> Result<()>;

    fn link_pipeline_core_to_doc(&self, pipeline_id: &str, doc_id: &str) -> Result<()>;

    fn link_pipeline_dependencies(&self) -> Result<usize>;

    fn get_all_pipeline_cores(&self, plugin_id: Option<&str>) -> Result<Vec<PipelineCoreRecord>>;

    fn get_pipeline_core(&self, pipeline_id: &str) -> Result<Option<PipelineCoreRecord>>;

    fn impact_analysis(&self, table_name: &str, max_depth: u32) -> Result<Vec<ImpactResult>>;

    fn get_pipeline_deps(&self) -> Result<Vec<(String, String, String)>>;

    fn query_plugin_table(
        &self,
        plugin_id: &str,
        field: &str,
        value: &str,
    ) -> Result<Vec<serde_json::Value>>;

    fn pipeline_core_count(&self) -> Result<usize>;
}
