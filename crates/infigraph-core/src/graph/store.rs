use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kuzu::{Connection, Database, SystemConfig};

use super::schema::{CREATE_SCHEMA, MIGRATIONS};
use super::store_util::escape;
use crate::lockfile::{self, LockFile};

/// RAII guard for exclusive write access to the graph store.
/// Holds an advisory file lock on `<db_path>.lock` with an identity
/// payload (see `crate::lockfile`).
#[derive(Debug)]
pub struct WriteLock {
    _guard: LockFile,
}

/// Role string stamped into the graph write lock's identity payload.
const GRAPH_WRITE_ROLE: &str = "graph-write";

/// Default wait budget for the graph write lock. Individual write calls
/// are short; 30s of waiting means something is wedged — surface it.
const GRAPH_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl WriteLock {
    fn acquire(lock_path: &Path) -> Result<Self> {
        Self::acquire_with_timeout(lock_path, GRAPH_WRITE_TIMEOUT)
    }

    fn acquire_with_timeout(lock_path: &Path, timeout: std::time::Duration) -> Result<Self> {
        let guard = lockfile::acquire(lock_path, GRAPH_WRITE_ROLE, timeout)?;
        Ok(Self { _guard: guard })
    }

    fn try_acquire(lock_path: &Path) -> Result<Option<Self>> {
        Ok(lockfile::try_acquire(lock_path, GRAPH_WRITE_ROLE)?.map(|guard| Self { _guard: guard }))
    }
}

/// Every WAL-family sibling of the database at `db_path` that currently
/// exists: `<db>.wal` plus the `<db>.wal.*` family.
///
/// Kuzu's on-disk WAL filename APPENDS ".wal" to the full db filename
/// (e.g. "graph" -> "graph.wal", "docs.kuzu" -> "docs.kuzu.wal"). It does
/// NOT replace the extension the way `Path::with_extension` does.
fn wal_family_paths(db_path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let wal = PathBuf::from(format!("{}.wal", db_path.display()));
    if wal.exists() {
        found.push(wal);
    }
    if let (Some(parent), Some(name)) = (db_path.parent(), db_path.file_name()) {
        let prefix = format!("{}.wal.", name.to_string_lossy());
        if let Ok(entries) = std::fs::read_dir(parent) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    found.push(e.path());
                }
            }
        }
    }
    found
}

/// Whether a process with this PID is currently running.
fn pid_is_alive(pid: u32) -> bool {
    let spid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[spid]), true);
    sys.process(spid).is_some()
}

/// If `db_path`'s graph shows signs of an unclean shutdown that can make
/// Kuzu's WAL-replay-on-open crash the whole process, returns the dead
/// holder's PID. `None` means it's safe to proceed to `Database::new` as
/// normal.
///
/// Observed directly: a stale WAL left by a process that died without a
/// clean checkpoint made `Database::new`'s WAL replay
/// (`WALReplayer::replayNodeTableInsertRecord` -> `NodeTable::insert` ->
/// `DiskArrayInternal::get`) hit `SIGBUS`/`EXC_BAD_ACCESS` and abort the
/// whole process -- on a plain **read-only** open reached by an ordinary
/// query, not just a risky write path, and before any `Result` existed for
/// calling code to catch.
///
/// Two signals together, deliberately not either alone:
/// - A WAL-family sibling exists (`wal_family_paths`) -- Kuzu did not
///   complete a clean checkpoint before this graph was last closed. This
///   alone is completely routine: a live writer mid-transaction, or a
///   replay that would succeed fine, both leave a WAL sibling in place.
/// - The write lock's recorded holder is confirmed dead (the OS reports no
///   such process running right now). This is what turns "needs replay"
///   (routine) into "the process that would have driven that
///   replay/checkpoint died before finishing it" (suspect).
///
/// Requiring both keeps this from flagging the common, harmless case (a
/// live writer's WAL, or a replay that would just work) while still
/// catching the crash scenario above. A lock file that's absent, empty, or
/// unparseable reads as "can't confirm a dead holder" and does not flag --
/// conservative by design, since a false positive here means refusing to
/// open a perfectly good graph.
fn unclean_shutdown_wal_holder(db_path: &Path, lock_path: &Path) -> Option<u32> {
    if wal_family_paths(db_path).is_empty() {
        return None;
    }
    let holder = lockfile::read_holder(lock_path)?;
    if pid_is_alive(holder.pid) {
        return None; // holder is alive -- not our call to intervene
    }
    Some(holder.pid)
}

/// Persistent graph store backed by Kuzu.
pub struct GraphStore {
    db: Database,
    lock_path: PathBuf,
}

impl GraphStore {
    /// Open or create a Kuzu database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_extension("lock");
        if let Some(pid) = unclean_shutdown_wal_holder(path, &lock_path) {
            anyhow::bail!(
                "graph {} has an unreplayed WAL from process {pid}, which is no longer \
                 running (unclean shutdown) -- refusing to open it directly, since WAL \
                 replay in this state has been observed to crash the whole process with \
                 SIGBUS; delete the graph directory to rebuild, or restore from a backup",
                path.display()
            );
        }
        let db = Database::new(path, SystemConfig::default())
            .map_err(|e| anyhow::anyhow!("failed to open kuzu db: {e}"))?;
        let store = Self { db, lock_path };
        store.init_schema()?;
        Ok(store)
    }

    /// Directory containing the graph database files. Used for disk-space
    /// preflight checks before a large write (see `store_util::check_disk_headroom`)
    /// -- Kuzu aborts the whole process with an uncaught C++ exception on
    /// ENOSPC mid-transaction rather than surfacing a Rust `Result`, so
    /// callers doing a large bulk write must check headroom themselves
    /// first (observed on sittir: SCIP enrichment ran the volume out of
    /// space mid-COPY and crashed the process).
    pub fn db_dir(&self) -> Option<&Path> {
        self.lock_path.parent()
    }

    /// Open an existing Kuzu database in read-only mode.
    /// Safe for concurrent access while a watcher is writing.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let lock_path = path.with_extension("lock");
        if let Some(pid) = unclean_shutdown_wal_holder(path, &lock_path) {
            anyhow::bail!(
                "graph {} has an unreplayed WAL from process {pid}, which is no longer \
                 running (unclean shutdown) -- refusing to open it directly, since WAL \
                 replay in this state has been observed to crash the whole process with \
                 SIGBUS; rebuild the graph or restore from a backup",
                path.display()
            );
        }
        let config = SystemConfig::default()
            .read_only(true)
            .throw_on_wal_replay_failure(false);
        let db = Database::new(path, config)
            .map_err(|e| anyhow::anyhow!("failed to open kuzu db (read-only): {e}"))?;
        Ok(Self { db, lock_path })
    }

    /// Acquire exclusive write lock. Waits up to 30s, returning `Busy` if
    /// still held at expiry.
    pub fn write_lock(&self) -> Result<WriteLock> {
        WriteLock::acquire(&self.lock_path)
    }

    /// Acquire the write lock with a caller-chosen wait budget.
    pub fn write_lock_with_timeout(&self, timeout: std::time::Duration) -> Result<WriteLock> {
        WriteLock::acquire_with_timeout(&self.lock_path, timeout)
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
mod unclean_shutdown_wal_tests {
    use super::*;

    fn write_holder_lock(lock_path: &Path, pid: u32) {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let info = lockfile::LockInfo {
            pid,
            role: "test".to_string(),
            build_hash: "test".to_string(),
            acquired_at: 0,
        };
        std::fs::write(lock_path, serde_json::to_string(&info).unwrap()).unwrap();
    }

    /// A PID essentially guaranteed not to be a running process, standing
    /// in for "the write lock's recorded holder is dead" across these tests.
    const DEAD_PID: u32 = 999_999;

    #[test]
    fn unclean_shutdown_wal_holder_flags_wal_plus_dead_holder() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph");
        std::fs::write(dir.path().join("graph.wal"), b"wal").unwrap();
        let lock_path = db_path.with_extension("lock");
        write_holder_lock(&lock_path, DEAD_PID);

        assert_eq!(
            unclean_shutdown_wal_holder(&db_path, &lock_path),
            Some(DEAD_PID)
        );
    }

    #[test]
    fn unclean_shutdown_wal_holder_ignores_a_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph");
        std::fs::write(dir.path().join("graph.wal"), b"wal").unwrap();
        let lock_path = db_path.with_extension("lock");
        write_holder_lock(&lock_path, std::process::id());

        assert_eq!(
            unclean_shutdown_wal_holder(&db_path, &lock_path),
            None,
            "a live writer's WAL is routine, not a signal to refuse opening"
        );
    }

    #[test]
    fn unclean_shutdown_wal_holder_ignores_no_wal_even_with_a_dead_holder() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph");
        let lock_path = db_path.with_extension("lock");
        write_holder_lock(&lock_path, DEAD_PID);

        assert_eq!(
            unclean_shutdown_wal_holder(&db_path, &lock_path),
            None,
            "a dead holder alone (no WAL) means nothing was left mid-replay"
        );
    }

    #[test]
    fn unclean_shutdown_wal_holder_ignores_a_wal_with_no_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph");
        std::fs::write(dir.path().join("graph.wal"), b"wal").unwrap();
        let lock_path = db_path.with_extension("lock");

        assert_eq!(
            unclean_shutdown_wal_holder(&db_path, &lock_path),
            None,
            "can't confirm a dead holder without a lock payload to read -- conservative by design"
        );
    }

    /// Regression test: a stale WAL from a dead process used to be handed
    /// straight to `kuzu::Database::new`, which crashed the whole process
    /// with SIGBUS deep inside WAL replay -- before any `Result` existed to
    /// catch it. Both `open` and `open_read_only` must refuse up front
    /// instead.
    #[test]
    fn open_refuses_a_graph_with_an_unreplayed_wal_from_a_dead_process() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph");
        std::fs::write(
            &db_path,
            b"not a real kuzu db, doesn't matter for this test",
        )
        .unwrap();
        std::fs::write(dir.path().join("graph.wal"), b"wal").unwrap();
        write_holder_lock(&db_path.with_extension("lock"), DEAD_PID);

        let err = GraphStore::open(&db_path)
            .map(|_| ())
            .expect_err("must refuse rather than attempt Database::new");
        assert!(err.to_string().contains("unreplayed WAL"), "{err}");
        assert!(err.to_string().contains(&DEAD_PID.to_string()), "{err}");

        let err = GraphStore::open_read_only(&db_path)
            .map(|_| ())
            .expect_err("read-only open must refuse too");
        assert!(err.to_string().contains("unreplayed WAL"), "{err}");
    }
}
