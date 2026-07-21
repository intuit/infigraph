mod backend;
pub mod cozo_store;
mod kuzu_backend;
#[cfg(feature = "neo4j")]
mod neo4j_backend;
pub mod parquet_loader;
mod queries;
mod schema;
mod session_store;
pub mod store;
mod store_bench;
mod store_bulk;
mod store_parquet;
pub(crate) mod store_util;
mod store_write;
pub mod test_templates;

pub use backend::GraphBackend;
pub use cozo_store::CozoStore;
pub use kuzu_backend::KuzuBackend;
#[cfg(feature = "neo4j")]
pub use neo4j_backend::Neo4jBackend;
pub use queries::{
    format_skeleton, ApiSymbol, ArchitectureStats, BranchInfo, ComplexityRow, CoverageRow,
    DeadCodeRow, ExampleTest, FileDeps, FileHotspot, GraphQuery, HierarchyNode, HubFunction,
    ImpactRow, KindCount, LanguageCount, ReferenceRow, SkeletonSymbol, SymbolDetail, SymbolMeta,
    SymbolRow, SymbolWithDocstring, TestContext, TestCoverage, TestTarget, TypeHierarchy,
};
pub use session_store::{SessionData, SessionStore};
pub use store::{GraphStats, GraphStore};
pub use test_templates::{test_templates_for, TestTemplate};

pub fn schema_ddl() -> Vec<&'static str> {
    let mut all: Vec<&str> = schema::CREATE_SCHEMA.to_vec();
    all.extend_from_slice(schema::MIGRATIONS);
    all
}

pub fn cozo_schema_ddl() -> Vec<&'static str> {
    cozo_store::cozo_schema_ddl()
}
