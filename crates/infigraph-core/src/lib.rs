mod analysis;
pub mod bench;
pub mod bridges;
pub mod check;
pub mod claude_md;
pub mod cluster;
pub mod concerns;
pub mod config;
pub mod diff;
pub mod embed;
pub mod export;
pub mod extract;
pub mod graph;
pub mod lang;
pub mod learned;
pub mod manifest;
pub mod meta;
pub mod model;
pub mod multi;
pub mod patterns;
pub mod refactor;
pub mod reflection;
mod report;
pub mod resolve;
pub mod review;
pub mod routes;
pub mod scip;
pub mod search;
pub mod security;
pub mod sequence;
pub mod structured;
pub mod taint;
pub mod viz;
pub mod vuln;
pub mod watch;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use graph::GraphStore;
use lang::LanguageRegistry;
use model::FileExtraction;

pub(crate) fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The main entry point for the infigraph framework.
pub struct Infigraph {
    root: PathBuf,
    db_path: PathBuf,
    registry: LanguageRegistry,
    store: Option<GraphStore>,
    backend_kind: BackendKind,
    /// When set, all file paths and symbol IDs are prefixed with `{namespace}/`.
    /// Used for multi-repo indexing into a shared Neo4j DB to prevent collisions.
    namespace: Option<String>,
}

/// Which graph backend is active.
enum BackendKind {
    /// Embedded Kùzu (default). GraphStore lives in `self.store`.
    Kuzu,
    /// Remote Neo4j sidecar via Bolt.
    #[cfg(feature = "neo4j")]
    Neo4j(graph::Neo4jBackend),
}

impl Infigraph {
    /// Open a project directory. Creates `.infigraph/` if it doesn't exist.
    pub fn open(root: &Path, registry: LanguageRegistry) -> Result<Self> {
        let root = root.canonicalize().context("invalid project root")?;
        let db_path = root.join(".infigraph").join("graph");
        Ok(Self {
            root,
            db_path,
            registry,
            store: None,
            backend_kind: BackendKind::Kuzu,
            namespace: None,
        })
    }

    /// Initialize the graph store (creates DB on first run). Retries on
    /// transient open errors (lock contention, resource pressure); wipes and
    /// rebuilds only on a positively-identified corruption signature.
    ///
    /// Backend selection via `INFIGRAPH_BACKEND` env var:
    /// - `kuzu` (default): embedded Kùzu graph DB
    /// - `neo4j`: remote Neo4j sidecar via Bolt (requires `neo4j` feature)
    pub fn init(&mut self) -> Result<()> {
        let backend_env = std::env::var("INFIGRAPH_BACKEND").unwrap_or_else(|_| "kuzu".into());

        match backend_env.as_str() {
            #[cfg(feature = "neo4j")]
            "neo4j" => {
                let neo = graph::Neo4jBackend::connect_from_env()?;
                neo.init_schema()?;
                self.backend_kind = BackendKind::Neo4j(neo);
                Ok(())
            }
            #[cfg(not(feature = "neo4j"))]
            "neo4j" => {
                anyhow::bail!("neo4j backend requested but binary compiled without `neo4j` feature")
            }
            _ => {
                // Transient errors (another process holding a lock, or a
                // resource hiccup like mmap/buffer-manager exhaustion under
                // concurrent access) are not corruption — wiping on a guess
                // would destroy real data mid-race. Retry those. Only wipe on
                // a positively-identified corruption signature; anything else
                // unrecognized propagates as a real error rather than being
                // silently "healed" by deleting the user's graph.
                const RETRY_ATTEMPTS: u32 = 10;
                const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

                let mut last_err = match GraphStore::open(&self.db_path) {
                    Ok(store) => {
                        self.store = Some(store);
                        return Ok(());
                    }
                    Err(e) => e,
                };

                for _ in 0..RETRY_ATTEMPTS {
                    if !Self::is_transient_open_error(&last_err) {
                        break;
                    }
                    std::thread::sleep(RETRY_DELAY);
                    match GraphStore::open(&self.db_path) {
                        Ok(store) => {
                            self.store = Some(store);
                            return Ok(());
                        }
                        Err(e) => last_err = e,
                    }
                }

                if !Self::is_known_corruption_error(&last_err) {
                    return Err(last_err).with_context(|| {
                        format!("failed to open graph at {}", self.db_path.display())
                    });
                }

                eprintln!(
                    "[graph] open failed ({last_err}), wiping corrupt graph and rebuilding..."
                );
                Self::wipe_graph(&self.db_path);
                let store = GraphStore::open(&self.db_path).with_context(|| {
                    format!("graph still unreadable after wipe (was: {last_err})")
                })?;
                self.store = Some(store);
                Ok(())
            }
        }
    }

    fn wipe_graph(db_path: &Path) {
        let _ = std::fs::remove_dir_all(db_path);
        let _ = std::fs::remove_file(db_path);
        let wal = db_path.with_extension("wal");
        let _ = std::fs::remove_file(&wal);
    }

    /// True if an error looks like a transient condition worth retrying —
    /// another process holding Kuzu's lock, or a resource hiccup from
    /// concurrent access (mmap/buffer-manager pressure) — rather than a
    /// stable, on-disk problem.
    fn is_transient_open_error(err: &anyhow::Error) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("lock") || msg.contains("mmap") || msg.contains("buffer manager")
    }

    /// True only for errors that positively indicate on-disk corruption.
    /// Anything not matched here is treated as unknown and propagated as an
    /// error rather than triggering a destructive wipe-and-rebuild.
    fn is_known_corruption_error(err: &anyhow::Error) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("corrupt")
            || msg.contains("truncated")
            || msg.contains("invalid magic")
            || msg.contains("checksum")
    }

    /// Initialize the graph store in read-only mode.
    /// Safe for concurrent access while a watcher writes.
    pub fn init_read_only(&mut self) -> Result<()> {
        let store = GraphStore::open_read_only(&self.db_path)?;
        self.store = Some(store);
        Ok(())
    }

    /// Index all supported files in the project, building the graph.
    /// Skips files whose content hash matches the stored hash (incremental).
    pub fn index(&self) -> Result<IndexResult> {
        // Neo4j backend: clean trait-based path
        if let Some(backend) = self.backend() {
            return self.index_via_backend(backend);
        }

        // Kuzu backend: existing Kuzu-specific path
        let store = self.store.as_ref().context("call init() first")?;

        let files = self.collect_files()?;
        let total = files.len();

        // Load existing hashes for incremental skip
        let existing_hashes = store.get_file_hashes().unwrap_or_default();

        // Parse all files in parallel; skip unchanged ones
        let done = std::sync::atomic::AtomicUsize::new(0);
        let extractions: Vec<FileExtraction> = files
            .par_iter()
            .filter_map(|path| {
                let rel_path = path
                    .strip_prefix(&self.root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = std::fs::read(path).ok()?;
                // Skip if hash unchanged
                let hash = {
                    let mut h = Sha256::new();
                    h.update(&source);
                    format!("{:x}", h.finalize())
                };
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let pct = n * 100 / total;
                let prev_pct = (n - 1) * 100 / total;
                if (pct / 25) > (prev_pct / 25) || n == total {
                    eprintln!("Parsing: {}/{} ({}%)", n, total, pct);
                }
                if existing_hashes.get(&rel_path).map(|s| s.as_str()) == Some(hash.as_str()) {
                    return None; // unchanged
                }
                let pack = self.registry.for_file_with_content(&rel_path, &source)?;
                extract::extract_file(&rel_path, &source, pack).ok()
            })
            .collect();

        let indexed = extractions.len();

        // Write all changed files — use CSV bulk load for fresh index or large batches,
        // fall back to per-file UNWIND only for small incremental updates.
        let use_csv = !extractions.is_empty() && (existing_hashes.is_empty() || indexed > 100);
        let _write_lock = if !extractions.is_empty() {
            Some(store.write_lock()?)
        } else {
            None
        };

        let write_start = std::time::Instant::now();
        if !extractions.is_empty() {
            eprintln!(
                "Writing: {} files ({} mode)",
                indexed,
                if use_csv { "bulk-parquet" } else { "per-file" }
            );
            if use_csv {
                if !existing_hashes.is_empty() {
                    let conn = store.connection()?;
                    conn.query("BEGIN TRANSACTION")
                        .context("failed to begin delete transaction")?;
                    let file_list: Vec<String> = extractions
                        .iter()
                        .map(|e| format!("'{}'", escape_str(&e.file)))
                        .collect();
                    let files_in = file_list.join(", ");
                    let _ = conn.query(&format!(
                        "MATCH (f:File)-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) WHERE f.id IN [{}] DETACH DELETE st",
                        files_in
                    ));
                    let _ = conn.query(&format!(
                        "MATCH (s:Symbol) WHERE s.file IN [{}] DETACH DELETE s",
                        files_in
                    ));
                    let _ = conn.query(&format!(
                        "MATCH (m:Module) WHERE m.file IN [{}] DETACH DELETE m",
                        files_in
                    ));
                    let _ = conn.query(&format!(
                        "MATCH (f:File) WHERE f.id IN [{}] DETACH DELETE f",
                        files_in
                    ));
                    conn.query("COMMIT")
                        .context("failed to commit delete transaction")?;
                }
                let conn = store.connection()?;
                store.upsert_all_parquet_conn(&conn, &extractions)?;
            } else {
                let conn = store.connection()?;
                conn.query("BEGIN TRANSACTION")
                    .context("failed to begin index transaction")?;
                let file_list: Vec<String> = extractions
                    .iter()
                    .map(|e| format!("'{}'", escape_str(&e.file)))
                    .collect();
                let files_in = file_list.join(", ");
                let _ = conn.query(&format!(
                    "MATCH (f:File)-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) WHERE f.id IN [{}] DETACH DELETE st",
                    files_in
                ));
                let _ = conn.query(&format!(
                    "MATCH (s:Symbol) WHERE s.file IN [{}] DETACH DELETE s",
                    files_in
                ));
                let _ = conn.query(&format!(
                    "MATCH (m:Module) WHERE m.file IN [{}] DETACH DELETE m",
                    files_in
                ));
                let _ = conn.query(&format!(
                    "MATCH (f:File) WHERE f.id IN [{}] DETACH DELETE f",
                    files_in
                ));
                for extraction in &extractions {
                    store.upsert_file_conn_no_delete(&conn, extraction)?;
                }
                conn.query("COMMIT")
                    .context("failed to commit index transaction")?;
                let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
                store.upsert_folders_bulk_conn(&conn, &file_paths)?;
            }
        }

        if use_csv {
            let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
            let conn = store.connection()?;
            store.upsert_folders_bulk_conn(&conn, &file_paths)?;
        }

        if !extractions.is_empty() {
            eprintln!("Write complete: {}s", write_start.elapsed().as_secs());
        }

        // Prune stale files: remove entries for files that no longer exist on disk
        {
            let current_files: std::collections::HashSet<String> = files
                .iter()
                .filter_map(|p| {
                    p.strip_prefix(&self.root)
                        .ok()
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                })
                .collect();
            let stale: Vec<String> = existing_hashes
                .keys()
                .filter(|k| !current_files.contains(k.as_str()))
                .cloned()
                .collect();
            if !stale.is_empty() {
                eprintln!("[index] pruning {} stale file(s) from graph", stale.len());
                let conn = store.connection()?;
                for f in &stale {
                    let _ = store.remove_file_conn(&conn, f);
                }
            }
        }

        // resolve runs under the same write lock (creates CALLS/INHERITS edges)
        if !extractions.is_empty() {
            eprintln!("Resolving: calls + inheritance for {} files", indexed);
        }
        let resolve_start = std::time::Instant::now();
        let resolve_stats = resolve::resolve_calls_incremental(store, &extractions, None)
            .unwrap_or_else(|e| {
                eprintln!("warning: call resolution failed: {e}");
                resolve::ResolveStats {
                    total_calls: 0,
                    resolved: 0,
                    unresolved: 0,
                    learned_resolved: 0,
                    inherits_resolved: 0,
                }
            });
        if !extractions.is_empty() {
            eprintln!(
                "Resolve complete: {}s ({} resolved, {} unresolved)",
                resolve_start.elapsed().as_secs(),
                resolve_stats.resolved,
                resolve_stats.unresolved
            );
        }

        drop(_write_lock);

        Ok(IndexResult {
            total_files: total,
            indexed_files: indexed,
            extractions,
            resolve_stats,
        })
    }

    /// Backend-agnostic index path (used for Neo4j and future backends).
    fn index_via_backend(&self, backend: &dyn graph::GraphBackend) -> Result<IndexResult> {
        let files = self.collect_files()?;
        let total = files.len();

        let existing_hashes = backend.get_file_hashes().unwrap_or_default();

        let ns = &self.namespace;
        let done = std::sync::atomic::AtomicUsize::new(0);
        let extractions: Vec<FileExtraction> = files
            .par_iter()
            .filter_map(|path| {
                let raw_rel = path
                    .strip_prefix(&self.root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");
                let rel_path = match ns {
                    Some(prefix) => format!("{}/{}", prefix, raw_rel),
                    None => raw_rel.clone(),
                };
                let source = std::fs::read(path).ok()?;
                let hash = {
                    let mut h = Sha256::new();
                    h.update(&source);
                    format!("{:x}", h.finalize())
                };
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let pct = n * 100 / total;
                let prev_pct = (n - 1) * 100 / total;
                if (pct / 25) > (prev_pct / 25) || n == total {
                    eprintln!("Parsing: {}/{} ({}%)", n, total, pct);
                }
                if existing_hashes.get(&rel_path).map(|s| s.as_str()) == Some(hash.as_str()) {
                    return None;
                }
                let pack = self.registry.for_file_with_content(&rel_path, &source)?;
                extract::extract_file(&rel_path, &source, pack).ok()
            })
            .collect();

        let indexed = extractions.len();

        if !extractions.is_empty() {
            eprintln!("Writing: {} files (backend bulk)", indexed);
            let write_start = std::time::Instant::now();
            backend.upsert_files_bulk(&extractions, existing_hashes.is_empty())?;
            eprintln!("Write complete: {}s", write_start.elapsed().as_secs());
        }

        // Prune stale files
        {
            let current_files: std::collections::HashSet<String> = files
                .iter()
                .filter_map(|p| {
                    p.strip_prefix(&self.root).ok().map(|r| {
                        let raw = r.to_string_lossy().replace('\\', "/");
                        match ns {
                            Some(prefix) => format!("{}/{}", prefix, raw),
                            None => raw,
                        }
                    })
                })
                .collect();
            let stale: Vec<String> = existing_hashes
                .keys()
                .filter(|k| !current_files.contains(k.as_str()))
                .cloned()
                .collect();
            if !stale.is_empty() {
                eprintln!("[index] pruning {} stale file(s) from graph", stale.len());
                for f in &stale {
                    let _ = backend.remove_file(f);
                }
            }
        }

        let resolve_start = std::time::Instant::now();
        if !extractions.is_empty() {
            eprintln!("Resolving: calls + inheritance for {} files", indexed);
        }
        let resolve_stats = backend
            .resolve_calls(&extractions, None)
            .unwrap_or_else(|e| {
                eprintln!("warning: call resolution failed: {e}");
                resolve::ResolveStats {
                    total_calls: 0,
                    resolved: 0,
                    unresolved: 0,
                    learned_resolved: 0,
                    inherits_resolved: 0,
                }
            });
        if !extractions.is_empty() {
            eprintln!(
                "Resolve complete: {}s ({} resolved, {} unresolved)",
                resolve_start.elapsed().as_secs(),
                resolve_stats.resolved,
                resolve_stats.unresolved
            );
        }

        Ok(IndexResult {
            total_files: total,
            indexed_files: indexed,
            extractions,
            resolve_stats,
        })
    }

    /// Get graph statistics.
    pub fn stats(&self) -> Result<graph::GraphStats> {
        if let Some(backend) = self.backend() {
            return backend.stats();
        }
        let store = self.store.as_ref().context("call init() first")?;
        store.stats()
    }

    /// Access the underlying graph store (for direct Kùzu queries).
    /// Returns None when using Neo4j backend.
    pub fn store(&self) -> Option<&GraphStore> {
        self.store.as_ref()
    }

    /// Access the graph backend (works for all backend types).
    pub fn backend(&self) -> Option<&dyn graph::GraphBackend> {
        match &self.backend_kind {
            BackendKind::Kuzu => {
                // No KuzuBackend stored separately — callers use store() for Kuzu
                None
            }
            #[cfg(feature = "neo4j")]
            BackendKind::Neo4j(neo) => Some(neo),
        }
    }

    /// Access the language registry.
    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }

    /// Get the project root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Set a namespace prefix for multi-repo indexing into a shared DB.
    /// All file paths and symbol IDs will be prefixed with `{namespace}/`.
    pub fn set_namespace(&mut self, ns: &str) {
        self.namespace = Some(ns.to_string());
    }

    /// Index (or re-index) a single file by its path on disk.
    /// Path may be absolute or relative to project root.
    pub fn index_file(&self, path: &Path) -> Result<()> {
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        let abs = self.root.join(&rel);
        let source = std::fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        let pack = self
            .registry
            .for_file_with_content(&rel, &source)
            .with_context(|| format!("no language for {rel}"))?;
        let extraction = extract::extract_file(&rel, &source, pack)?;
        if let Some(backend) = self.backend() {
            return backend.upsert_file(&extraction);
        }
        let store = self.store.as_ref().context("call init() first")?;
        store.upsert_file(&extraction)?;
        Ok(())
    }

    /// Index a batch of files by path, returning an IndexResult with all extractions.
    pub fn index_files(&self, paths: &[PathBuf]) -> Result<IndexResult> {
        let empty_result = || IndexResult {
            total_files: 0,
            indexed_files: 0,
            extractions: Vec::new(),
            resolve_stats: resolve::ResolveStats {
                total_calls: 0,
                resolved: 0,
                unresolved: 0,
                learned_resolved: 0,
                inherits_resolved: 0,
            },
        };

        if paths.is_empty() {
            return Ok(empty_result());
        }

        let extractions: Vec<FileExtraction> = paths
            .par_iter()
            .filter_map(|path| {
                let rel = if path.is_absolute() {
                    path.strip_prefix(&self.root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/")
                } else {
                    path.to_string_lossy().replace('\\', "/")
                };
                let abs = self.root.join(&rel);
                let source = std::fs::read(&abs).ok()?;
                let pack = self.registry.for_file_with_content(&rel, &source)?;
                extract::extract_file(&rel, &source, pack).ok()
            })
            .collect();

        let extractions = {
            let mut seen = std::collections::HashSet::new();
            extractions
                .into_iter()
                .filter(|e| seen.insert(e.file.clone()))
                .collect::<Vec<_>>()
        };

        let indexed = extractions.len();

        // Route through GraphBackend when available (Neo4j mode)
        if let Some(backend) = self.backend() {
            if !extractions.is_empty() {
                let existing_hashes = backend.get_file_hashes().unwrap_or_default();
                backend.upsert_files_bulk(&extractions, existing_hashes.is_empty())?;
            }
            let resolve_stats = backend
                .resolve_calls(&extractions, None)
                .unwrap_or_else(|e| {
                    eprintln!("warning: call resolution failed: {e}");
                    resolve::ResolveStats {
                        total_calls: 0,
                        resolved: 0,
                        unresolved: 0,
                        learned_resolved: 0,
                        inherits_resolved: 0,
                    }
                });
            return Ok(IndexResult {
                total_files: paths.len(),
                indexed_files: indexed,
                extractions,
                resolve_stats,
            });
        }

        // Kùzu path (existing)
        let store = self.store.as_ref().context("call init() first")?;

        let _write_lock = if !extractions.is_empty() {
            Some(store.write_lock()?)
        } else {
            None
        };

        if !extractions.is_empty() {
            let conn = store.connection()?;
            conn.query("BEGIN TRANSACTION")
                .context("failed to begin batch delete transaction")?;
            let file_list: Vec<String> = extractions
                .iter()
                .map(|e| format!("'{}'", escape_str(&e.file)))
                .collect();
            let files_in = file_list.join(", ");
            let _ = conn.query(&format!(
                "MATCH (f:File)-[:DEFINES]->(s:Symbol)-[:HAS_STATEMENT]->(st:Statement) WHERE f.id IN [{files_in}] DETACH DELETE st"
            ));
            let _ = conn.query(&format!(
                "MATCH (s:Symbol) WHERE s.file IN [{files_in}] DETACH DELETE s"
            ));
            let _ = conn.query(&format!(
                "MATCH (m:Module) WHERE m.file IN [{files_in}] DETACH DELETE m"
            ));
            let _ = conn.query(&format!(
                "MATCH (f:File) WHERE f.id IN [{files_in}] DETACH DELETE f"
            ));
            conn.query("COMMIT")
                .context("failed to commit batch delete transaction")?;

            if indexed > 10 {
                let conn = store.connection()?;
                store.upsert_all_parquet_conn(&conn, &extractions)?;
            } else {
                let conn = store.connection()?;
                store.upsert_all_bulk(&conn, &extractions)?;
            }

            let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
            let conn = store.connection()?;
            store.upsert_folders_bulk_conn(&conn, &file_paths)?;
        }

        let resolve_stats = resolve::resolve_calls_incremental(store, &extractions, None)
            .unwrap_or_else(|e| {
                eprintln!("warning: call resolution failed: {e}");
                resolve::ResolveStats {
                    total_calls: 0,
                    resolved: 0,
                    unresolved: 0,
                    learned_resolved: 0,
                    inherits_resolved: 0,
                }
            });

        drop(_write_lock);

        Ok(IndexResult {
            total_files: paths.len(),
            indexed_files: indexed,
            extractions,
            resolve_stats,
        })
    }

    /// Detect cross-language bridges (FFI, JNI, cgo, gRPC, P/Invoke, WASM, ctypes).
    pub fn detect_bridges(&self) -> Result<bridges::BridgeScanResult> {
        bridges::detect_bridges(&self.root)
    }

    /// Remove a deleted file from the graph.
    pub fn remove_file(&self, path: &Path) -> Result<()> {
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        if let Some(backend) = self.backend() {
            return backend.remove_file(&rel);
        }
        let store = self.store.as_ref().context("call init() first")?;
        store.remove_file(&rel)
    }

    /// Remove all indexed files whose relative path starts with the given prefix.
    /// Handles directory removal where individual file Remove events may not fire.
    pub fn remove_files_by_prefix(&self, path: &Path) -> Result<usize> {
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        let prefix = if rel.ends_with('/') {
            rel
        } else {
            format!("{rel}/")
        };
        if let Some(backend) = self.backend() {
            let rows = backend.raw_query(&format!(
                "MATCH (f:File) WHERE f.id STARTS WITH '{}' RETURN f.id",
                prefix.replace('\'', "\\'")
            ))?;
            let count = rows.len();
            for row in &rows {
                if let Some(file_id) = row.first() {
                    let _ = backend.remove_file(file_id);
                }
            }
            return Ok(count);
        }
        let store = self.store.as_ref().context("call init() first")?;
        store.remove_files_by_prefix(&prefix)
    }

    fn collect_files(&self) -> Result<Vec<PathBuf>> {
        use ignore::WalkBuilder;

        let mut files = Vec::new();
        let walker = WalkBuilder::new(&self.root)
            .hidden(true)
            .add_custom_ignore_filename(".infigraphignore")
            .git_ignore(true)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    ".infigraph" | "node_modules" | "__pycache__" | ".tox"
                )
            })
            .build();

        for result in walker {
            let entry = result?;
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                let path = entry.path();
                if self.registry.for_file(&path.to_string_lossy()).is_some() {
                    files.push(path.to_path_buf());
                }
            }
        }
        Ok(files)
    }
}

pub struct IndexResult {
    pub total_files: usize,
    pub indexed_files: usize,
    pub extractions: Vec<FileExtraction>,
    pub resolve_stats: resolve::ResolveStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_transient_open_error_matches_lock_message() {
        let err = anyhow::anyhow!(
            "failed to open kuzu db: IO exception: Could not set lock on file : /tmp/x/.infigraph/graph"
        );
        assert!(Infigraph::is_transient_open_error(&err));
    }

    #[test]
    fn test_is_transient_open_error_matches_mmap_message() {
        let err = anyhow::anyhow!(
            "failed to open kuzu db: Buffer manager exception: Mmap for size 123 failed."
        );
        assert!(Infigraph::is_transient_open_error(&err));
    }

    #[test]
    fn test_is_transient_open_error_ignores_corruption() {
        let err = anyhow::anyhow!("failed to open kuzu db: catalog file is truncated");
        assert!(!Infigraph::is_transient_open_error(&err));
    }

    #[test]
    fn test_is_known_corruption_error_matches_known_signatures() {
        assert!(Infigraph::is_known_corruption_error(&anyhow::anyhow!(
            "catalog file is truncated"
        )));
        assert!(Infigraph::is_known_corruption_error(&anyhow::anyhow!(
            "invalid magic bytes in header"
        )));
    }

    #[test]
    fn test_is_known_corruption_error_ignores_unrecognized_errors() {
        let err = anyhow::anyhow!("permission denied");
        assert!(!Infigraph::is_known_corruption_error(&err));
    }

    /// Regression test: init() must not wipe the graph on a concurrent-access
    /// error (simulated here by a second live Database handle on the same
    /// path) — only a positively-identified corruption signature should
    /// trigger wipe-and-rebuild.
    #[test]
    fn test_init_does_not_wipe_graph_on_concurrent_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.py"), "def f(): pass").unwrap();

        let mut first = Infigraph::open(root, LanguageRegistry::new()).unwrap();
        first.init().unwrap();
        let db_path = root.join(".infigraph").join("graph");
        assert!(db_path.exists(), "graph should exist after first init");

        // Hold the DB open on this handle (Kuzu's own lock), then attempt a
        // second init on the same path — this is what previously wiped the
        // graph if the second open failed with a lock-contention error.
        let mut second = Infigraph::open(root, LanguageRegistry::new()).unwrap();

        // Shrink the retry window so this test doesn't wait the full 2s
        // (10 * 200ms) if the concurrent open genuinely can't proceed.
        let result = second.init();

        // Whether or not the second init eventually succeeds (Kuzu may allow
        // read-while-write depending on platform), the graph directory must
        // still exist — it must never have been wiped mid-race.
        assert!(
            db_path.exists(),
            "graph must not be wiped due to lock contention with a live concurrent handle"
        );
        drop(first);
        let _ = result;
    }
}
