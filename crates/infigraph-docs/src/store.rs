use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use kuzu::{Connection, Database, SystemConfig};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::chunk::Chunk;
use crate::extract::ExtractedDoc;

const CREATE_SCHEMA: &[&str] = &[
    "CREATE NODE TABLE IF NOT EXISTS Document(
        id STRING,
        title STRING,
        file STRING,
        format STRING,
        content_hash STRING,
        page_count INT64,
        chunk_count INT64,
        PRIMARY KEY(id)
    )",
    "CREATE NODE TABLE IF NOT EXISTS Chunk(
        id STRING,
        doc_file STRING,
        idx INT64,
        heading STRING,
        text STRING,
        start_offset INT64,
        end_offset INT64,
        page INT64,
        content_hash STRING,
        PRIMARY KEY(id)
    )",
    "CREATE REL TABLE IF NOT EXISTS HAS_CHUNK(FROM Document TO Chunk)",
    "CREATE NODE TABLE IF NOT EXISTS Source(
        id STRING,
        source_type STRING,
        base_url STRING,
        space_key STRING,
        last_synced STRING,
        PRIMARY KEY(id)
    )",
    "CREATE REL TABLE IF NOT EXISTS FROM_SOURCE(FROM Document TO Source)",
    "CREATE REL TABLE IF NOT EXISTS LINKS_TO(FROM Document TO Document, url STRING, link_type STRING)",
];

pub struct DocStore {
    db: Database,
}

impl DocStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::new(path, SystemConfig::default())
            .map_err(|e| anyhow::anyhow!("failed to open docs kuzu db: {e}"))?;
        let store = Self { db };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.connection()?;
        for ddl in CREATE_SCHEMA {
            conn.query(ddl)
                .map_err(|e| anyhow::anyhow!("schema DDL failed: {e}"))?;
        }
        Ok(())
    }

    pub fn connection(&self) -> Result<Connection<'_>> {
        Connection::new(&self.db)
            .map_err(|e| anyhow::anyhow!("failed to create connection: {e}"))
    }

    pub fn get_doc_hashes(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let result = conn
            .query("MATCH (d:Document) RETURN d.file, d.content_hash")
            .map_err(|e| anyhow::anyhow!("query doc hashes: {e}"))?;
        let mut hashes = HashMap::new();
        for row in result {
            if row.len() >= 2 {
                hashes.insert(row[0].to_string(), row[1].to_string());
            }
        }
        Ok(hashes)
    }

    pub fn upsert_all_parquet(&self, docs: &[&ExtractedDoc], chunks: &[&Chunk]) -> Result<()> {
        let conn = self.connection()?;

        // Delete existing data for changed files
        let file_list: Vec<String> = docs
            .iter()
            .map(|d| format!("'{}'", escape_str(&d.file)))
            .collect();
        if !file_list.is_empty() {
            let files_in = file_list.join(", ");
            let _ = conn.query(&format!(
                "MATCH (c:Chunk) WHERE c.doc_file IN [{}] DETACH DELETE c",
                files_in
            ));
            let _ = conn.query(&format!(
                "MATCH (d:Document) WHERE d.file IN [{}] DETACH DELETE d",
                files_in
            ));
        }

        let tmp_dir = tempfile::tempdir().context("create temp dir")?;

        // Write Document parquet
        {
            let ids: Vec<&str> = docs.iter().map(|d| d.file.as_str()).collect();
            let titles: Vec<Option<&str>> = docs.iter().map(|d| d.title.as_deref()).collect();
            let files: Vec<&str> = docs.iter().map(|d| d.file.as_str()).collect();
            let formats: Vec<&str> = docs.iter().map(|d| d.format.as_str()).collect();
            let hashes: Vec<&str> = docs.iter().map(|d| d.content_hash.as_str()).collect();
            let page_counts: Vec<i64> = docs.iter().map(|d| d.page_count.unwrap_or(0) as i64).collect();
            let chunk_counts: Vec<i64> = docs
                .iter()
                .map(|d| chunks.iter().filter(|c| c.doc_file == d.file).count() as i64)
                .collect();

            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Utf8, false),
                Field::new("title", DataType::Utf8, true),
                Field::new("file", DataType::Utf8, false),
                Field::new("format", DataType::Utf8, false),
                Field::new("content_hash", DataType::Utf8, false),
                Field::new("page_count", DataType::Int64, false),
                Field::new("chunk_count", DataType::Int64, false),
            ]));

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(titles)),
                    Arc::new(StringArray::from(files)),
                    Arc::new(StringArray::from(formats)),
                    Arc::new(StringArray::from(hashes)),
                    Arc::new(Int64Array::from(page_counts)),
                    Arc::new(Int64Array::from(chunk_counts)),
                ],
            )?;

            let path = tmp_dir.path().join("documents.parquet");
            let file = std::fs::File::create(&path)?;
            let props = WriterProperties::builder().build();
            let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
            writer.write(&batch)?;
            writer.close()?;

            conn.query(&format!(
                "COPY Document FROM '{}'",
                path.to_string_lossy()
            ))
            .map_err(|e| anyhow::anyhow!("COPY Document: {e}"))?;
        }

        // Write Chunk parquet
        if !chunks.is_empty() {
            let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
            let doc_files: Vec<&str> = chunks.iter().map(|c| c.doc_file.as_str()).collect();
            let indices: Vec<i64> = chunks.iter().map(|c| c.index as i64).collect();
            let headings: Vec<Option<&str>> = chunks.iter().map(|c| c.heading.as_deref()).collect();
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let start_offsets: Vec<i64> = chunks.iter().map(|c| c.start_offset as i64).collect();
            let end_offsets: Vec<i64> = chunks.iter().map(|c| c.end_offset as i64).collect();
            let pages: Vec<i64> = chunks.iter().map(|c| c.page.unwrap_or(0) as i64).collect();
            let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();

            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Utf8, false),
                Field::new("doc_file", DataType::Utf8, false),
                Field::new("idx", DataType::Int64, false),
                Field::new("heading", DataType::Utf8, true),
                Field::new("text", DataType::Utf8, false),
                Field::new("start_offset", DataType::Int64, false),
                Field::new("end_offset", DataType::Int64, false),
                Field::new("page", DataType::Int64, false),
                Field::new("content_hash", DataType::Utf8, false),
            ]));

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(doc_files)),
                    Arc::new(Int64Array::from(indices)),
                    Arc::new(StringArray::from(headings)),
                    Arc::new(StringArray::from(texts)),
                    Arc::new(Int64Array::from(start_offsets)),
                    Arc::new(Int64Array::from(end_offsets)),
                    Arc::new(Int64Array::from(pages)),
                    Arc::new(StringArray::from(hashes)),
                ],
            )?;

            let path = tmp_dir.path().join("chunks.parquet");
            let file = std::fs::File::create(&path)?;
            let props = WriterProperties::builder().build();
            let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
            writer.write(&batch)?;
            writer.close()?;

            conn.query(&format!(
                "COPY Chunk FROM '{}'",
                path.to_string_lossy()
            ))
            .map_err(|e| anyhow::anyhow!("COPY Chunk: {e}"))?;
        }

        // Write HAS_CHUNK edges
        if !chunks.is_empty() {
            let froms: Vec<&str> = chunks.iter().map(|c| c.doc_file.as_str()).collect();
            let tos: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();

            let schema = Arc::new(Schema::new(vec![
                Field::new("from", DataType::Utf8, false),
                Field::new("to", DataType::Utf8, false),
            ]));

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(froms)),
                    Arc::new(StringArray::from(tos)),
                ],
            )?;

            let path = tmp_dir.path().join("has_chunk.parquet");
            let file = std::fs::File::create(&path)?;
            let props = WriterProperties::builder().build();
            let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
            writer.write(&batch)?;
            writer.close()?;

            conn.query(&format!(
                "COPY HAS_CHUNK FROM '{}'",
                path.to_string_lossy()
            ))
            .map_err(|e| anyhow::anyhow!("COPY HAS_CHUNK: {e}"))?;
        }

        Ok(())
    }

    pub fn upsert_source(&self, id: &str, source_type: &str, base_url: &str, space_key: &str) -> Result<()> {
        let conn = self.connection()?;
        let _ = conn.query(&format!(
            "MATCH (s:Source) WHERE s.id = '{}' DETACH DELETE s",
            escape_str(id)
        ));
        conn.query(&format!(
            "CREATE (s:Source {{id: '{}', source_type: '{}', base_url: '{}', space_key: '{}', last_synced: '{}'}})",
            escape_str(id),
            escape_str(source_type),
            escape_str(base_url),
            escape_str(space_key),
            chrono::Utc::now().to_rfc3339(),
        ))
        .map_err(|e| anyhow::anyhow!("create Source: {e}"))?;
        Ok(())
    }

    pub fn link_doc_to_source(&self, doc_id: &str, source_id: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.query(&format!(
            "MATCH (d:Document), (s:Source) WHERE d.id = '{}' AND s.id = '{}' CREATE (d)-[:FROM_SOURCE]->(s)",
            escape_str(doc_id),
            escape_str(source_id),
        ))
        .map_err(|e| anyhow::anyhow!("link FROM_SOURCE: {e}"))?;
        Ok(())
    }

    pub fn get_docs_by_source(&self, source_id: &str) -> Result<Vec<String>> {
        let conn = self.connection()?;
        let result = conn
            .query(&format!(
                "MATCH (d:Document)-[:FROM_SOURCE]->(s:Source) WHERE s.id = '{}' RETURN d.id",
                escape_str(source_id)
            ))
            .map_err(|e| anyhow::anyhow!("query docs by source: {e}"))?;
        let mut ids = Vec::new();
        for row in result {
            if !row.is_empty() {
                ids.insert(ids.len(), row[0].to_string());
            }
        }
        Ok(ids)
    }

    pub fn delete_docs_by_ids(&self, doc_ids: &[&str]) -> Result<()> {
        if doc_ids.is_empty() {
            return Ok(());
        }
        let conn = self.connection()?;
        let id_list: String = doc_ids
            .iter()
            .map(|id| format!("'{}'", escape_str(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = conn.query(&format!(
            "MATCH (c:Chunk) WHERE c.doc_file IN [{}] DETACH DELETE c",
            id_list
        ));
        let _ = conn.query(&format!(
            "MATCH (d:Document) WHERE d.id IN [{}] DETACH DELETE d",
            id_list
        ));
        Ok(())
    }

    pub fn create_link(&self, from_doc_id: &str, to_doc_id: &str, url: &str, link_type: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.query(&format!(
            "MATCH (a:Document), (b:Document) WHERE a.id = '{}' AND b.id = '{}' \
             CREATE (a)-[:LINKS_TO {{url: '{}', link_type: '{}'}}]->(b)",
            escape_str(from_doc_id),
            escape_str(to_doc_id),
            escape_str(url),
            escape_str(link_type),
        ))
        .map_err(|e| anyhow::anyhow!("create LINKS_TO: {e}"))?;
        Ok(())
    }

    pub fn delete_links_from(&self, doc_id: &str) -> Result<()> {
        let conn = self.connection()?;
        let _ = conn.query(&format!(
            "MATCH (a:Document)-[r:LINKS_TO]->() WHERE a.id = '{}' DELETE r",
            escape_str(doc_id),
        ));
        Ok(())
    }

    pub fn stats(&self) -> Result<DocStoreStats> {
        let conn = self.connection()?;
        let doc_count = count_query(&conn, "MATCH (d:Document) RETURN count(d)");
        let chunk_count = count_query(&conn, "MATCH (c:Chunk) RETURN count(c)");
        Ok(DocStoreStats {
            document_count: doc_count,
            chunk_count,
        })
    }

    pub fn get_all_chunks(&self) -> Result<Vec<(String, String)>> {
        let conn = self.connection()?;
        let result = conn
            .query("MATCH (c:Chunk) RETURN c.id, c.text")
            .map_err(|e| anyhow::anyhow!("query chunks: {e}"))?;
        let mut chunks = Vec::new();
        for row in result {
            if row.len() >= 2 {
                chunks.push((row[0].to_string(), row[1].to_string()));
            }
        }
        Ok(chunks)
    }

    pub fn get_chunk_ids(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.connection()?;
        let result = conn
            .query("MATCH (c:Chunk) RETURN c.id")
            .map_err(|e| anyhow::anyhow!("query chunk ids: {e}"))?;
        let mut ids = std::collections::HashSet::new();
        for row in result {
            if !row.is_empty() {
                ids.insert(row[0].to_string());
            }
        }
        Ok(ids)
    }

    pub fn get_chunk_details(&self, chunk_ids: &[&str]) -> Result<Vec<ChunkDetail>> {
        let conn = self.connection()?;
        let id_list: String = chunk_ids
            .iter()
            .map(|id| format!("'{}'", escape_str(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "MATCH (c:Chunk) WHERE c.id IN [{}] RETURN c.id, c.doc_file, c.idx, c.heading, c.text, c.start_offset, c.end_offset, c.page",
            id_list
        );
        let result = conn
            .query(&query)
            .map_err(|e| anyhow::anyhow!("chunk details: {e}"))?;
        let mut details = Vec::new();
        for row in result {
            if row.len() >= 8 {
                let heading_str = row[3].to_string();
                let page_val: i64 = row[7].to_string().parse().unwrap_or(0);
                details.push(ChunkDetail {
                    id: row[0].to_string(),
                    doc_file: row[1].to_string(),
                    index: row[2].to_string().parse().unwrap_or(0),
                    heading: if heading_str.is_empty() {
                        None
                    } else {
                        Some(heading_str)
                    },
                    text: row[4].to_string(),
                    start_offset: row[5].to_string().parse().unwrap_or(0),
                    end_offset: row[6].to_string().parse().unwrap_or(0),
                    page: if page_val > 0 {
                        Some(page_val as usize)
                    } else {
                        None
                    },
                });
            }
        }
        Ok(details)
    }
}

#[derive(Debug)]
pub struct DocStoreStats {
    pub document_count: usize,
    pub chunk_count: usize,
}

#[derive(Debug, Clone)]
pub struct ChunkDetail {
    pub id: String,
    pub doc_file: String,
    pub index: usize,
    pub heading: Option<String>,
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub page: Option<usize>,
}

fn count_query(conn: &Connection<'_>, query: &str) -> usize {
    conn.query(query)
        .ok()
        .and_then(|mut r| r.next().map(|row| row[0].to_string().parse().unwrap_or(0)))
        .unwrap_or(0)
}

fn escape_str(s: &str) -> String {
    s.replace('\'', "\\'")
}
