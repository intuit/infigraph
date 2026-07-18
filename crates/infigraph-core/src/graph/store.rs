use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use fs2::FileExt;
use kuzu::{Connection, Database, SystemConfig};

use super::schema::{CREATE_SCHEMA, MIGRATIONS};
use super::store_util::escape;

/// RAII guard for exclusive write access to the graph store.
/// Holds an advisory file lock on `<db_path>.lock`.
pub struct WriteLock {
    _file: std::fs::File,
}

impl WriteLock {
    fn acquire(lock_path: &Path) -> Result<Self> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        file.lock_exclusive()
            .map_err(|e| anyhow::anyhow!("failed to acquire write lock: {e}"))?;
        Ok(Self { _file: file })
    }

    fn try_acquire(lock_path: &Path) -> Result<Option<Self>> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) =>
            {
                Ok(None)
            }
            Err(e) => Err(anyhow::anyhow!("lock error: {e}")),
        }
    }
}

/// Persistent graph store backed by Kuzu.
pub struct GraphStore {
    db: Database,
    lock_path: PathBuf,
}

/// A freshly-initialized Kuzu DB file is at least this large. A file well
/// below this can't be a real database — most likely truncated/corrupt
/// (e.g. torn write, disk full mid-write). Reject it before calling into
/// Kuzu: a malformed header can make Kuzu's own parser read a bogus size
/// field and request a huge allocation, which some allocators (observed on
/// Linux) abort the whole process on (SIGABRT) rather than erroring —
/// there's no Rust-level Result to catch after that happens.
const MIN_PLAUSIBLE_DB_BYTES: u64 = 4096;

fn reject_implausible_db_file(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && meta.len() < MIN_PLAUSIBLE_DB_BYTES {
            anyhow::bail!(
                "file at {} is truncated/corrupt: only {} bytes, expected at least {MIN_PLAUSIBLE_DB_BYTES} for a valid Kuzu database",
                path.display(),
                meta.len()
            );
        }
    }
    Ok(())
}

impl GraphStore {
    /// Open or create a Kuzu database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        reject_implausible_db_file(path)?;
        let lock_path = path.with_extension("lock");
        let db = Database::new(path, SystemConfig::default())
            .map_err(|e| anyhow::anyhow!("failed to open kuzu db: {e}"))?;
        let store = Self { db, lock_path };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an existing Kuzu database in read-only mode.
    /// Safe for concurrent access while a watcher is writing.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        reject_implausible_db_file(path)?;
        let lock_path = path.with_extension("lock");
        let config = SystemConfig::default()
            .read_only(true)
            .throw_on_wal_replay_failure(false);
        let db = Database::new(path, config)
            .map_err(|e| anyhow::anyhow!("failed to open kuzu db (read-only): {e}"))?;
        Ok(Self { db, lock_path })
    }

    /// Acquire exclusive write lock. Blocks until available.
    pub fn write_lock(&self) -> Result<WriteLock> {
        WriteLock::acquire(&self.lock_path)
    }

    /// Try to acquire write lock without blocking. Returns None if already held.
    pub fn try_write_lock(&self) -> Result<Option<WriteLock>> {
        WriteLock::try_acquire(&self.lock_path)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.connection()?;
        for ddl in CREATE_SCHEMA {
            conn.query(ddl)
                .map_err(|e| anyhow::anyhow!("schema error: {e}\n  DDL: {ddl}"))?;
        }
        for migration in MIGRATIONS {
            let _ = conn.query(migration);
        }
        Ok(())
    }

    pub fn connection(&self) -> Result<Connection<'_>> {
        Connection::new(&self.db).map_err(|e| anyhow::anyhow!("failed to create connection: {e}"))
    }

    /// Remove all graph data for a deleted file.
    pub fn remove_file(&self, file: &str) -> Result<()> {
        let _lock = self.write_lock()?;
        let conn = self.connection()?;
        self.remove_file_conn(&conn, file)
    }

    /// Caller must hold WriteLock.
    pub fn remove_file_conn(&self, conn: &Connection<'_>, file: &str) -> Result<()> {
        let _ = conn.query(&format!(
            "MATCH (f:File)-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) WHERE f.id = '{}' DETACH DELETE st",
            escape(file)
        ));
        let _ = conn.query(&format!(
            "MATCH (s:Symbol) WHERE s.file = '{}' DETACH DELETE s",
            escape(file)
        ));
        let _ = conn.query(&format!(
            "MATCH (m:Module) WHERE m.file = '{}' DETACH DELETE m",
            escape(file)
        ));
        let _ = conn.query(&format!(
            "MATCH (f:File) WHERE f.id = '{}' DETACH DELETE f",
            escape(file)
        ));
        Ok(())
    }

    /// Remove all files whose path starts with the given prefix (handles directory removal).
    pub fn remove_files_by_prefix(&self, prefix: &str) -> Result<usize> {
        let _lock = self.write_lock()?;
        let conn = self.connection()?;
        let escaped = escape(prefix);
        let result = conn
            .query(&format!(
                "MATCH (f:File) WHERE f.id STARTS WITH '{escaped}' RETURN f.id"
            ))
            .map_err(|e| anyhow::anyhow!("query files by prefix: {e}"))?;
        let mut files = Vec::new();
        for row in result {
            if let Some(val) = row.first() {
                files.push(val.to_string());
            }
        }
        for f in &files {
            self.remove_file_conn(&conn, f)?;
        }
        Ok(files.len())
    }

    /// Return map of file path -> content_hash for all indexed modules.
    /// Used by incremental indexing to skip unchanged files.
    pub fn get_file_hashes(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let result = conn
            .query("MATCH (m:Module) RETURN m.file, m.content_hash")
            .map_err(|e| anyhow::anyhow!("get_file_hashes failed: {e}"))?;
        let mut map = HashMap::new();
        for row in result {
            if row.len() >= 2 {
                map.insert(row[0].to_string(), row[1].to_string());
            }
        }
        Ok(map)
    }

    /// Return all symbols as (name, id, file, kind) tuples -- used by resolve_calls.
    pub fn get_all_symbols(&self) -> Result<Vec<(String, String, String, String)>> {
        let conn = self.connection()?;
        let result = conn
            .query("MATCH (s:Symbol) RETURN s.name, s.id, s.file, s.kind")
            .map_err(|e| anyhow::anyhow!("get_all_symbols failed: {e}"))?;
        let mut symbols = Vec::new();
        for row in result {
            if row.len() >= 4 {
                symbols.push((
                    row[0].to_string(),
                    row[1].to_string(),
                    row[2].to_string(),
                    row[3].to_string(),
                ));
            }
        }
        Ok(symbols)
    }

    /// Get total counts for stats.
    pub fn derive_tested_by_edges(&self) -> Result<usize> {
        let _lock = self.write_lock()?;
        let conn = self.connection()?;
        let q = super::queries::GraphQuery::new(&conn);
        q.derive_tested_by_edges()
    }

    pub fn stats(&self) -> Result<GraphStats> {
        let conn = self.connection()?;

        let symbol_count = count_query(&conn, "MATCH (s:Symbol) RETURN count(s)")?;
        let module_count = count_query(&conn, "MATCH (m:Module) RETURN count(m)")?;
        let file_count = count_query(&conn, "MATCH (f:File) RETURN count(f)")?;
        let folder_count = count_query(&conn, "MATCH (d:Folder) RETURN count(d)")?;
        let calls_count = count_query(&conn, "MATCH ()-[r:CALLS]->() RETURN count(r)")?;
        let inherits_count = count_query(&conn, "MATCH ()-[r:INHERITS]->() RETURN count(r)")?;
        let contains_count = count_query(&conn, "MATCH ()-[r:CONTAINS]->() RETURN count(r)")?;

        Ok(GraphStats {
            symbols: symbol_count,
            modules: module_count,
            files: file_count,
            folders: folder_count,
            calls: calls_count,
            inherits: inherits_count,
            contains: contains_count,
        })
    }
}

#[derive(Debug)]
pub struct GraphStats {
    pub symbols: u64,
    pub modules: u64,
    pub files: u64,
    pub folders: u64,
    pub calls: u64,
    pub inherits: u64,
    pub contains: u64,
}

impl std::fmt::Display for GraphStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Graph Statistics:")?;
        writeln!(f, "  Symbols:      {}", self.symbols)?;
        writeln!(f, "  Modules:      {}", self.modules)?;
        writeln!(f, "  Files:        {}", self.files)?;
        writeln!(f, "  Folders:      {}", self.folders)?;
        writeln!(f, "  Calls edges:  {}", self.calls)?;
        writeln!(f, "  Inherits:     {}", self.inherits)?;
        writeln!(f, "  Contains:     {}", self.contains)
    }
}

fn count_query(conn: &Connection, query: &str) -> Result<u64> {
    let mut result = conn
        .query(query)
        .map_err(|e| anyhow::anyhow!("query failed: {e}"))?;
    if let Some(row) = result.next() {
        if let Some(val) = row.first() {
            return Ok(val.to_string().parse().unwrap_or(0));
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: opening a truncated/garbage file must return a
    /// clean error, not crash the process. On Linux, handing a tiny
    /// malformed file straight to Kuzu can make its parser read a bogus
    /// size field from the garbage bytes and abort on an implausible
    /// allocation request (observed: ~92GB) before any Rust Result exists
    /// to catch it.
    #[test]
    fn test_open_rejects_truncated_file_without_crashing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("corrupt.kuzu");
        std::fs::write(&path, b"not a real kuzu database").unwrap();

        match GraphStore::open(&path) {
            Ok(_) => panic!("opening a truncated file should error, not succeed"),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                assert!(
                    msg.contains("truncated") || msg.contains("corrupt"),
                    "error should be classifiable as corruption, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn test_open_read_only_rejects_truncated_file_without_crashing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("corrupt.kuzu");
        std::fs::write(&path, b"not a real kuzu database").unwrap();

        let result = GraphStore::open_read_only(&path);
        assert!(
            result.is_err(),
            "opening a truncated file read-only should error, not succeed"
        );
    }
}
