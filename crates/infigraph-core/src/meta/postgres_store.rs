#![cfg(feature = "postgres")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use pgvector::Vector;
use tokio::runtime::Handle;
use tokio_postgres::{Client, NoTls};

use crate::graph::SessionData;
use crate::multi::{Contract, ContractKind, Group, Registry, RepoEntry};

static PG_CONN: OnceLock<PostgresMetaStore> = OnceLock::new();

const CORE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS repos (
    name TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    languages TEXT NOT NULL DEFAULT '[]',
    symbol_count BIGINT NOT NULL DEFAULT 0,
    module_count BIGINT NOT NULL DEFAULT 0,
    last_indexed_commit TEXT
);

CREATE TABLE IF NOT EXISTS groups (
    name TEXT NOT NULL,
    org TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (org, name)
);

CREATE TABLE IF NOT EXISTS group_repos (
    group_org TEXT NOT NULL DEFAULT '',
    group_name TEXT NOT NULL,
    repo_name TEXT REFERENCES repos(name) ON DELETE CASCADE,
    PRIMARY KEY (group_org, group_name, repo_name),
    FOREIGN KEY (group_org, group_name) REFERENCES groups(org, name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS contracts (
    id SERIAL PRIMARY KEY,
    group_org TEXT NOT NULL DEFAULT '',
    group_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    service TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    symbol_id TEXT NOT NULL,
    file TEXT NOT NULL,
    FOREIGN KEY (group_org, group_name) REFERENCES groups(org, name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS file_hashes (
    repo TEXT NOT NULL,
    file TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    PRIMARY KEY (repo, file)
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    pending_tasks TEXT NOT NULL DEFAULT '',
    decisions TEXT NOT NULL DEFAULT '',
    files_touched TEXT NOT NULL DEFAULT '',
    constraints_text TEXT NOT NULL DEFAULT '',
    assumptions TEXT NOT NULL DEFAULT '',
    blockers TEXT NOT NULL DEFAULT '',
    confidence REAL NOT NULL DEFAULT 1.0,
    created_at BIGINT NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL DEFAULT 0,
    last_accessed BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_contracts_group ON contracts(group_name);
CREATE INDEX IF NOT EXISTS idx_file_hashes_repo ON file_hashes(repo);
"#;

const MIGRATION_ORG_SQL: &str = r#"
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'groups' AND column_name = 'org'
    ) THEN
        -- Drop FKs that reference old PK
        ALTER TABLE group_repos DROP CONSTRAINT IF EXISTS group_repos_group_name_fkey;
        ALTER TABLE contracts DROP CONSTRAINT IF EXISTS contracts_group_name_fkey;

        -- Drop old PK, add org column, create new composite PK
        ALTER TABLE groups DROP CONSTRAINT groups_pkey;
        ALTER TABLE groups ADD COLUMN org TEXT NOT NULL DEFAULT '';
        ALTER TABLE groups ADD PRIMARY KEY (org, name);

        -- Add org column to group_repos
        ALTER TABLE group_repos ADD COLUMN group_org TEXT NOT NULL DEFAULT '';
        ALTER TABLE group_repos DROP CONSTRAINT group_repos_pkey;
        ALTER TABLE group_repos ADD PRIMARY KEY (group_org, group_name, repo_name);
        ALTER TABLE group_repos ADD FOREIGN KEY (group_org, group_name)
            REFERENCES groups(org, name) ON DELETE CASCADE;

        -- Add org column to contracts
        ALTER TABLE contracts ADD COLUMN group_org TEXT NOT NULL DEFAULT '';
        ALTER TABLE contracts ADD FOREIGN KEY (group_org, group_name)
            REFERENCES groups(org, name) ON DELETE CASCADE;
    END IF;
END $$;
"#;

const MIGRATION_COMMIT_SQL: &str = r#"
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'repos' AND column_name = 'last_indexed_commit'
    ) THEN
        ALTER TABLE repos ADD COLUMN last_indexed_commit TEXT;
    END IF;
END $$;
"#;

const VECTOR_SQL: &str = r#"
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS embeddings (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL DEFAULT 'symbol',
    vector vector(256) NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_embeddings_kind ON embeddings(kind);
CREATE INDEX IF NOT EXISTS idx_embeddings_hnsw ON embeddings USING hnsw (vector vector_cosine_ops);
"#;

/// Postgres-backed metadata store for remote (sidecar) deployment.
///
/// Replaces local JSON files (registry.json, sessions/) with Postgres tables.
/// Runs alongside Neo4jBackend in the same pod. Connection via localhost.
pub struct PostgresMetaStore {
    client: Client,
    handle: Handle,
}

impl PostgresMetaStore {
    /// Connect to Postgres. Default: `host=localhost user=infigraph password=infigraph dbname=infigraph`.
    pub fn connect(conn_str: &str) -> Result<Self> {
        let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let client = rt.block_on(async {
            let (client, connection) = tokio_postgres::connect(conn_str, NoTls)
                .await
                .map_err(|e| anyhow::anyhow!("postgres connect failed: {e:?}"))?;
            tokio::spawn(async move {
                if let Err(e) = connection.await {
                    eprintln!("postgres connection error: {e}");
                }
            });
            Ok::<_, anyhow::Error>(client)
        })?;
        let handle = rt.handle().clone();
        std::mem::forget(rt);
        Ok(Self { client, handle })
    }

    /// Connect using `DATABASE_URL` env var.
    pub fn connect_from_env() -> Result<Self> {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "host=localhost user=infigraph password=infigraph dbname=infigraph".into()
        });
        Self::connect(&url)
    }

    /// Cached connection — creates once, reuses across all callers.
    /// Avoids per-call runtime leak from `connect_from_env()`.
    pub fn connect_from_env_cached() -> Result<&'static Self> {
        if let Some(store) = PG_CONN.get() {
            return Ok(store);
        }
        let store = Self::connect_from_env()?;
        let _ = PG_CONN.set(store);
        PG_CONN
            .get()
            .ok_or_else(|| anyhow::anyhow!("PG_CONN OnceLock unexpectedly empty"))
    }

    /// Run schema migrations (idempotent).
    /// Core tables (repos, groups, etc.) are created first.
    /// pgvector extension + embeddings table are created separately so a missing
    /// pgvector package doesn't block registry operations.
    pub fn init_schema(&self) -> Result<()> {
        self.block_on(async {
            self.client
                .batch_execute(CORE_SQL)
                .await
                .map_err(|e| anyhow::anyhow!("core schema init failed: {e:?}"))?;
            self.client
                .batch_execute(MIGRATION_ORG_SQL)
                .await
                .map_err(|e| anyhow::anyhow!("org migration failed: {e:?}"))?;
            self.client
                .batch_execute(MIGRATION_COMMIT_SQL)
                .await
                .map_err(|e| anyhow::anyhow!("commit column migration failed: {e:?}"))?;
            if let Err(e) = self.client.batch_execute(VECTOR_SQL).await {
                eprintln!("warning: pgvector schema init failed (embeddings unavailable): {e:?}");
            }
            Ok(())
        })
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.handle.block_on(f)
    }

    pub fn execute_raw(&self, sql: &str) -> Result<u64> {
        self.block_on(async {
            self.client
                .execute(sql, &[])
                .await
                .map_err(|e| anyhow::anyhow!("execute_raw failed: {e:?}"))
        })
    }

    // ── Registry operations ──────────────────────────────────────────

    pub fn load_registry(&self) -> Result<Registry> {
        let repos = self.load_repos()?;
        let groups = self.load_groups()?;
        Ok(Registry { repos, groups })
    }

    pub fn save_registry(&self, registry: &Registry) -> Result<()> {
        for (name, entry) in &registry.repos {
            self.upsert_repo(name, entry)?;
        }
        for (name, group) in &registry.groups {
            self.upsert_group(name, group)?;
        }
        Ok(())
    }

    pub fn upsert_repo(&self, name: &str, entry: &RepoEntry) -> Result<()> {
        let langs = serde_json::to_string(&entry.languages)?;
        let commit = entry.last_indexed_commit.as_deref();
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO repos (name, path, languages, symbol_count, module_count, last_indexed_commit) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (name) DO UPDATE SET \
                       path = EXCLUDED.path, languages = EXCLUDED.languages, \
                       symbol_count = EXCLUDED.symbol_count, module_count = EXCLUDED.module_count, \
                       last_indexed_commit = EXCLUDED.last_indexed_commit",
                    &[
                        &name,
                        &entry.path.to_string_lossy().as_ref(),
                        &langs,
                        &(entry.symbol_count as i64),
                        &(entry.module_count as i64),
                        &commit,
                    ],
                )
                .await
                .map_err(|e| anyhow::anyhow!("upsert repo failed: {e:?}"))
        })?;
        Ok(())
    }

    fn load_repos(&self) -> Result<HashMap<String, RepoEntry>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT name, path, languages, symbol_count, module_count, last_indexed_commit FROM repos",
                    &[],
                )
                .await
                .map_err(|e| anyhow::anyhow!("load repos failed: {e:?}"))
        })?;

        let mut map = HashMap::new();
        for row in &rows {
            let name: String = row.get(0);
            let path_str: String = row.get(1);
            let langs_str: String = row.get(2);
            let languages: Vec<String> = serde_json::from_str(&langs_str).unwrap_or_default();
            let symbol_count: i64 = row.get(3);
            let module_count: i64 = row.get(4);
            let last_indexed_commit: Option<String> = row.get(5);
            map.insert(
                name.clone(),
                RepoEntry {
                    name,
                    path: PathBuf::from(path_str),
                    languages,
                    symbol_count: symbol_count as u64,
                    module_count: module_count as u64,
                    last_indexed_commit,
                },
            );
        }
        Ok(map)
    }

    fn load_groups(&self) -> Result<HashMap<String, Group>> {
        let group_rows = self.block_on(async {
            self.client
                .query("SELECT org, name FROM groups", &[])
                .await
                .map_err(|e| anyhow::anyhow!("load groups failed: {e:?}"))
        })?;

        let mut groups = HashMap::new();
        for row in &group_rows {
            let org: String = row.get(0);
            let name: String = row.get(1);
            let key = crate::multi::qualified_group_name(&org, &name);
            let repos = self.load_group_repos(&org, &name)?;
            let contracts = self.load_group_contracts(&org, &name)?;
            groups.insert(
                key,
                Group {
                    name,
                    org,
                    repos,
                    contracts,
                },
            );
        }
        Ok(groups)
    }

    fn load_group_repos(&self, org: &str, group_name: &str) -> Result<Vec<String>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT repo_name FROM group_repos WHERE group_org = $1 AND group_name = $2",
                    &[&org, &group_name],
                )
                .await
                .map_err(|e| anyhow::anyhow!("load group repos failed: {e:?}"))
        })?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    fn load_group_contracts(&self, org: &str, group_name: &str) -> Result<Vec<Contract>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT kind, service, method, path, symbol_id, file \
                         FROM contracts WHERE group_org = $1 AND group_name = $2",
                    &[&org, &group_name],
                )
                .await
                .map_err(|e| anyhow::anyhow!("load contracts failed: {e:?}"))
        })?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                let kind_str: String = r.get(0);
                let kind = match kind_str.as_str() {
                    "HttpRoute" => ContractKind::HttpRoute,
                    "GrpcService" => ContractKind::GrpcService,
                    "EventPublish" => ContractKind::EventPublish,
                    "EventSubscribe" => ContractKind::EventSubscribe,
                    "SharedPackage" => ContractKind::SharedPackage,
                    _ => return None,
                };
                Some(Contract {
                    kind,
                    service: r.get(1),
                    method: r.get(2),
                    path: r.get(3),
                    symbol_id: r.get(4),
                    file: r.get(5),
                })
            })
            .collect())
    }

    pub fn upsert_group(&self, _key: &str, group: &Group) -> Result<()> {
        let org = &group.org;
        let name = &group.name;
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO groups (org, name) VALUES ($1, $2) ON CONFLICT (org, name) DO NOTHING",
                    &[&org.as_str(), &name.as_str()],
                )
                .await
                .map_err(|e| anyhow::anyhow!("upsert group failed: {e:?}"))
        })?;

        self.block_on(async {
            self.client
                .execute(
                    "DELETE FROM group_repos WHERE group_org = $1 AND group_name = $2",
                    &[&org.as_str(), &name.as_str()],
                )
                .await
                .map_err(|e| anyhow::anyhow!("clear group repos failed: {e:?}"))
        })?;

        if !group.repos.is_empty() {
            let mut sql =
                String::from("INSERT INTO group_repos (group_org, group_name, repo_name) VALUES ");
            let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
            for (i, repo) in group.repos.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                let base = i * 3;
                sql.push_str(&format!("(${}, ${}, ${})", base + 1, base + 2, base + 3));
                params.push(Box::new(org.to_string()));
                params.push(Box::new(name.to_string()));
                params.push(Box::new(repo.clone()));
            }
            sql.push_str(" ON CONFLICT DO NOTHING");
            let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                params.iter().map(|p| p.as_ref()).collect();
            self.block_on(async {
                self.client
                    .execute(&sql as &str, &param_refs)
                    .await
                    .map_err(|e| anyhow::anyhow!("bulk add group repos failed: {e:?}"))
            })?;
        }

        self.block_on(async {
            self.client
                .execute(
                    "DELETE FROM contracts WHERE group_org = $1 AND group_name = $2",
                    &[&org.as_str(), &name.as_str()],
                )
                .await
                .map_err(|e| anyhow::anyhow!("clear contracts failed: {e:?}"))
        })?;

        if !group.contracts.is_empty() {
            let mut sql = String::from(
                "INSERT INTO contracts (group_org, group_name, kind, service, method, path, symbol_id, file) VALUES ",
            );
            let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
            for (i, c) in group.contracts.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                let base = i * 8;
                sql.push_str(&format!(
                    "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                    base + 1,
                    base + 2,
                    base + 3,
                    base + 4,
                    base + 5,
                    base + 6,
                    base + 7,
                    base + 8
                ));
                let kind_str = match c.kind {
                    ContractKind::HttpRoute => "HttpRoute",
                    ContractKind::GrpcService => "GrpcService",
                    ContractKind::EventPublish => "EventPublish",
                    ContractKind::EventSubscribe => "EventSubscribe",
                    ContractKind::SharedPackage => "SharedPackage",
                };
                params.push(Box::new(org.to_string()));
                params.push(Box::new(name.to_string()));
                params.push(Box::new(kind_str.to_string()));
                params.push(Box::new(c.service.clone()));
                params.push(Box::new(c.method.clone()));
                params.push(Box::new(c.path.clone()));
                params.push(Box::new(c.symbol_id.clone()));
                params.push(Box::new(c.file.clone()));
            }
            let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                params.iter().map(|p| p.as_ref()).collect();
            self.block_on(async {
                self.client
                    .execute(&sql as &str, &param_refs)
                    .await
                    .map_err(|e| anyhow::anyhow!("bulk insert contracts failed: {e:?}"))
            })?;
        }

        Ok(())
    }

    pub fn create_group(&self, name: &str) -> Result<()> {
        self.create_group_with_org(name, "")
    }

    pub fn create_group_with_org(&self, name: &str, org: &str) -> Result<()> {
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO groups (org, name) VALUES ($1, $2) ON CONFLICT (org, name) DO NOTHING",
                    &[&org, &name],
                )
                .await
                .map_err(|e| anyhow::anyhow!("create group failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn group_add(&self, group_name: &str, repo_name: &str) -> Result<()> {
        self.group_add_with_org("", group_name, repo_name)
    }

    pub fn group_add_with_org(&self, org: &str, group_name: &str, repo_name: &str) -> Result<()> {
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO group_repos (group_org, group_name, repo_name) VALUES ($1, $2, $3) \
                     ON CONFLICT DO NOTHING",
                    &[&org, &group_name, &repo_name],
                )
                .await
                .map_err(|e| anyhow::anyhow!("group add failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn group_remove(&self, group_name: &str, repo_name: &str) -> Result<()> {
        self.group_remove_with_org("", group_name, repo_name)
    }

    pub fn group_remove_with_org(
        &self,
        org: &str,
        group_name: &str,
        repo_name: &str,
    ) -> Result<()> {
        self.block_on(async {
            self.client
                .execute(
                    "DELETE FROM group_repos WHERE group_org = $1 AND group_name = $2 AND repo_name = $3",
                    &[&org, &group_name, &repo_name],
                )
                .await
                .map_err(|e| anyhow::anyhow!("group remove failed: {e:?}"))
        })?;
        Ok(())
    }

    // ── File hashes ──────────────────────────────────────────────────

    pub fn get_file_hashes(&self, repo: &str) -> Result<HashMap<String, String>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT file, sha256 FROM file_hashes WHERE repo = $1",
                    &[&repo],
                )
                .await
                .map_err(|e| anyhow::anyhow!("get file hashes failed: {e:?}"))
        })?;

        let mut map = HashMap::new();
        for row in &rows {
            let file: String = row.get(0);
            let hash: String = row.get(1);
            map.insert(file, hash);
        }
        Ok(map)
    }

    pub fn upsert_file_hashes(&self, repo: &str, hashes: &HashMap<String, String>) -> Result<()> {
        if hashes.is_empty() {
            return Ok(());
        }
        const CHUNK_SIZE: usize = 1000;
        let items: Vec<(&String, &String)> = hashes.iter().collect();
        for chunk in items.chunks(CHUNK_SIZE) {
            let mut sql = String::from("INSERT INTO file_hashes (repo, file, sha256) VALUES ");
            let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
            for (i, (file, hash)) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                let base = i * 3;
                sql.push_str(&format!("(${}, ${}, ${})", base + 1, base + 2, base + 3));
                params.push(Box::new(repo.to_string()));
                params.push(Box::new((*file).clone()));
                params.push(Box::new((*hash).clone()));
            }
            sql.push_str(" ON CONFLICT (repo, file) DO UPDATE SET sha256 = EXCLUDED.sha256");
            let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                params.iter().map(|p| p.as_ref()).collect();
            self.block_on(async {
                self.client
                    .execute(&sql as &str, &param_refs)
                    .await
                    .map_err(|e| anyhow::anyhow!("bulk upsert file hashes failed: {e:?}"))
            })?;
        }
        Ok(())
    }

    pub fn delete_file_hashes(&self, repo: &str, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        self.block_on(async {
            self.client
                .execute(
                    "DELETE FROM file_hashes WHERE repo = $1 AND file = ANY($2)",
                    &[&repo, &files],
                )
                .await
                .map_err(|e| anyhow::anyhow!("bulk delete file hashes failed: {e:?}"))
        })?;
        Ok(())
    }

    // ── Session operations ───────────────────────────────────────────

    pub fn save_session(&self, session: &SessionData) -> Result<()> {
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO sessions (id, name, summary, pending_tasks, decisions, \
                     files_touched, constraints_text, assumptions, blockers, confidence, \
                     created_at, updated_at, last_accessed) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                     ON CONFLICT (id) DO UPDATE SET \
                       name = EXCLUDED.name, summary = EXCLUDED.summary, \
                       pending_tasks = EXCLUDED.pending_tasks, decisions = EXCLUDED.decisions, \
                       files_touched = EXCLUDED.files_touched, constraints_text = EXCLUDED.constraints_text, \
                       assumptions = EXCLUDED.assumptions, blockers = EXCLUDED.blockers, \
                       confidence = EXCLUDED.confidence, updated_at = EXCLUDED.updated_at, \
                       last_accessed = EXCLUDED.last_accessed",
                    &[
                        &session.id,
                        &session.name,
                        &session.summary,
                        &session.pending_tasks,
                        &session.decisions,
                        &session.files_touched,
                        &session.constraints,
                        &session.assumptions,
                        &session.blockers,
                        &session.confidence,
                        &session.created_at,
                        &session.updated_at,
                        &session.last_accessed,
                    ],
                )
                .await
                .map_err(|e| anyhow::anyhow!("save session failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<SessionData>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT id, name, summary, pending_tasks, decisions, files_touched, \
                         constraints_text, assumptions, blockers, confidence, created_at, \
                         updated_at, last_accessed \
                         FROM sessions WHERE id = $1",
                    &[&session_id],
                )
                .await
                .map_err(|e| anyhow::anyhow!("load session failed: {e:?}"))
        })?;

        Ok(rows.first().map(row_to_session))
    }

    pub fn list_sessions_recent(&self, limit: usize) -> Result<Vec<SessionData>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT id, name, summary, pending_tasks, decisions, files_touched, \
                         constraints_text, assumptions, blockers, confidence, created_at, \
                         updated_at, last_accessed \
                         FROM sessions ORDER BY updated_at DESC LIMIT $1",
                    &[&(limit as i64)],
                )
                .await
                .map_err(|e| anyhow::anyhow!("list sessions failed: {e:?}"))
        })?;

        Ok(rows.iter().map(row_to_session).collect())
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.block_on(async {
            self.client
                .execute("DELETE FROM sessions WHERE id = $1", &[&session_id])
                .await
                .map_err(|e| anyhow::anyhow!("delete session failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn touch_session(&self, session_id: &str, now_epoch: i64) -> Result<()> {
        self.block_on(async {
            self.client
                .execute(
                    "UPDATE sessions SET last_accessed = $1 WHERE id = $2",
                    &[&now_epoch, &session_id],
                )
                .await
                .map_err(|e| anyhow::anyhow!("touch session failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn purge_expired_sessions(&self, now_epoch: i64) -> Result<Vec<String>> {
        let all = self.list_sessions_recent(10000)?;
        let mut deleted = Vec::new();
        for session in &all {
            if session.compute_confidence(now_epoch) < 0.1 {
                self.delete_session(&session.id)?;
                deleted.push(session.id.clone());
            }
        }
        Ok(deleted)
    }

    // ── Embedding operations ─────────────────────────────────────────

    pub fn upsert_embedding(&self, id: &str, kind: &str, vec: &[f32]) -> Result<()> {
        let v = Vector::from(vec.to_vec());
        self.block_on(async {
            self.client
                .execute(
                    "INSERT INTO embeddings (id, kind, vector) VALUES ($1, $2, $3) \
                     ON CONFLICT (id) DO UPDATE SET kind = EXCLUDED.kind, vector = EXCLUDED.vector",
                    &[&id, &kind, &v],
                )
                .await
                .map_err(|e| anyhow::anyhow!("upsert embedding failed: {e:?}"))
        })?;
        Ok(())
    }

    pub fn upsert_embeddings_bulk(
        &self,
        embeddings: &[(String, Vec<f32>)],
        kind: &str,
    ) -> Result<usize> {
        if embeddings.is_empty() {
            return Ok(0);
        }
        const CHUNK_SIZE: usize = 1000;
        let mut total = 0usize;
        for chunk in embeddings.chunks(CHUNK_SIZE) {
            let mut sql = String::from("INSERT INTO embeddings (id, kind, vector) VALUES ");
            let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
            for (i, (id, vec)) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                let base = i * 3;
                sql.push_str(&format!("(${}, ${}, ${})", base + 1, base + 2, base + 3));
                params.push(Box::new(id.clone()));
                params.push(Box::new(kind.to_string()));
                params.push(Box::new(Vector::from(vec.clone())));
            }
            sql.push_str(
                " ON CONFLICT (id) DO UPDATE SET kind = EXCLUDED.kind, vector = EXCLUDED.vector",
            );
            let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                params.iter().map(|p| p.as_ref()).collect();
            self.block_on(async {
                self.client
                    .execute(&sql as &str, &param_refs)
                    .await
                    .map_err(|e| anyhow::anyhow!("bulk upsert embeddings failed: {e:?}"))
            })?;
            total += chunk.len();
        }
        Ok(total)
    }

    pub fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let rows = self.block_on(async {
            self.client
                .query("SELECT vector FROM embeddings WHERE id = $1", &[&id])
                .await
                .map_err(|e| anyhow::anyhow!("get embedding failed: {e:?}"))
        })?;
        Ok(rows.first().map(|r| {
            let v: Vector = r.get(0);
            v.to_vec()
        }))
    }

    pub fn delete_embeddings(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        const BATCH: usize = 500;
        for chunk in ids.chunks(BATCH) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("${}", i)).collect();
            let sql = format!(
                "DELETE FROM embeddings WHERE id IN ({})",
                placeholders.join(", ")
            );
            let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = chunk
                .iter()
                .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            self.block_on(async {
                self.client
                    .execute(&sql, &params)
                    .await
                    .map_err(|e| anyhow::anyhow!("delete embeddings failed: {e:?}"))
            })?;
        }
        Ok(())
    }

    /// Nearest-neighbor search using pgvector HNSW index with cosine distance.
    pub fn search_nearest(
        &self,
        query_vec: &[f32],
        kind: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        let qv = Vector::from(query_vec.to_vec());
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT id, vector <=> $1 AS distance \
                         FROM embeddings WHERE kind = $2 \
                         ORDER BY vector <=> $1 LIMIT $3",
                    &[&qv, &kind, &(limit as i64)],
                )
                .await
                .map_err(|e| anyhow::anyhow!("search nearest failed: {e:?}"))
        })?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                let id: String = r.get(0);
                let dist: f64 = r.get(1);
                Some((id, dist as f32))
            })
            .collect())
    }

    pub fn all_embeddings(&self, kind: &str) -> Result<Vec<(String, Vec<f32>)>> {
        let rows = self.block_on(async {
            self.client
                .query(
                    "SELECT id, vector FROM embeddings WHERE kind = $1",
                    &[&kind],
                )
                .await
                .map_err(|e| anyhow::anyhow!("all embeddings failed: {e:?}"))
        })?;
        Ok(rows
            .iter()
            .map(|r| {
                let id: String = r.get(0);
                let v: Vector = r.get(1);
                (id, v.to_vec())
            })
            .collect())
    }

    pub fn all_embedding_ids(&self, kind: &str) -> Result<Vec<String>> {
        let rows = self.block_on(async {
            self.client
                .query("SELECT id FROM embeddings WHERE kind = $1", &[&kind])
                .await
                .map_err(|e| anyhow::anyhow!("all embedding ids failed: {e:?}"))
        })?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub fn embedding_count(&self, kind: &str) -> Result<i64> {
        let row = self.block_on(async {
            self.client
                .query_one("SELECT COUNT(*) FROM embeddings WHERE kind = $1", &[&kind])
                .await
                .map_err(|e| anyhow::anyhow!("embedding count failed: {e:?}"))
        })?;
        Ok(row.get(0))
    }
}

fn row_to_session(r: &tokio_postgres::Row) -> SessionData {
    SessionData {
        id: r.get(0),
        name: r.get(1),
        summary: r.get(2),
        pending_tasks: r.get(3),
        decisions: r.get(4),
        files_touched: r.get(5),
        constraints: r.get(6),
        assumptions: r.get(7),
        blockers: r.get(8),
        confidence: r.get(9),
        created_at: r.get(10),
        updated_at: r.get(11),
        last_accessed: r.get(12),
    }
}
