#![cfg(feature = "remote")]

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use neo4rs::{query, Graph, Query};
use tokio::runtime::Handle;

use crate::backend::DocBackend;
use crate::chunk::Chunk;
use crate::extract::ExtractedDoc;
use crate::store::{ChunkDetail, DocStoreStats, ImpactResult, PipelineCoreRecord};

const BATCH_SIZE: usize = 500;

static NEO4J_CONN: OnceLock<(Graph, Handle)> = OnceLock::new();

fn get_or_init_connection(uri: &str, user: &str, password: &str) -> Result<(Graph, Handle)> {
    if let Some((g, h)) = NEO4J_CONN.get() {
        return Ok((g.clone(), h.clone()));
    }
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let graph = rt.block_on(async {
        Graph::new(uri, user, password)
            .await
            .map_err(|e| anyhow::anyhow!("neo4j doc store connect failed: {e}"))
    })?;
    let handle = rt.handle().clone();
    std::mem::forget(rt);
    let _ = NEO4J_CONN.set((graph.clone(), handle.clone()));
    Ok((graph, handle))
}

/// Neo4j-backed document storage (remote mode).
///
/// Shares the same Neo4j Community instance as `Neo4jBackend` (code graph).
/// Document/Chunk/Source/PipelineCore nodes coexist with Symbol/File/Module
/// nodes in the same single-database graph, distinguished by label.
pub struct Neo4jDocStore {
    graph: Graph,
    handle: Handle,
}

impl Neo4jDocStore {
    pub fn connect(uri: &str, user: &str, password: &str) -> Result<Self> {
        let (graph, handle) = get_or_init_connection(uri, user, password)?;
        Ok(Self { graph, handle })
    }

    pub fn connect_from_env() -> Result<Self> {
        let uri = std::env::var("NEO4J_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string());
        let user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
        let password = std::env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "infigraph".to_string());
        Self::connect(&uri, &user, &password)
    }

    pub fn init_schema(&self) -> Result<()> {
        self.run_void(
            "CREATE CONSTRAINT doc_id IF NOT EXISTS FOR (d:Document) REQUIRE d.id IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT chunk_id IF NOT EXISTS FOR (c:Chunk) REQUIRE c.id IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT source_id IF NOT EXISTS FOR (s:Source) REQUIRE s.id IS UNIQUE",
        )?;
        self.run_void(
            "CREATE CONSTRAINT pipeline_core_id IF NOT EXISTS FOR (p:PipelineCore) REQUIRE p.id IS UNIQUE",
        )?;
        self.run_void("CREATE INDEX chunk_doc_file IF NOT EXISTS FOR (c:Chunk) ON (c.doc_file)")?;
        self.run_void("CREATE INDEX doc_file IF NOT EXISTS FOR (d:Document) ON (d.file)")?;
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

    fn count_query(&self, cypher: &str, key: &str) -> Result<usize> {
        let rows = self.run_query(query(cypher))?;
        if let Some(row) = rows.first() {
            Ok(row.get::<i64>(key).unwrap_or(0) as usize)
        } else {
            Ok(0)
        }
    }
}

impl DocBackend for Neo4jDocStore {
    fn get_doc_hashes(&self) -> Result<HashMap<String, String>> {
        let rows = self.run_query(query(
            "MATCH (d:Document) RETURN d.file AS file, d.content_hash AS hash",
        ))?;
        let mut hashes = HashMap::new();
        for row in rows {
            if let (Ok(file), Ok(hash)) = (row.get::<String>("file"), row.get::<String>("hash")) {
                hashes.insert(file, hash);
            }
        }
        Ok(hashes)
    }

    fn upsert_docs(&self, docs: &[&ExtractedDoc], chunks: &[&Chunk]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }

        // Delete existing data for changed files
        let files: Vec<String> = docs.iter().map(|d| d.file.clone()).collect();
        for chunk in files.chunks(BATCH_SIZE) {
            let file_list: Vec<String> = chunk.to_vec();
            let q = query("UNWIND $files AS f MATCH (c:Chunk {doc_file: f}) DETACH DELETE c")
                .param("files", file_list.clone());
            let _ = self.block_on(self.graph.run(q));

            let q = query("UNWIND $files AS f MATCH (d:Document {file: f}) DETACH DELETE d")
                .param("files", file_list);
            let _ = self.block_on(self.graph.run(q));
        }

        // Insert documents one at a time (neo4rs doesn't support serde_json in UNWIND maps)
        for d in docs {
            let cc = chunks.iter().filter(|c| c.doc_file == d.file).count() as i64;
            let q = query(
                "CREATE (doc:Document { \
                   id: $id, title: $title, file: $file, format: $format, \
                   content_hash: $hash, page_count: $page_count, \
                   chunk_count: $chunk_count \
                 })",
            )
            .param("id", d.file.clone())
            .param("title", d.title.clone().unwrap_or_default())
            .param("file", d.file.clone())
            .param("format", d.format.as_str().to_string())
            .param("hash", d.content_hash.clone())
            .param("page_count", d.page_count.unwrap_or(0) as i64)
            .param("chunk_count", cc);
            self.block_on(self.graph.run(q))
                .map_err(|e| anyhow::anyhow!("insert Document: {e}"))?;
        }

        // Insert chunks one at a time + HAS_CHUNK edges
        for c in chunks {
            let q = query(
                "CREATE (ch:Chunk { \
                   id: $id, doc_file: $doc_file, idx: $idx, heading: $heading, \
                   text: $text, start_offset: $start_offset, end_offset: $end_offset, \
                   page: $page, content_hash: $hash \
                 })",
            )
            .param("id", c.id.clone())
            .param("doc_file", c.doc_file.clone())
            .param("idx", c.index as i64)
            .param("heading", c.heading.clone().unwrap_or_default())
            .param("text", c.text.clone())
            .param("start_offset", c.start_offset as i64)
            .param("end_offset", c.end_offset as i64)
            .param("page", c.page.unwrap_or(0) as i64)
            .param("hash", c.content_hash.clone());
            self.block_on(self.graph.run(q))
                .map_err(|e| anyhow::anyhow!("insert Chunk: {e}"))?;

            // HAS_CHUNK edge
            let q = query(
                "MATCH (d:Document {id: $doc_file}), (ch:Chunk {id: $chunk_id}) \
                 CREATE (d)-[:HAS_CHUNK]->(ch)",
            )
            .param("doc_file", c.doc_file.clone())
            .param("chunk_id", c.id.clone());
            self.block_on(self.graph.run(q))
                .map_err(|e| anyhow::anyhow!("create HAS_CHUNK: {e}"))?;
        }

        Ok(())
    }

    fn delete_docs_by_ids(&self, doc_ids: &[&str]) -> Result<()> {
        if doc_ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = doc_ids.iter().map(|s| s.to_string()).collect();
        for chunk in ids.chunks(BATCH_SIZE) {
            let id_list: Vec<String> = chunk.to_vec();
            let q = query("UNWIND $ids AS id MATCH (c:Chunk {doc_file: id}) DETACH DELETE c")
                .param("ids", id_list.clone());
            let _ = self.block_on(self.graph.run(q));

            let q = query("UNWIND $ids AS id MATCH (d:Document {id: id}) DETACH DELETE d")
                .param("ids", id_list);
            let _ = self.block_on(self.graph.run(q));
        }
        Ok(())
    }

    fn ensure_document_node(&self, doc_id: &str) -> Result<()> {
        let q = query("MERGE (d:Document {id: $id})").param("id", doc_id.to_string());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("ensure document node: {e}"))?;
        Ok(())
    }

    fn upsert_source(
        &self,
        id: &str,
        source_type: &str,
        base_url: &str,
        space_key: &str,
    ) -> Result<()> {
        let q = query("MATCH (s:Source {id: $id}) DETACH DELETE s").param("id", id.to_string());
        let _ = self.block_on(self.graph.run(q));

        let q = query(
            "CREATE (s:Source { \
               id: $id, source_type: $source_type, base_url: $base_url, \
               space_key: $space_key, last_synced: $last_synced \
             })",
        )
        .param("id", id.to_string())
        .param("source_type", source_type.to_string())
        .param("base_url", base_url.to_string())
        .param("space_key", space_key.to_string())
        .param("last_synced", chrono::Utc::now().to_rfc3339());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("create Source: {e}"))?;
        Ok(())
    }

    fn link_doc_to_source(&self, doc_id: &str, source_id: &str) -> Result<()> {
        let q = query(
            "MATCH (d:Document {id: $doc_id}), (s:Source {id: $source_id}) \
             CREATE (d)-[:FROM_SOURCE]->(s)",
        )
        .param("doc_id", doc_id.to_string())
        .param("source_id", source_id.to_string());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("link FROM_SOURCE: {e}"))?;
        Ok(())
    }

    fn get_docs_by_source(&self, source_id: &str) -> Result<Vec<String>> {
        let q = query(
            "MATCH (d:Document)-[:FROM_SOURCE]->(s:Source {id: $source_id}) RETURN d.id AS id",
        )
        .param("source_id", source_id.to_string());
        let rows = self.run_query(q)?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>("id").ok())
            .collect())
    }

    fn create_link(
        &self,
        from_doc_id: &str,
        to_doc_id: &str,
        url: &str,
        link_type: &str,
    ) -> Result<()> {
        let q = query(
            "MATCH (a:Document {id: $from}), (b:Document {id: $to}) \
             CREATE (a)-[:LINKS_TO {url: $url, link_type: $link_type}]->(b)",
        )
        .param("from", from_doc_id.to_string())
        .param("to", to_doc_id.to_string())
        .param("url", url.to_string())
        .param("link_type", link_type.to_string());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("create LINKS_TO: {e}"))?;
        Ok(())
    }

    fn delete_links_from(&self, doc_id: &str) -> Result<()> {
        let q = query("MATCH (a:Document {id: $id})-[r:LINKS_TO]->() DELETE r")
            .param("id", doc_id.to_string());
        let _ = self.block_on(self.graph.run(q));
        Ok(())
    }

    fn get_all_chunks(&self) -> Result<Vec<(String, String)>> {
        let rows = self.run_query(query("MATCH (c:Chunk) RETURN c.id AS id, c.text AS text"))?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let id = r.get::<String>("id").ok()?;
                let text = r.get::<String>("text").ok()?;
                Some((id, text))
            })
            .collect())
    }

    fn get_chunk_ids(&self) -> Result<std::collections::HashSet<String>> {
        let rows = self.run_query(query("MATCH (c:Chunk) RETURN c.id AS id"))?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>("id").ok())
            .collect())
    }

    fn get_chunk_details(&self, chunk_ids: &[&str]) -> Result<Vec<ChunkDetail>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = chunk_ids.iter().map(|s| s.to_string()).collect();
        let q = query(
            "UNWIND $ids AS cid \
             MATCH (c:Chunk {id: cid}) \
             RETURN c.id AS id, c.doc_file AS doc_file, c.idx AS idx, \
                    c.heading AS heading, c.text AS text, \
                    c.start_offset AS start_offset, c.end_offset AS end_offset, \
                    c.page AS page",
        )
        .param("ids", ids);
        let rows = self.run_query(q)?;
        let mut details = Vec::new();
        for row in rows {
            let id = row.get::<String>("id").unwrap_or_default();
            let doc_file = row.get::<String>("doc_file").unwrap_or_default();
            let idx = row.get::<i64>("idx").unwrap_or(0);
            let heading = row.get::<String>("heading").ok();
            let text = row.get::<String>("text").unwrap_or_default();
            let start_offset = row.get::<i64>("start_offset").unwrap_or(0);
            let end_offset = row.get::<i64>("end_offset").unwrap_or(0);
            let page_val = row.get::<i64>("page").unwrap_or(0);
            details.push(ChunkDetail {
                id,
                doc_file,
                index: idx as usize,
                heading: heading.filter(|h| !h.is_empty()),
                text,
                start_offset: start_offset as usize,
                end_offset: end_offset as usize,
                page: if page_val > 0 {
                    Some(page_val as usize)
                } else {
                    None
                },
            });
        }
        Ok(details)
    }

    fn stats(&self) -> Result<DocStoreStats> {
        let doc_count = self.count_query("MATCH (d:Document) RETURN count(d) AS cnt", "cnt")?;
        let chunk_count = self.count_query("MATCH (c:Chunk) RETURN count(c) AS cnt", "cnt")?;
        Ok(DocStoreStats {
            document_count: doc_count,
            chunk_count,
        })
    }

    fn ensure_plugin_table(&self, _plugin_id: &str, _columns: &[(String, String)]) -> Result<()> {
        // Neo4j is schemaless — plugin properties are stored as node properties
        // on Pipeline_<plugin_id>-labeled nodes. No DDL needed.
        Ok(())
    }

    fn upsert_pipeline_core(&self, record: &PipelineCoreRecord) -> Result<()> {
        let q = query("MATCH (p:PipelineCore {id: $id}) DETACH DELETE p")
            .param("id", record.id.clone());
        let _ = self.block_on(self.graph.run(q));

        let q = query(
            "CREATE (p:PipelineCore { \
               id: $id, name: $name, doc_id: $doc_id, plugin_id: $plugin_id, \
               inputs: $inputs, outputs: $outputs \
             })",
        )
        .param("id", record.id.clone())
        .param("name", record.name.clone())
        .param("doc_id", record.doc_id.clone())
        .param("plugin_id", record.plugin_id.clone())
        .param("inputs", record.inputs.clone())
        .param("outputs", record.outputs.clone());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("create PipelineCore: {e}"))?;
        Ok(())
    }

    fn upsert_plugin_properties(
        &self,
        pipeline_id: &str,
        plugin_id: &str,
        properties: &serde_json::Map<String, serde_json::Value>,
        schema: &[(String, String)],
    ) -> Result<()> {
        let label = format!("Pipeline_{}", plugin_id);

        // Delete existing
        let q = query(&format!("MATCH (p:{} {{id: $id}}) DELETE p", label))
            .param("id", pipeline_id.to_string());
        let _ = self.block_on(self.graph.run(q));

        // Build property assignments and params individually
        let mut prop_parts = vec!["id: $id".to_string()];
        let mut param_values: Vec<(String, String)> =
            vec![("id".to_string(), pipeline_id.to_string())];
        for (col_name, _col_type) in schema {
            if let Some(val) = properties.get(col_name.as_str()) {
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                prop_parts.push(format!("{}: ${}", col_name, col_name));
                param_values.push((col_name.clone(), s));
            }
        }

        let cypher = format!("CREATE (p:{} {{{}}})", label, prop_parts.join(", "));
        let mut q = query(&cypher);
        for (key, val) in &param_values {
            q = q.param(key.as_str(), val.clone());
        }
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("upsert plugin properties: {e}"))?;
        Ok(())
    }

    fn link_pipeline_core_to_doc(&self, pipeline_id: &str, doc_id: &str) -> Result<()> {
        let q = query(
            "MATCH (p:PipelineCore {id: $pid}), (d:Document {id: $did}) \
             CREATE (p)-[:DEFINED_IN]->(d)",
        )
        .param("pid", pipeline_id.to_string())
        .param("did", doc_id.to_string());
        self.block_on(self.graph.run(q))
            .map_err(|e| anyhow::anyhow!("link DEFINED_IN: {e}"))?;
        Ok(())
    }

    fn link_pipeline_dependencies(&self) -> Result<usize> {
        let cores = self.get_all_pipeline_cores(None)?;
        if cores.len() < 2 {
            return Ok(0);
        }

        let q = query("MATCH ()-[r:DEPENDS_ON]->() DELETE r");
        let _ = self.block_on(self.graph.run(q));

        let mut count = 0;
        for producer in &cores {
            if producer.outputs.is_empty() {
                continue;
            }
            for consumer in &cores {
                if consumer.id == producer.id || consumer.inputs.is_empty() {
                    continue;
                }
                let has_match = producer
                    .outputs
                    .iter()
                    .any(|out| consumer.inputs.iter().any(|inp| inp == out));
                if has_match {
                    let q = query(
                        "MATCH (a:PipelineCore {id: $a_id}), (b:PipelineCore {id: $b_id}) \
                         CREATE (a)-[:DEPENDS_ON {dep_type: 'data'}]->(b)",
                    )
                    .param("a_id", consumer.id.clone())
                    .param("b_id", producer.id.clone());
                    self.block_on(self.graph.run(q))
                        .map_err(|e| anyhow::anyhow!("create DEPENDS_ON: {e}"))?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    fn get_all_pipeline_cores(&self, plugin_id: Option<&str>) -> Result<Vec<PipelineCoreRecord>> {
        let q = match plugin_id {
            Some(pid) => query(
                "MATCH (p:PipelineCore) WHERE p.plugin_id = $pid \
                 RETURN p.id AS id, p.name AS name, p.doc_id AS doc_id, \
                        p.plugin_id AS plugin_id, p.inputs AS inputs, p.outputs AS outputs",
            )
            .param("pid", pid.to_string()),
            None => query(
                "MATCH (p:PipelineCore) \
                 RETURN p.id AS id, p.name AS name, p.doc_id AS doc_id, \
                        p.plugin_id AS plugin_id, p.inputs AS inputs, p.outputs AS outputs",
            ),
        };
        let rows = self.run_query(q)?;
        let mut records = Vec::new();
        for row in rows {
            let id = row.get::<String>("id").unwrap_or_default();
            let name = row.get::<String>("name").unwrap_or_default();
            let doc_id = row.get::<String>("doc_id").unwrap_or_default();
            let plugin_id = row.get::<String>("plugin_id").unwrap_or_default();
            // neo4rs returns typed lists — extract directly
            let inputs: Vec<String> = row.get::<Vec<String>>("inputs").unwrap_or_default();
            let outputs: Vec<String> = row.get::<Vec<String>>("outputs").unwrap_or_default();
            records.push(PipelineCoreRecord {
                id,
                name,
                doc_id,
                plugin_id,
                inputs,
                outputs,
            });
        }
        Ok(records)
    }

    fn get_pipeline_core(&self, pipeline_id: &str) -> Result<Option<PipelineCoreRecord>> {
        let q = query(
            "MATCH (p:PipelineCore {id: $id}) \
             RETURN p.id AS id, p.name AS name, p.doc_id AS doc_id, \
                    p.plugin_id AS plugin_id, p.inputs AS inputs, p.outputs AS outputs",
        )
        .param("id", pipeline_id.to_string());
        let rows = self.run_query(q)?;
        if let Some(row) = rows.first() {
            let id = row.get::<String>("id").unwrap_or_default();
            let name = row.get::<String>("name").unwrap_or_default();
            let doc_id = row.get::<String>("doc_id").unwrap_or_default();
            let plugin_id = row.get::<String>("plugin_id").unwrap_or_default();
            let inputs: Vec<String> = row.get::<Vec<String>>("inputs").unwrap_or_default();
            let outputs: Vec<String> = row.get::<Vec<String>>("outputs").unwrap_or_default();
            Ok(Some(PipelineCoreRecord {
                id,
                name,
                doc_id,
                plugin_id,
                inputs,
                outputs,
            }))
        } else {
            Ok(None)
        }
    }

    fn impact_analysis(&self, table_name: &str, max_depth: u32) -> Result<Vec<ImpactResult>> {
        let mut results = Vec::new();

        // Direct impact: pipelines that consume this table
        // Neo4j uses `$table IN p.inputs` instead of Kùzu's `list_contains(p.inputs, ...)`
        let q = query(
            "MATCH (p:PipelineCore) WHERE $table IN p.inputs \
             RETURN p.id AS id, p.name AS name",
        )
        .param("table", table_name.to_string());
        let rows = self.run_query(q)?;
        let mut affected_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in rows {
            let id = row.get::<String>("id").unwrap_or_default();
            let name = row.get::<String>("name").unwrap_or_default();
            affected_ids.insert(id.clone());
            results.push(ImpactResult {
                pipeline_id: id,
                pipeline_name: name,
                impact_type: "direct".to_string(),
                depth: 1,
                path: table_name.to_string(),
            });
        }

        // Transitive impact
        if max_depth > 1 && !affected_ids.is_empty() {
            for depth in 2..=max_depth {
                let current_ids: Vec<String> = affected_ids.iter().cloned().collect();
                let mut new_ids = Vec::new();

                for src_id in &current_ids {
                    let q = query(
                        "MATCH (a:PipelineCore)-[:DEPENDS_ON]->(b:PipelineCore {id: $bid}) \
                         RETURN a.id AS id, a.name AS name",
                    )
                    .param("bid", src_id.clone());
                    let rows = self.run_query(q)?;

                    for row in rows {
                        let id = row.get::<String>("id").unwrap_or_default();
                        if !affected_ids.contains(&id) {
                            results.push(ImpactResult {
                                pipeline_id: id.clone(),
                                pipeline_name: row.get::<String>("name").unwrap_or_default(),
                                impact_type: "transitive".to_string(),
                                depth,
                                path: format!("{} → ... (depth {})", table_name, depth),
                            });
                            new_ids.push(id);
                        }
                    }
                }

                if new_ids.is_empty() {
                    break;
                }
                affected_ids.extend(new_ids);
            }
        }

        Ok(results)
    }

    fn get_pipeline_deps(&self) -> Result<Vec<(String, String, String)>> {
        let rows = self.run_query(query(
            "MATCH (c:PipelineCore)-[r:DEPENDS_ON]->(p:PipelineCore) \
             RETURN c.name AS consumer, p.name AS producer, r.dep_type AS dep_type",
        ))?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let consumer = r.get::<String>("consumer").ok()?;
                let producer = r.get::<String>("producer").ok()?;
                let dep_type = r.get::<String>("dep_type").unwrap_or_default();
                Some((consumer, producer, dep_type))
            })
            .collect())
    }

    fn query_plugin_table(
        &self,
        plugin_id: &str,
        field: &str,
        value: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let label = format!("Pipeline_{}", plugin_id);
        // Neo4j: use `RETURN properties(p)` instead of Kùzu's `RETURN p.*`
        // Use toLower() instead of lower() for Neo4j compatibility
        let cypher = format!(
            "MATCH (p:{}) WHERE toLower(p.{}) CONTAINS toLower($val) \
             RETURN properties(p) AS props",
            label, field
        );
        let q = query(&cypher).param("val", value.to_string());
        let rows = self.run_query(q)?;
        let mut results = Vec::new();
        for row in rows {
            if let Ok(props) = row.get::<HashMap<String, String>>("props") {
                let vals: Vec<serde_json::Value> = props
                    .values()
                    .map(|v| serde_json::Value::String(v.clone()))
                    .collect();
                results.push(serde_json::Value::Array(vals));
            }
        }
        Ok(results)
    }

    fn pipeline_core_count(&self) -> Result<usize> {
        self.count_query("MATCH (p:PipelineCore) RETURN count(p) AS cnt", "cnt")
    }
}
