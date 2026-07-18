#![cfg(feature = "neo4j")]

use std::collections::HashMap;

use anyhow::{Context, Result};
use neo4rs::{query, Graph, Query};
use tokio::runtime::Handle;

use crate::learned::LearnedStore;
use crate::model::FileExtraction;
use crate::resolve::ResolveStats;

use super::backend::GraphBackend;
use super::{
    ApiSymbol, BranchInfo, FileDeps, GraphStats, ImpactRow, ReferenceRow, SymbolDetail, SymbolRow,
    TestContext, TestCoverage, TypeHierarchy,
};

const BATCH_SIZE: usize = 1000;

/// Neo4j-backed graph storage (remote, sidecar mode).
///
/// Connects to a Neo4j Community sidecar via Bolt protocol.
/// All trait methods are sync — async neo4rs calls are bridged via `block_on`.
/// Supports concurrent writes (no single-writer bottleneck).
pub struct Neo4jBackend {
    graph: Graph,
    handle: Handle,
}

impl Neo4jBackend {
    /// Connect to Neo4j at the given Bolt URI.
    /// Defaults: `bolt://localhost:7687`, user `neo4j`, password `infigraph`.
    pub fn connect(uri: &str, user: &str, password: &str) -> Result<Self> {
        let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let graph = rt.block_on(async {
            Graph::new(uri, user, password)
                .await
                .map_err(|e| anyhow::anyhow!("neo4j connect failed: {e}"))
        })?;
        let handle = rt.handle().clone();
        std::mem::forget(rt);
        Ok(Self { graph, handle })
    }

    /// Connect using environment variables.
    /// `NEO4J_URI` (default `127.0.0.1:7687`), `NEO4J_USER` (default `neo4j`),
    /// `NEO4J_PASSWORD` (default `infigraph`).
    pub fn connect_from_env() -> Result<Self> {
        let uri = std::env::var("NEO4J_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string());
        let user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
        let password = std::env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "infigraph".to_string());
        Self::connect(&uri, &user, &password)
    }

    /// Initialize schema constraints (idempotent).
    pub fn init_schema(&self) -> Result<()> {
        self.run_void(
            "CREATE CONSTRAINT symbol_id IF NOT EXISTS FOR (s:Symbol) REQUIRE s.id IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT file_id IF NOT EXISTS FOR (f:File) REQUIRE f.id IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT module_file IF NOT EXISTS FOR (m:Module) REQUIRE m.file IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT folder_id IF NOT EXISTS FOR (d:Folder) REQUIRE d.id IS UNIQUE",
        )?;
        self.run_void("CREATE INDEX symbol_file IF NOT EXISTS FOR (s:Symbol) ON (s.file)")?;
        self.run_void("CREATE INDEX symbol_name IF NOT EXISTS FOR (s:Symbol) ON (s.name)")?;
        Ok(())
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.handle.block_on(f)
    }

    fn run_void(&self, cypher: &str) -> Result<()> {
        self.block_on(async {
            self.graph
                .run(query(cypher))
                .await
                .map_err(|e| anyhow::anyhow!("neo4j query failed: {e}"))
        })
    }

    fn run_query(&self, q: Query) -> Result<Vec<neo4rs::Row>> {
        self.block_on(async {
            let mut stream = self
                .graph
                .execute(q)
                .await
                .map_err(|e| anyhow::anyhow!("neo4j execute failed: {e}"))?;
            let mut rows = Vec::new();
            while let Ok(Some(row)) = stream.next().await {
                rows.push(row);
            }
            Ok(rows)
        })
    }

    fn collect_strings(&self, cypher: &str, key: &str) -> Result<Vec<String>> {
        let rows = self.run_query(query(cypher))?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>(key).ok())
            .collect())
    }

    fn count_query(&self, cypher: &str, key: &str) -> Result<u64> {
        let rows = self.run_query(query(cypher))?;
        if let Some(row) = rows.first() {
            Ok(row.get::<i64>(key).unwrap_or(0) as u64)
        } else {
            Ok(0)
        }
    }

    fn delete_files_data(&self, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        for chunk in files.chunks(BATCH_SIZE) {
            let file_list: Vec<String> = chunk.to_vec();
            let q = query(
                "UNWIND $files AS f \
                 MATCH (file:File {id: f})-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) \
                 DETACH DELETE st",
            )
            .param("files", file_list.clone());
            let _ = self.block_on(self.graph.run(q));

            let q = query("UNWIND $files AS f MATCH (s:Symbol {file: f}) DETACH DELETE s")
                .param("files", file_list.clone());
            let _ = self.block_on(self.graph.run(q));

            let q = query("UNWIND $files AS f MATCH (m:Module {file: f}) DETACH DELETE m")
                .param("files", file_list.clone());
            let _ = self.block_on(self.graph.run(q));

            let q = query("UNWIND $files AS f MATCH (file:File {id: f}) DETACH DELETE file")
                .param("files", file_list);
            let _ = self.block_on(self.graph.run(q));
        }
        Ok(())
    }

    fn upsert_extraction(&self, ext: &FileExtraction) -> Result<()> {
        // File node
        self.block_on(
            self.graph.run(
                query("MERGE (f:File {id: $id}) SET f.language = $lang")
                    .param("id", ext.file.clone())
                    .param("lang", ext.language.clone()),
            ),
        )
        .map_err(|e| anyhow::anyhow!("upsert file failed: {e}"))?;

        // Module node
        self.block_on(
            self.graph.run(
                query(
                    "MERGE (m:Module {file: $file}) \
                 SET m.language = $lang, m.content_hash = $hash",
                )
                .param("file", ext.file.clone())
                .param("lang", ext.language.clone())
                .param("hash", ext.content_hash.clone()),
            ),
        )
        .map_err(|e| anyhow::anyhow!("upsert module failed: {e}"))?;

        // Symbols in batches
        for chunk in ext.symbols.chunks(BATCH_SIZE) {
            let params: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|s| {
                    let mut m = HashMap::new();
                    m.insert("id".into(), s.id.clone());
                    m.insert("name".into(), s.name.clone());
                    m.insert("kind".into(), format!("{:?}", s.kind));
                    m.insert("file".into(), s.span.file.clone());
                    m.insert("start_line".into(), s.span.start_line.to_string());
                    m.insert("end_line".into(), s.span.end_line.to_string());
                    m.insert(
                        "visibility".into(),
                        s.visibility.clone().unwrap_or_default(),
                    );
                    m.insert("signature_hash".into(), s.signature_hash.clone());
                    m.insert("complexity".into(), s.complexity.to_string());
                    m.insert("language".into(), s.language.clone());
                    m.insert(
                        "parameters".into(),
                        s.parameters.clone().unwrap_or_default(),
                    );
                    m.insert(
                        "return_type".into(),
                        s.return_type.clone().unwrap_or_default(),
                    );
                    m.insert("docstring".into(), s.docstring.clone().unwrap_or_default());
                    m.insert("parent".into(), s.parent.clone().unwrap_or_default());
                    m
                })
                .collect();

            self.block_on(self.graph.run(
                query(
                    "UNWIND $batch AS s \
                     MERGE (sym:Symbol {id: s.id}) \
                     SET sym.name = s.name, sym.kind = s.kind, sym.file = s.file, \
                         sym.start_line = toInteger(s.start_line), sym.end_line = toInteger(s.end_line), \
                         sym.visibility = s.visibility, sym.signature_hash = s.signature_hash, \
                         sym.complexity = toInteger(s.complexity), sym.language = s.language, \
                         sym.parameters = s.parameters, sym.return_type = s.return_type, \
                         sym.docstring = s.docstring, sym.parent = s.parent",
                )
                .param("batch", params),
            ))
            .map_err(|e| anyhow::anyhow!("upsert symbols failed: {e}"))?;
        }

        // DEFINES edges (File -> Symbol)
        if !ext.symbols.is_empty() {
            let sym_ids: Vec<String> = ext.symbols.iter().map(|s| s.id.clone()).collect();
            for chunk in sym_ids.chunks(BATCH_SIZE) {
                self.block_on(
                    self.graph.run(
                        query(
                            "UNWIND $ids AS sid \
                         MATCH (f:File {id: $file}), (s:Symbol {id: sid}) \
                         MERGE (f)-[:DEFINES]->(s)",
                        )
                        .param("file", ext.file.clone())
                        .param("ids", chunk.to_vec()),
                    ),
                )
                .map_err(|e| anyhow::anyhow!("upsert DEFINES failed: {e}"))?;
            }
        }

        // Statements
        for chunk in ext.statements.chunks(BATCH_SIZE) {
            let params: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|st| {
                    let mut m = HashMap::new();
                    m.insert("id".into(), st.id.clone());
                    m.insert("kind".into(), format!("{:?}", st.kind));
                    m.insert("condition".into(), st.condition.clone());
                    m.insert("start_line".into(), st.start_line.to_string());
                    m.insert("end_line".into(), st.end_line.to_string());
                    m.insert("depth".into(), st.depth.to_string());
                    m.insert("parent".into(), st.parent_symbol.clone());
                    m
                })
                .collect();

            self.block_on(self.graph.run(
                query(
                    "UNWIND $batch AS st \
                     MERGE (s:Statement {id: st.id}) \
                     SET s.kind = st.kind, s.condition = st.condition, \
                         s.start_line = toInteger(st.start_line), s.end_line = toInteger(st.end_line), \
                         s.depth = toInteger(st.depth) \
                     WITH s, st \
                     MATCH (sym:Symbol {id: st.parent}) \
                     MERGE (sym)-[:HAS_STATEMENT]->(s)",
                )
                .param("batch", params),
            ))
            .map_err(|e| anyhow::anyhow!("upsert statements failed: {e}"))?;
        }

        // IMPORTS edges
        let imports: Vec<(String, String)> = ext
            .relations
            .iter()
            .filter(|r| r.kind == crate::model::RelationKind::Imports)
            .map(|r| (r.source_id.clone(), r.target_id.clone()))
            .collect();
        for chunk in imports.chunks(BATCH_SIZE) {
            let pairs: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|(src, tgt)| {
                    let mut m = HashMap::new();
                    m.insert("src".into(), src.clone());
                    m.insert("tgt".into(), tgt.clone());
                    m
                })
                .collect();
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                     MERGE (a)-[:IMPORTS]->(b)",
                    )
                    .param("batch", pairs),
                ),
            );
        }

        // CALLS edges — both intra-file and cross-file (MATCH skips missing targets)
        let calls: Vec<(String, String)> = ext
            .relations
            .iter()
            .filter(|r| r.kind == crate::model::RelationKind::Calls)
            .map(|r| (r.source_id.clone(), r.target_id.clone()))
            .collect();
        for chunk in calls.chunks(BATCH_SIZE) {
            let pairs: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|(src, tgt)| {
                    let mut m = HashMap::new();
                    m.insert("src".into(), src.clone());
                    m.insert("tgt".into(), tgt.clone());
                    m
                })
                .collect();
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                     MERGE (a)-[:CALLS]->(b)",
                    )
                    .param("batch", pairs),
                ),
            );
        }

        Ok(())
    }

    fn upsert_folders(&self, file_paths: &[&str]) -> Result<()> {
        let mut folders: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut child_parent: Vec<(String, String)> = Vec::new();

        for file in file_paths {
            let parts: Vec<&str> = file.split('/').collect();
            for i in 1..parts.len() {
                let folder = parts[..i].join("/");
                folders.insert(folder);
            }
            if parts.len() > 1 {
                let parent = parts[..parts.len() - 1].join("/");
                child_parent.push((file.to_string(), parent));
            }
        }

        // Create folder nodes
        let folder_list: Vec<String> = folders.into_iter().collect();
        for chunk in folder_list.chunks(BATCH_SIZE) {
            self.block_on(
                self.graph.run(
                    query("UNWIND $folders AS f MERGE (d:Folder {id: f})")
                        .param("folders", chunk.to_vec()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("upsert folders failed: {e}"))?;
        }

        // CONTAINS edges (Folder -> File)
        for chunk in child_parent.chunks(BATCH_SIZE) {
            let pairs: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|(child, parent)| {
                    let mut m = HashMap::new();
                    m.insert("child".into(), child.clone());
                    m.insert("parent".into(), parent.clone());
                    m
                })
                .collect();
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (d:Folder {id: p.parent}), (f:File {id: p.child}) \
                     MERGE (d)-[:CONTAINS]->(f)",
                    )
                    .param("batch", pairs),
                ),
            );
        }

        Ok(())
    }
}

fn escape(s: &str) -> String {
    s.replace('\'', "\\'")
}

impl GraphBackend for Neo4jBackend {
    fn stats(&self) -> Result<GraphStats> {
        Ok(GraphStats {
            symbols: self.count_query("MATCH (s:Symbol) RETURN count(s) AS c", "c")?,
            modules: self.count_query("MATCH (m:Module) RETURN count(m) AS c", "c")?,
            files: self.count_query("MATCH (f:File) RETURN count(f) AS c", "c")?,
            folders: self.count_query("MATCH (d:Folder) RETURN count(d) AS c", "c")?,
            calls: self.count_query("MATCH ()-[r:CALLS]->() RETURN count(r) AS c", "c")?,
            inherits: self.count_query("MATCH ()-[r:INHERITS]->() RETURN count(r) AS c", "c")?,
            contains: self.count_query("MATCH ()-[r:CONTAINS]->() RETURN count(r) AS c", "c")?,
        })
    }

    fn get_file_hashes(&self) -> Result<HashMap<String, String>> {
        let rows = self.run_query(query(
            "MATCH (m:Module) RETURN m.file AS file, m.content_hash AS hash",
        ))?;
        let mut map = HashMap::new();
        for row in &rows {
            if let (Ok(file), Ok(hash)) = (row.get::<String>("file"), row.get::<String>("hash")) {
                map.insert(file, hash);
            }
        }
        Ok(map)
    }

    fn get_all_symbols(&self) -> Result<Vec<(String, String, String, String)>> {
        let rows = self.run_query(query(
            "MATCH (s:Symbol) RETURN s.name AS name, s.id AS id, s.file AS file, s.kind AS kind",
        ))?;
        let mut symbols = Vec::new();
        for row in &rows {
            if let (Ok(name), Ok(id), Ok(file), Ok(kind)) = (
                row.get::<String>("name"),
                row.get::<String>("id"),
                row.get::<String>("file"),
                row.get::<String>("kind"),
            ) {
                symbols.push((name, id, file, kind));
            }
        }
        Ok(symbols)
    }

    fn symbols_in_file(&self, file: &str) -> Result<Vec<SymbolRow>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {file: $file}) \
                 RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                        s.start_line AS start_line, s.end_line AS end_line \
                 ORDER BY s.start_line",
            )
            .param("file", file.to_string()),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(SymbolRow {
                    id: r.get::<String>("id").ok()?,
                    name: r.get::<String>("name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    start_line: r.get::<i64>("start_line").ok()? as u32,
                    end_line: r.get::<i64>("end_line").ok()? as u32,
                })
            })
            .collect())
    }

    fn find_symbol_by_id(&self, id: &str) -> Result<Option<SymbolDetail>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {id: $id}) \
                 RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                        s.file AS file, s.start_line AS start_line, s.end_line AS end_line",
            )
            .param("id", id.to_string()),
        )?;
        Ok(rows.first().and_then(|r| {
            Some(SymbolDetail {
                id: r.get::<String>("id").ok()?,
                name: r.get::<String>("name").ok()?,
                kind: r.get::<String>("kind").ok()?,
                file: r.get::<String>("file").ok()?,
                start_line: r.get::<i64>("start_line").ok()? as u32,
                end_line: r.get::<i64>("end_line").ok()? as u32,
            })
        }))
    }

    fn symbols_in_range(&self, file: &str, start: u32, end: u32) -> Result<Vec<SymbolDetail>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {file: $file}) \
                 WHERE s.start_line >= $start AND s.end_line <= $end \
                 RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                        s.file AS file, s.start_line AS start_line, s.end_line AS end_line \
                 ORDER BY s.start_line",
            )
            .param("file", file.to_string())
            .param("start", start as i64)
            .param("end", end as i64),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(SymbolDetail {
                    id: r.get::<String>("id").ok()?,
                    name: r.get::<String>("name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    file: r.get::<String>("file").ok()?,
                    start_line: r.get::<i64>("start_line").ok()? as u32,
                    end_line: r.get::<i64>("end_line").ok()? as u32,
                })
            })
            .collect())
    }

    fn skeleton(&self, file: &str) -> Result<String> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {file: $file}) \
                 OPTIONAL MATCH (caller)-[:CALLS]->(s) \
                 OPTIONAL MATCH (s)-[:HAS_STATEMENT]->(st:Statement) \
                 WITH s, count(DISTINCT caller) AS fan_in, count(DISTINCT st) AS stmt_count \
                 RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                        s.start_line AS start_line, s.complexity AS complexity, \
                        s.parameters AS params, s.return_type AS return_type, \
                        s.visibility AS visibility, s.parent AS parent, \
                        fan_in, stmt_count \
                 ORDER BY s.start_line",
            )
            .param("file", file.to_string()),
        )?;
        let symbols: Vec<super::queries::SkeletonSymbol> = rows
            .iter()
            .filter_map(|r| {
                Some(super::queries::SkeletonSymbol {
                    id: r.get::<String>("id").ok()?,
                    name: r.get::<String>("name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    start_line: r.get::<String>("start_line").ok().unwrap_or_default(),
                    complexity: r.get::<i64>("complexity").ok().unwrap_or(0) as u32,
                    params: r.get::<String>("params").ok().unwrap_or_default(),
                    return_type: r.get::<String>("return_type").ok().unwrap_or_default(),
                    visibility: r.get::<String>("visibility").ok().unwrap_or_default(),
                    parent: r.get::<String>("parent").ok().unwrap_or_default(),
                    fan_in: r.get::<i64>("fan_in").ok().unwrap_or(0) as usize,
                    stmt_count: r.get::<i64>("stmt_count").ok().unwrap_or(0) as usize,
                    nesting: 0,
                })
            })
            .collect();
        Ok(super::queries::format_skeleton(file, &symbols))
    }

    fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        self.collect_strings(
            &format!(
                "MATCH (caller:Symbol)-[:CALLS]->(s:Symbol {{id: '{}'}}) RETURN caller.id AS id",
                escape(symbol_id)
            ),
            "id",
        )
    }

    fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        self.collect_strings(
            &format!(
                "MATCH (s:Symbol {{id: '{}'}})-[:CALLS]->(callee:Symbol) RETURN callee.id AS id",
                escape(symbol_id)
            ),
            "id",
        )
    }

    fn branches_of(&self, symbol_id: &str) -> Result<Vec<BranchInfo>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {id: $id})-[:HAS_STATEMENT]->(st:Statement) \
                 RETURN st.kind AS kind, st.condition AS condition, \
                        st.start_line AS line, st.depth AS depth \
                 ORDER BY st.start_line",
            )
            .param("id", symbol_id.to_string()),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(BranchInfo {
                    kind: r.get::<String>("kind").ok()?,
                    condition: r.get::<String>("condition").ok()?,
                    line: r.get::<i64>("line").ok()? as u32,
                    depth: r.get::<i64>("depth").ok()? as u32,
                })
            })
            .collect())
    }

    fn transitive_impact(&self, id: &str, max_depth: u32) -> Result<Vec<ImpactRow>> {
        let rows = self.run_query(
            query(&format!(
                "MATCH (s:Symbol {{id: $id}})<-[:CALLS*1..{}]-(caller:Symbol) \
                 RETURN DISTINCT caller.id AS id, caller.name AS name, \
                        caller.file AS file, caller.kind AS kind",
                max_depth
            ))
            .param("id", id.to_string()),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(ImpactRow {
                    id: r.get::<String>("id").ok()?,
                    name: r.get::<String>("name").ok()?,
                    file: r.get::<String>("file").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                })
            })
            .collect())
    }

    fn find_all_references(&self, id: &str) -> Result<Vec<ReferenceRow>> {
        let rows = self.run_query(
            query(
                "MATCH (ref:Symbol)-[r:CALLS|IMPORTS|INHERITS]->(target:Symbol {id: $id}) \
                 RETURN ref.id AS caller_id, ref.name AS caller_name, \
                        ref.file AS file, ref.start_line AS line, target.id AS target_id",
            )
            .param("id", id.to_string()),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(ReferenceRow {
                    caller_id: r.get::<String>("caller_id").ok()?,
                    caller_name: r.get::<String>("caller_name").ok()?,
                    file: r.get::<String>("file").ok()?,
                    line: r.get::<i64>("line").ok().unwrap_or(0) as u32,
                    target_id: r.get::<String>("target_id").ok()?,
                })
            })
            .collect())
    }

    fn cross_cutting_for(&self, id: &str) -> Result<Vec<(String, String)>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {id: $id})<-[:CALLS]-(caller:Symbol) \
                 WHERE caller.file <> s.file \
                 RETURN DISTINCT caller.file AS file, caller.name AS name",
            )
            .param("id", id.to_string()),
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| Some((r.get::<String>("file").ok()?, r.get::<String>("name").ok()?)))
            .collect())
    }

    fn get_api_surface(&self) -> Result<Vec<ApiSymbol>> {
        let rows = self.run_query(query(
            "MATCH (s:Symbol) WHERE s.visibility = 'public' \
             RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                    s.file AS file, s.start_line AS line, \
                    s.visibility AS visibility, s.docstring AS docstring \
             ORDER BY s.file, s.start_line",
        ))?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(ApiSymbol {
                    id: r.get::<String>("id").ok()?,
                    name: r.get::<String>("name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    file: r.get::<String>("file").ok()?,
                    line: r.get::<i64>("line").ok()? as u32,
                    visibility: r.get::<String>("visibility").ok().unwrap_or_default(),
                    docstring: r.get::<String>("docstring").ok().unwrap_or_default(),
                })
            })
            .collect())
    }

    fn get_file_deps(&self, file: &str) -> Result<FileDeps> {
        let imports = self.collect_strings(
            &format!(
                "MATCH (s:Symbol {{file: '{}'}})-[:IMPORTS]->(t:Symbol) \
                 RETURN DISTINCT t.file AS file",
                escape(file)
            ),
            "file",
        )?;
        let imported_by = self.collect_strings(
            &format!(
                "MATCH (s:Symbol)-[:IMPORTS]->(t:Symbol {{file: '{}'}}) \
                 RETURN DISTINCT s.file AS file",
                escape(file)
            ),
            "file",
        )?;
        Ok(FileDeps {
            file: file.to_string(),
            imports,
            imported_by,
        })
    }

    fn get_type_hierarchy(&self, id: &str, max_depth: u32) -> Result<TypeHierarchy> {
        let root = self.find_symbol_by_id(id)?.context("symbol not found")?;

        let ancestor_rows = self.run_query(
            query(&format!(
                "MATCH (s:Symbol {{id: $id}})-[:INHERITS*1..{}]->(anc:Symbol) \
                 RETURN DISTINCT anc.id AS id, anc.name AS name, anc.kind AS kind, anc.file AS file",
                max_depth
            ))
            .param("id", id.to_string()),
        )?;

        let descendant_rows = self.run_query(
            query(&format!(
                "MATCH (desc:Symbol)-[:INHERITS*1..{}]->(s:Symbol {{id: $id}}) \
                 RETURN DISTINCT desc.id AS id, desc.name AS name, desc.kind AS kind, desc.file AS file",
                max_depth
            ))
            .param("id", id.to_string()),
        )?;

        let to_node = |r: &neo4rs::Row| -> Option<super::HierarchyNode> {
            Some(super::HierarchyNode {
                id: r.get::<String>("id").ok()?,
                name: r.get::<String>("name").ok()?,
                kind: r.get::<String>("kind").ok()?,
                file: r.get::<String>("file").ok()?,
            })
        };

        Ok(TypeHierarchy {
            root_id: root.id,
            root_name: root.name,
            ancestors: ancestor_rows.iter().filter_map(to_node).collect(),
            descendants: descendant_rows.iter().filter_map(to_node).collect(),
        })
    }

    fn get_test_coverage(&self) -> Result<TestCoverage> {
        let covered_rows = self.run_query(query(
            "MATCH (s:Symbol)<-[:TESTED_BY]-(t:Symbol) \
             RETURN s.id AS symbol_id, s.name AS symbol_name, s.kind AS kind, \
                    s.file AS file, t.id AS test_id",
        ))?;

        let uncovered_rows = self.run_query(query(
            "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
             AND NOT (s)<-[:TESTED_BY]-() \
             RETURN s.id AS symbol_id, s.name AS symbol_name, s.kind AS kind, s.file AS file",
        ))?;

        let covered: Vec<super::CoverageRow> = covered_rows
            .iter()
            .filter_map(|r| {
                Some(super::CoverageRow {
                    symbol_id: r.get::<String>("symbol_id").ok()?,
                    symbol_name: r.get::<String>("symbol_name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    file: r.get::<String>("file").ok()?,
                    test_id: r.get::<String>("test_id").ok(),
                })
            })
            .collect();

        let uncovered: Vec<super::CoverageRow> = uncovered_rows
            .iter()
            .filter_map(|r| {
                Some(super::CoverageRow {
                    symbol_id: r.get::<String>("symbol_id").ok()?,
                    symbol_name: r.get::<String>("symbol_name").ok()?,
                    kind: r.get::<String>("kind").ok()?,
                    file: r.get::<String>("file").ok()?,
                    test_id: None,
                })
            })
            .collect();

        let total = covered.len() + uncovered.len();
        let pct = if total > 0 {
            covered.len() * 100 / total
        } else {
            0
        };

        Ok(TestCoverage {
            covered_count: covered.len(),
            uncovered_count: uncovered.len(),
            coverage_pct: pct,
            covered,
            uncovered,
        })
    }

    fn generate_test_context(
        &self,
        _file_filter: Option<&str>,
        _limit: usize,
        _test_type: Option<&str>,
    ) -> Result<TestContext> {
        // Stub — complex query involving templates, not critical for sidecar MVP
        Ok(TestContext {
            framework: "unknown".to_string(),
            example_test: None,
            targets: vec![],
            templates: vec![],
        })
    }

    fn raw_query(&self, cypher: &str) -> Result<Vec<Vec<String>>> {
        let rows = self.run_query(query(cypher))?;
        Ok(rows
            .iter()
            .map(|r| {
                // Neo4j rows are key-value; extract all values as strings
                // This is a best-effort conversion for raw queries
                let bolt_map: HashMap<String, String> = r.to().unwrap_or_default();
                bolt_map.into_values().collect()
            })
            .collect())
    }

    fn get_symbols_for_search(&self) -> Result<Vec<Vec<String>>> {
        let rows = self.run_query(query(
            "MATCH (s:Symbol) \
             RETURN s.id AS id, s.name AS name, s.kind AS kind, \
                    s.file AS file, s.docstring AS docstring, \
                    s.start_line AS start_line, s.end_line AS end_line",
        ))?;
        Ok(rows
            .iter()
            .map(|r| {
                let id: String = r.get("id").unwrap_or_default();
                let name: String = r.get("name").unwrap_or_default();
                let kind: String = r.get("kind").unwrap_or_default();
                let file: String = r.get("file").unwrap_or_default();
                let docstring: String = r.get("docstring").unwrap_or_default();
                let start_line: String = r
                    .get::<i64>("start_line")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let end_line: String = r
                    .get::<i64>("end_line")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                vec![id, name, kind, file, docstring, start_line, end_line]
            })
            .collect())
    }

    fn upsert_file(&self, extraction: &FileExtraction) -> Result<()> {
        self.delete_files_data(&[extraction.file.clone()])?;
        self.upsert_extraction(extraction)
    }

    fn upsert_files_bulk(
        &self,
        extractions: &[FileExtraction],
        existing_hashes_empty: bool,
    ) -> Result<()> {
        if extractions.is_empty() {
            return Ok(());
        }

        if !existing_hashes_empty {
            let files: Vec<String> = extractions.iter().map(|e| e.file.clone()).collect();
            self.delete_files_data(&files)?;
        }

        for ext in extractions {
            self.upsert_extraction(ext)?;
        }

        let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
        self.upsert_folders(&file_paths)?;

        Ok(())
    }

    fn remove_file(&self, file: &str) -> Result<()> {
        self.delete_files_data(&[file.to_string()])
    }

    fn derive_tested_by_edges(&self) -> Result<usize> {
        // Match test files (files containing "test" in path) calling non-test symbols
        self.run_void(
            "MATCH (test:Symbol)-[:CALLS]->(target:Symbol) \
             WHERE test.file CONTAINS 'test' AND NOT target.file CONTAINS 'test' \
             MERGE (target)<-[:TESTED_BY]-(test)",
        )?;
        let count = self.count_query("MATCH ()-[r:TESTED_BY]->() RETURN count(r) AS c", "c")?;
        Ok(count as usize)
    }

    fn resolve_calls(
        &self,
        extractions: &[FileExtraction],
        _learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats> {
        if extractions.is_empty() {
            return Ok(ResolveStats {
                total_calls: 0,
                resolved: 0,
                unresolved: 0,
                learned_resolved: 0,
                inherits_resolved: 0,
            });
        }

        // Build symbol map from graph
        let all_symbols = self.get_all_symbols()?;
        let mut symbol_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for (name, id, file, kind) in &all_symbols {
            symbol_map.entry(name.clone()).or_default().push((
                id.clone(),
                file.clone(),
                kind.clone(),
            ));
        }

        let mut resolved = 0usize;
        let mut unresolved = 0usize;
        let mut total_dangling = 0usize;
        let mut pairs: Vec<(String, String)> = Vec::new();

        for ext in extractions {
            let local_symbols: HashMap<&str, &str> = ext
                .symbols
                .iter()
                .map(|s| (s.name.as_str(), s.id.as_str()))
                .collect();

            for rel in &ext.relations {
                if rel.kind != crate::model::RelationKind::Calls {
                    continue;
                }
                let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);
                if local_symbols.contains_key(target_name) {
                    continue;
                }
                total_dangling += 1;

                if let Some(candidates) = symbol_map.get(target_name) {
                    let cross_file: Vec<_> = candidates
                        .iter()
                        .filter(|(_, f, _)| *f != ext.file)
                        .collect();
                    if cross_file.len() == 1 {
                        pairs.push((rel.source_id.clone(), cross_file[0].0.clone()));
                        resolved += 1;
                    } else if cross_file.len() > 1 {
                        // Pick shortest ID as tiebreaker
                        if let Some(best) = cross_file.iter().min_by_key(|(id, _, _)| id.len()) {
                            pairs.push((rel.source_id.clone(), best.0.clone()));
                            resolved += 1;
                        } else {
                            unresolved += 1;
                        }
                    } else {
                        unresolved += 1;
                    }
                } else {
                    unresolved += 1;
                }
            }
        }

        // Batch insert CALLS edges
        for chunk in pairs.chunks(BATCH_SIZE) {
            let batch: Vec<HashMap<String, String>> = chunk
                .iter()
                .map(|(src, tgt)| {
                    let mut m = HashMap::new();
                    m.insert("src".into(), src.clone());
                    m.insert("tgt".into(), tgt.clone());
                    m
                })
                .collect();
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                     MERGE (a)-[:CALLS]->(b)",
                    )
                    .param("batch", batch),
                ),
            );
        }

        // Resolve INHERITS edges
        let mut inherits_resolved = 0usize;
        for ext in extractions {
            let inherits: Vec<_> = ext
                .relations
                .iter()
                .filter(|r| r.kind == crate::model::RelationKind::Inherits)
                .collect();
            for rel in inherits {
                let target_name = rel.target_id.rsplit("::").next().unwrap_or(&rel.target_id);
                if let Some(candidates) = symbol_map.get(target_name) {
                    let cross_file: Vec<_> = candidates
                        .iter()
                        .filter(|(_, f, _)| *f != ext.file)
                        .collect();
                    if let Some(best) = cross_file.first() {
                        let batch = vec![{
                            let mut m: HashMap<String, String> = HashMap::new();
                            m.insert("src".into(), rel.source_id.clone());
                            m.insert("tgt".into(), best.0.clone());
                            m
                        }];
                        let _ = self.block_on(
                            self.graph.run(
                                query(
                                    "UNWIND $batch AS p \
                                 MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                                 MERGE (a)-[:INHERITS]->(b)",
                                )
                                .param("batch", batch),
                            ),
                        );
                        inherits_resolved += 1;
                    }
                }
            }
        }

        Ok(ResolveStats {
            total_calls: total_dangling,
            resolved,
            unresolved,
            learned_resolved: 0,
            inherits_resolved,
        })
    }
}
