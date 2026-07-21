#![cfg(feature = "neo4j")]

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use neo4rs::{query, Graph, Query};
use tokio::runtime::Handle;

use crate::learned::LearnedStore;
use crate::model::FileExtraction;
use crate::resolve::ResolveStats;

use super::backend::GraphBackend;
use super::{
    ApiSymbol, ArchitectureStats, BranchInfo, ComplexityRow, DeadCodeRow, FileDeps, FileHotspot,
    GraphStats, HubFunction, ImpactRow, KindCount, LanguageCount, ReferenceRow, SymbolDetail,
    SymbolMeta, SymbolRow, SymbolWithDocstring, TestContext, TestCoverage, TypeHierarchy,
};

const BATCH_SIZE: usize = 1000;

static NEO4J_CONN: OnceLock<(Graph, Handle)> = OnceLock::new();

fn get_or_init_connection(uri: &str, user: &str, password: &str) -> Result<(Graph, Handle)> {
    if let Some((g, h)) = NEO4J_CONN.get() {
        return Ok((g.clone(), h.clone()));
    }
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let graph = rt.block_on(async {
        Graph::new(uri, user, password)
            .await
            .map_err(|e| anyhow::anyhow!("neo4j connect failed: {e}"))
    })?;
    let handle = rt.handle().clone();
    std::mem::forget(rt);
    let _ = NEO4J_CONN.set((graph.clone(), handle.clone()));
    Ok((graph, handle))
}

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
        let (graph, handle) = get_or_init_connection(uri, user, password)?;
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
        self.run_void(
            "CREATE CONSTRAINT repo_name IF NOT EXISTS FOR (r:Repo) REQUIRE r.name IS UNIQUE",
        )?;
        self.run_void("CREATE INDEX symbol_file IF NOT EXISTS FOR (s:Symbol) ON (s.file)")?;
        self.run_void("CREATE INDEX symbol_name IF NOT EXISTS FOR (s:Symbol) ON (s.name)")?;
        self.run_void("CREATE INDEX file_repo IF NOT EXISTS FOR (f:File) ON (f.repo)")?;
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

fn bolt_get_string(row: &neo4rs::Row, key: &str) -> String {
    if let Ok(s) = row.get::<String>(key) {
        return s;
    }
    if let Ok(n) = row.get::<i64>(key) {
        return n.to_string();
    }
    if let Ok(f) = row.get::<f64>(key) {
        return f.to_string();
    }
    if let Ok(b) = row.get::<bool>(key) {
        return b.to_string();
    }
    String::new()
}

fn parse_return_columns(cypher: &str) -> Vec<String> {
    let upper = cypher.to_uppercase();
    let return_pos = match upper.rfind("RETURN ") {
        Some(pos) => pos + 7,
        None => return Vec::new(),
    };
    let after_return = &cypher[return_pos..];
    let end = ["ORDER BY", "LIMIT", "SKIP", "UNION"]
        .iter()
        .filter_map(|kw| {
            let u = after_return.to_uppercase();
            u.find(kw).map(|p| p)
        })
        .min()
        .unwrap_or(after_return.len());
    let columns_str = after_return[..end].trim();
    let mut cols = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, ch) in columns_str.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                cols.push(extract_column_name(columns_str[start..i].trim()));
                start = i + 1;
            }
            _ => {}
        }
    }
    cols.push(extract_column_name(columns_str[start..].trim()));
    cols
}

fn extract_column_name(expr: &str) -> String {
    let upper = expr.to_uppercase();
    if let Some(pos) = upper.rfind(" AS ") {
        return expr[pos + 4..].trim().to_string();
    }
    if expr.starts_with("DISTINCT ") || expr.starts_with("distinct ") {
        return extract_column_name(&expr[9..]);
    }
    expr.trim().to_string()
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
        let keys = parse_return_columns(cypher);
        let rows = self.run_query(query(cypher))?;
        if keys.is_empty() {
            return Ok(rows.iter().map(|_| Vec::new()).collect());
        }
        Ok(rows
            .iter()
            .map(|r| keys.iter().map(|k| bolt_get_string(r, k)).collect())
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

    // ── Phase-2: backend-agnostic query methods ──────────────────────

    fn symbol_metadata(&self, id: &str) -> Result<Option<SymbolMeta>> {
        let rows = self.run_query(
            query(
                "MATCH (s:Symbol {id: $id}) \
             RETURN s.docstring AS docstring, s.complexity AS complexity",
            )
            .param("id", id),
        )?;
        if rows.is_empty() {
            return Ok(None);
        }
        let r = &rows[0];
        let docstring: String = r.get("docstring").unwrap_or_default();
        let complexity: u32 = r.get::<i64>("complexity").unwrap_or(0) as u32;

        let parent_rows = self.run_query(
            query(
                "MATCH (parent)-[:CONTAINS]->(s:Symbol {id: $id}) \
             RETURN parent.id AS pid, parent.name AS pname",
            )
            .param("id", id),
        )?;
        let (parent_id, parent_name) = if let Some(pr) = parent_rows.first() {
            (
                Some(pr.get::<String>("pid").unwrap_or_default()),
                Some(pr.get::<String>("pname").unwrap_or_default()),
            )
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
        let cypher = if file_filter.is_some() {
            "MATCH (s:Symbol) \
             WHERE s.kind IN ['Function', 'Method', 'Test'] AND s.file CONTAINS $file \
             RETURN s.name AS name, s.file AS file, s.start_line AS start_line, s.complexity AS complexity \
             ORDER BY s.complexity DESC"
        } else {
            "MATCH (s:Symbol) \
             WHERE s.kind IN ['Function', 'Method', 'Test'] \
             RETURN s.name AS name, s.file AS file, s.start_line AS start_line, s.complexity AS complexity \
             ORDER BY s.complexity DESC"
        };
        let q = if let Some(f) = file_filter {
            query(cypher).param("file", f)
        } else {
            query(cypher)
        };
        let rows = self.run_query(q)?;
        Ok(rows
            .iter()
            .map(|r| ComplexityRow {
                name: r.get("name").unwrap_or_default(),
                file: r.get("file").unwrap_or_default(),
                start_line: r.get::<i64>("start_line").unwrap_or(0) as u32,
                complexity: r.get::<i64>("complexity").unwrap_or(0) as u32,
            })
            .collect())
    }

    fn list_indexed_files(&self) -> Result<Vec<String>> {
        self.collect_strings(
            "MATCH (s:Symbol) RETURN DISTINCT s.file AS f ORDER BY f",
            "f",
        )
    }

    fn find_uncalled_symbols(&self) -> Result<Vec<DeadCodeRow>> {
        let rows = self.run_query(query(
            "MATCH (s:Symbol) \
             WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } \
             RETURN s.name AS name, s.kind AS kind, s.file AS file \
             ORDER BY s.file, s.name",
        ))?;
        Ok(rows
            .iter()
            .map(|r| DeadCodeRow {
                name: r.get("name").unwrap_or_default(),
                kind: r.get("kind").unwrap_or_default(),
                file: r.get("file").unwrap_or_default(),
            })
            .collect())
    }

    fn get_architecture_stats(&self) -> Result<ArchitectureStats> {
        let lang_rows = self.run_query(query(
            "MATCH (m:Module) RETURN m.language AS lang, count(m) AS cnt ORDER BY cnt DESC",
        ))?;
        let languages: Vec<LanguageCount> = lang_rows
            .iter()
            .map(|r| LanguageCount {
                language: r.get("lang").unwrap_or_default(),
                count: r.get::<i64>("cnt").unwrap_or(0) as u64,
            })
            .collect();

        let kind_rows = self.run_query(query(
            "MATCH (s:Symbol) RETURN s.kind AS kind, count(s) AS cnt ORDER BY cnt DESC",
        ))?;
        let kind_counts: Vec<KindCount> = kind_rows
            .iter()
            .map(|r| KindCount {
                kind: r.get("kind").unwrap_or_default(),
                count: r.get::<i64>("cnt").unwrap_or(0) as u64,
            })
            .collect();

        let hotspot_rows = self.run_query(query(
            "MATCH (s:Symbol) RETURN s.file AS file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10",
        ))?;
        let hotspot_files: Vec<FileHotspot> = hotspot_rows
            .iter()
            .map(|r| FileHotspot {
                file: r.get("file").unwrap_or_default(),
                count: r.get::<i64>("cnt").unwrap_or(0) as u64,
            })
            .collect();

        let hub_rows = self.run_query(query(
            "MATCH ()-[r:CALLS]->(s:Symbol) \
             RETURN s.name AS name, s.file AS file, count(r) AS calls \
             ORDER BY calls DESC LIMIT 10",
        ))?;
        let hub_functions: Vec<HubFunction> = hub_rows
            .iter()
            .map(|r| HubFunction {
                name: r.get("name").unwrap_or_default(),
                file: r.get("file").unwrap_or_default(),
                calls: r.get::<i64>("calls").unwrap_or(0) as u64,
            })
            .collect();

        let entry_rows = self.run_query(query(
            "MATCH (s:Symbol)-[:CALLS]->() \
             WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } \
             RETURN DISTINCT s.name AS name, s.kind AS kind, s.file AS file \
             ORDER BY s.file, s.name LIMIT 20",
        ))?;
        let entry_points: Vec<DeadCodeRow> = entry_rows
            .iter()
            .map(|r| DeadCodeRow {
                name: r.get("name").unwrap_or_default(),
                kind: r.get("kind").unwrap_or_default(),
                file: r.get("file").unwrap_or_default(),
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
        let cypher = if let Some(kinds) = kind_filter {
            let k_list: Vec<String> = kinds.iter().map(|k| format!("'{}'", escape(k))).collect();
            format!(
                "MATCH (s:Symbol) WHERE s.kind IN [{}] \
                 RETURN s.id AS id, s.name AS name, s.kind AS kind, s.file AS file, s.docstring AS docstring",
                k_list.join(", ")
            )
        } else {
            "MATCH (s:Symbol) \
             RETURN s.id AS id, s.name AS name, s.kind AS kind, s.file AS file, s.docstring AS docstring"
                .to_string()
        };
        let rows = self.run_query(query(&cypher))?;
        Ok(rows
            .iter()
            .map(|r| SymbolWithDocstring {
                id: r.get("id").unwrap_or_default(),
                name: r.get("name").unwrap_or_default(),
                kind: r.get("kind").unwrap_or_default(),
                file: r.get("file").unwrap_or_default(),
                docstring: r.get("docstring").unwrap_or_default(),
            })
            .collect())
    }

    fn upsert_similar_edge(&self, id_a: &str, id_b: &str, score: f32) -> Result<()> {
        self.block_on(
            self.graph.run(
                query(
                    "MATCH (a:Symbol {id: $ida}), (b:Symbol {id: $idb}) \
                 MERGE (a)-[r:SIMILAR_TO]->(b) SET r.score = $score",
                )
                .param("ida", id_a)
                .param("idb", id_b)
                .param("score", score as f64),
            ),
        )
        .map_err(|e| anyhow::anyhow!("upsert_similar_edge failed: {e}"))?;
        Ok(())
    }

    // ── Write ────────────────────────────────────────────────────────

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

        // ── Phase 1: All nodes (File, Module, Symbol, Statement) ─────

        // File nodes — one batch per chunk
        let file_params: Vec<HashMap<String, String>> = extractions
            .iter()
            .map(|ext| {
                let mut m = HashMap::new();
                m.insert("id".into(), ext.file.clone());
                m.insert("lang".into(), ext.language.clone());
                m
            })
            .collect();
        for chunk in file_params.chunks(BATCH_SIZE) {
            self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS f \
                     MERGE (file:File {id: f.id}) SET file.language = f.lang",
                    )
                    .param("batch", chunk.to_vec()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("bulk upsert files failed: {e}"))?;
        }

        // Module nodes
        let module_params: Vec<HashMap<String, String>> = extractions
            .iter()
            .map(|ext| {
                let mut m = HashMap::new();
                m.insert("file".into(), ext.file.clone());
                m.insert("lang".into(), ext.language.clone());
                m.insert("hash".into(), ext.content_hash.clone());
                m
            })
            .collect();
        for chunk in module_params.chunks(BATCH_SIZE) {
            self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS m \
                     MERGE (mod:Module {file: m.file}) \
                     SET mod.language = m.lang, mod.content_hash = m.hash",
                    )
                    .param("batch", chunk.to_vec()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("bulk upsert modules failed: {e}"))?;
        }

        // Symbol nodes — collect across all files
        let all_symbols: Vec<HashMap<String, String>> = extractions
            .iter()
            .flat_map(|ext| {
                ext.symbols.iter().map(|s| {
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
            })
            .collect();
        for chunk in all_symbols.chunks(BATCH_SIZE) {
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
                .param("batch", chunk.to_vec()),
            ))
            .map_err(|e| anyhow::anyhow!("bulk upsert symbols failed: {e}"))?;
        }

        // Statement nodes + HAS_STATEMENT edges (fused — symbol nodes already exist)
        let all_statements: Vec<HashMap<String, String>> = extractions
            .iter()
            .flat_map(|ext| {
                ext.statements.iter().map(|st| {
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
            })
            .collect();
        for chunk in all_statements.chunks(BATCH_SIZE) {
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
                .param("batch", chunk.to_vec()),
            ))
            .map_err(|e| anyhow::anyhow!("bulk upsert statements failed: {e}"))?;
        }

        // ── Phase 2: Edges (DEFINES, IMPORTS, CALLS) ─────────────────

        // DEFINES edges (File -> Symbol)
        let all_defines: Vec<HashMap<String, String>> = extractions
            .iter()
            .flat_map(|ext| {
                ext.symbols.iter().map(|s| {
                    let mut m = HashMap::new();
                    m.insert("file".into(), ext.file.clone());
                    m.insert("sym".into(), s.id.clone());
                    m
                })
            })
            .collect();
        for chunk in all_defines.chunks(BATCH_SIZE) {
            self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS d \
                     MATCH (f:File {id: d.file}), (s:Symbol {id: d.sym}) \
                     MERGE (f)-[:DEFINES]->(s)",
                    )
                    .param("batch", chunk.to_vec()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("bulk upsert DEFINES failed: {e}"))?;
        }

        // IMPORTS edges
        let all_imports: Vec<HashMap<String, String>> = extractions
            .iter()
            .flat_map(|ext| {
                ext.relations
                    .iter()
                    .filter(|r| r.kind == crate::model::RelationKind::Imports)
                    .map(|r| {
                        let mut m = HashMap::new();
                        m.insert("src".into(), r.source_id.clone());
                        m.insert("tgt".into(), r.target_id.clone());
                        m
                    })
            })
            .collect();
        for chunk in all_imports.chunks(BATCH_SIZE) {
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                     MERGE (a)-[:IMPORTS]->(b)",
                    )
                    .param("batch", chunk.to_vec()),
                ),
            );
        }

        // CALLS edges
        let all_calls: Vec<HashMap<String, String>> = extractions
            .iter()
            .flat_map(|ext| {
                ext.relations
                    .iter()
                    .filter(|r| r.kind == crate::model::RelationKind::Calls)
                    .map(|r| {
                        let mut m = HashMap::new();
                        m.insert("src".into(), r.source_id.clone());
                        m.insert("tgt".into(), r.target_id.clone());
                        m
                    })
            })
            .collect();
        for chunk in all_calls.chunks(BATCH_SIZE) {
            let _ = self.block_on(
                self.graph.run(
                    query(
                        "UNWIND $batch AS p \
                     MATCH (a:Symbol {id: p.src}), (b:Symbol {id: p.tgt}) \
                     MERGE (a)-[:CALLS]->(b)",
                    )
                    .param("batch", chunk.to_vec()),
                ),
            );
        }

        let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
        self.upsert_folders(&file_paths)?;

        Ok(())
    }

    fn remove_file(&self, file: &str) -> Result<()> {
        self.delete_files_data(&[file.to_string()])
    }

    fn derive_tested_by_edges(&self, changed_files: Option<&[&str]>) -> Result<usize> {
        match changed_files {
            Some(files) if !files.is_empty() => {
                let file_list: Vec<String> = files.iter().map(|f| f.to_string()).collect();
                self.block_on(
                    self.graph.run(
                        query(
                            "MATCH (test:Symbol)-[:CALLS]->(target:Symbol) \
                         WHERE (test.file IN $files OR target.file IN $files) \
                           AND test.file CONTAINS 'test' \
                           AND NOT target.file CONTAINS 'test' \
                         MERGE (target)<-[:TESTED_BY]-(test)",
                        )
                        .param("files", file_list),
                    ),
                )
                .map_err(|e| anyhow::anyhow!("derive_tested_by scoped failed: {e}"))?;
            }
            _ => {
                self.run_void(
                    "MATCH (test:Symbol)-[:CALLS]->(target:Symbol) \
                     WHERE test.file CONTAINS 'test' AND NOT target.file CONTAINS 'test' \
                     MERGE (target)<-[:TESTED_BY]-(test)",
                )?;
            }
        }
        let count = self.count_query("MATCH ()-[r:TESTED_BY]->() RETURN count(r) AS c", "c")?;
        Ok(count as usize)
    }

    fn clear_all_data(&self) -> Result<()> {
        loop {
            let deleted = self.count_query(
                "MATCH (n) WITH n LIMIT 5000 DETACH DELETE n RETURN count(*) AS c",
                "c",
            )?;
            if deleted == 0 {
                break;
            }
        }
        Ok(())
    }

    fn upsert_repo(&self, repo_name: &str) -> Result<()> {
        self.block_on(
            self.graph
                .run(query("MERGE (r:Repo {name: $name})").param("name", repo_name)),
        )
        .map_err(|e| anyhow::anyhow!("upsert Repo node failed: {e}"))?;
        self.block_on(
            self.graph.run(
                query(
                    "MATCH (f:File) WHERE f.repo IS NULL OR f.repo = '' \
                 SET f.repo = $repo",
                )
                .param("repo", repo_name),
            ),
        )
        .map_err(|e| anyhow::anyhow!("set file repo failed: {e}"))?;
        self.block_on(
            self.graph.run(
                query(
                    "MATCH (f:File {repo: $repo}), (r:Repo {name: $repo}) \
                 MERGE (f)-[:BELONGS_TO]->(r)",
                )
                .param("repo", repo_name),
            ),
        )
        .map_err(|e| anyhow::anyhow!("upsert BELONGS_TO edges failed: {e}"))?;
        Ok(())
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

    fn import_scip_index(
        &self,
        _index_path: &std::path::Path,
        _project_root: Option<&std::path::Path>,
    ) -> Result<crate::scip::ImportStats> {
        anyhow::bail!("SCIP import is not yet implemented for Neo4j backend")
    }

    fn ingest_structured_data(
        &self,
        schema: &crate::structured::SchemaMeta,
        data: &[serde_json::Value],
    ) -> Result<crate::structured::IngestResult> {
        use crate::structured::escape;

        let mut nodes_created = 0usize;
        let mut edges_created = 0usize;

        for (idx, record) in data.iter().enumerate() {
            let obj = record
                .as_object()
                .with_context(|| format!("record {} is not an object", idx))?;

            let id = if let Some(tmpl) = &schema.id_template {
                crate::structured::interpolate_template(tmpl, obj)
            } else if let Some(v) = obj.get("id") {
                v.as_str()
                    .unwrap_or(&format!("{}_{}", schema.schema_id, idx))
                    .to_string()
            } else {
                format!("{}_{}", schema.schema_id, idx)
            };

            let mut props = vec![format!("id: '{}'", escape(&id))];
            for col in &schema.columns {
                let val = obj.get(&col.name);
                if col.required && val.is_none() {
                    anyhow::bail!("Record {}: missing required field '{}'", idx, col.name);
                }
                let formatted = crate::structured::format_value(&col.col_type, val);
                props.push(format!("{}: {}", col.name, formatted));
            }

            let cypher = format!("CREATE (:{} {{{}}})", schema.node_table, props.join(", "));
            self.raw_query(&cypher)?;
            nodes_created += 1;

            for edge in &schema.edges {
                let targets = match obj.get(&edge.source_field) {
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>(),
                    Some(serde_json::Value::String(s)) => vec![s.clone()],
                    _ => continue,
                };

                for target in &targets {
                    let target_id = if edge.to_table == "Symbol" {
                        let esc = target.replace('\'', "\\'");
                        let q = format!(
                            "MATCH (s:Symbol) WHERE s.id = '{}' OR s.name = '{}' RETURN s.id LIMIT 1",
                            esc, esc
                        );
                        self.raw_query(&q)
                            .ok()
                            .and_then(|rows| rows.into_iter().next())
                            .and_then(|row| row.into_iter().next())
                            .unwrap_or_else(|| {
                                eprintln!("[warn] unresolved symbol reference: '{}'", target);
                                target.clone()
                            })
                    } else if let Some(lookup) = &edge.target_lookup {
                        format!("{}_{}", lookup, target)
                    } else {
                        target.clone()
                    };

                    let mut edge_props = String::new();
                    if !edge.properties.is_empty() {
                        let p: Vec<String> = edge
                            .properties
                            .iter()
                            .map(|c| {
                                let val = obj.get(&c.name);
                                format!(
                                    "{}: {}",
                                    c.name,
                                    crate::structured::format_value(&c.col_type, val)
                                )
                            })
                            .collect();
                        edge_props = format!(", {}", p.join(", "));
                    }

                    let edge_prop_str = if edge_props.is_empty() {
                        String::new()
                    } else {
                        format!("{{{}}}", edge_props.trim_start_matches(", "))
                    };

                    let check_query = format!(
                        "MATCH (a:{} {{id: '{}'}}), (b:{} {{id: '{}'}}) RETURN count(*)",
                        schema.node_table,
                        escape(&id),
                        edge.to_table,
                        escape(&target_id),
                    );
                    let target_exists = self
                        .raw_query(&check_query)
                        .ok()
                        .and_then(|rows| rows.into_iter().next())
                        .and_then(|row| row.into_iter().next())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0)
                        > 0;

                    if target_exists {
                        let cypher = format!(
                            "MATCH (a:{} {{id: '{}'}}), (b:{} {{id: '{}'}}) CREATE (a)-[:{}{}]->(b)",
                            schema.node_table,
                            escape(&id),
                            edge.to_table,
                            escape(&target_id),
                            edge.name,
                            edge_prop_str,
                        );
                        if self.raw_query(&cypher).is_ok() {
                            edges_created += 1;
                        }
                    }
                }
            }
        }

        Ok(crate::structured::IngestResult {
            nodes_created,
            edges_created,
        })
    }

    fn ingest_structured_file(
        &self,
        schema: &crate::structured::SchemaMeta,
        path: &std::path::Path,
    ) -> Result<crate::structured::IngestResult> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read data file: {}", path.display()))?;

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let data: Vec<serde_json::Value> = match ext {
            "json" => {
                let parsed: serde_json::Value = serde_json::from_str(&content)
                    .with_context(|| format!("invalid JSON: {}", path.display()))?;
                match parsed {
                    serde_json::Value::Array(arr) => arr,
                    obj @ serde_json::Value::Object(_) => vec![obj],
                    _ => anyhow::bail!("JSON must be an array or object"),
                }
            }
            "yaml" | "yml" => {
                let parsed: serde_json::Value = serde_yaml::from_str(&content)
                    .with_context(|| format!("invalid YAML: {}", path.display()))?;
                match parsed {
                    serde_json::Value::Array(arr) => arr,
                    obj @ serde_json::Value::Object(_) => vec![obj],
                    _ => anyhow::bail!("YAML must be a sequence or mapping"),
                }
            }
            _ => anyhow::bail!(
                "Unsupported data file format '{}' — use .json or .yaml/.yml",
                ext
            ),
        };

        self.ingest_structured_data(schema, &data)
    }

    fn ingest_structured_directory(
        &self,
        schema: &crate::structured::SchemaMeta,
        dir: &std::path::Path,
    ) -> Result<crate::structured::IngestResult> {
        if !dir.is_dir() {
            anyhow::bail!("'{}' is not a directory", dir.display());
        }

        let mut total = crate::structured::IngestResult {
            nodes_created: 0,
            edges_created: 0,
        };

        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "json" | "yaml" | "yml") {
                continue;
            }
            let result = self.ingest_structured_file(schema, &path)?;
            total.nodes_created += result.nodes_created;
            total.edges_created += result.edges_created;
        }

        Ok(total)
    }

    fn re_resolve_for_files(
        &self,
        files: &[String],
        extractions: &[FileExtraction],
        learned: Option<&LearnedStore>,
    ) -> Result<ResolveStats> {
        if files.is_empty() || extractions.is_empty() {
            return Ok(ResolveStats {
                total_calls: 0,
                resolved: 0,
                unresolved: 0,
                learned_resolved: 0,
                inherits_resolved: 0,
            });
        }

        for file in files {
            let escaped = file.replace('\'', "\\'");
            let _ = self.run_void(&format!(
                "MATCH (a:Symbol)-[r:CALLS]->(b:Symbol) WHERE a.file = '{}' DELETE r",
                escaped
            ));
            let _ = self.run_void(&format!(
                "MATCH (a:Symbol)-[r:INHERITS]->(b:Symbol) WHERE a.file = '{}' DELETE r",
                escaped
            ));
        }

        let target_files: std::collections::HashSet<&str> =
            files.iter().map(|f| f.as_str()).collect();
        let filtered: Vec<FileExtraction> = extractions
            .iter()
            .filter(|e| target_files.contains(e.file.as_str()))
            .cloned()
            .collect();

        self.resolve_calls(&filtered, learned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_columns() {
        let cols = parse_return_columns("MATCH (s:Symbol) RETURN s.id, s.name, s.kind");
        assert_eq!(cols, vec!["s.id", "s.name", "s.kind"]);
    }

    #[test]
    fn parse_aliased_columns() {
        let cols = parse_return_columns(
            "MATCH (s:Symbol) RETURN s.id AS id, s.name AS name, count(s) AS cnt",
        );
        assert_eq!(cols, vec!["id", "name", "cnt"]);
    }

    #[test]
    fn parse_with_order_by() {
        let cols = parse_return_columns("MATCH (s:Symbol) RETURN s.id, s.name ORDER BY s.name");
        assert_eq!(cols, vec!["s.id", "s.name"]);
    }

    #[test]
    fn parse_with_limit() {
        let cols = parse_return_columns("MATCH (s:Symbol) RETURN s.id, s.name LIMIT 10");
        assert_eq!(cols, vec!["s.id", "s.name"]);
    }

    #[test]
    fn parse_function_with_commas() {
        let cols = parse_return_columns(
            "MATCH (s:Symbol) RETURN coalesce(s.name, 'unknown') AS name, s.id",
        );
        assert_eq!(cols, vec!["name", "s.id"]);
    }

    #[test]
    fn parse_distinct() {
        let cols =
            parse_return_columns("MATCH (s:Symbol) RETURN DISTINCT s.id AS id, s.name AS name");
        assert_eq!(cols, vec!["id", "name"]);
    }

    #[test]
    fn parse_export_query() {
        let cols = parse_return_columns(
            "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, \
             s.start_line, s.end_line, s.visibility, s.parameters, \
             s.return_type, s.docstring",
        );
        assert_eq!(cols.len(), 10);
        assert_eq!(cols[0], "s.id");
        assert_eq!(cols[9], "s.docstring");
    }

    #[test]
    fn parse_no_return() {
        let cols = parse_return_columns("MATCH (s:Symbol)-[r:MEMBER_OF]->(c:Cluster) DELETE r");
        assert!(cols.is_empty());
    }

    #[test]
    fn extract_alias() {
        assert_eq!(extract_column_name("s.id AS id"), "id");
        assert_eq!(extract_column_name("count(s) AS cnt"), "cnt");
        assert_eq!(extract_column_name("s.name"), "s.name");
    }
}
