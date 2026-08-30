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
pub mod lockfile;
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

use graph::GraphStore;
use lang::LanguageRegistry;
use model::FileExtraction;

pub(crate) fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Short git SHA this binary was built from ("-dirty" when the tree had
/// uncommitted changes; "unknown" outside a git checkout). Stamped into
/// lock-file identity payloads so stale-binary holders are identifiable.
pub fn build_hash() -> &'static str {
    env!("INFIGRAPH_BUILD_HASH")
}

/// Kuzu's IO-layer error for "another process holds this database's lock"
/// (see docs.ladybugdb.com/concurrency) is lock contention, not corruption.
/// `GraphStore::open` collapses the underlying Kuzu error into a stringified
/// `anyhow::Error` before it reaches `Infigraph::init`, so there's no
/// structured error variant to match on here -- only Kuzu's own error text.
/// This was previously indistinguishable from genuine corruption, so a
/// second `infigraph` process opening a graph while a watcher already had
/// it open would trigger `wipe_graph`, destroying the watcher's live data.
fn is_lock_contention_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("Could not set lock on file")
}

/// Open the Kùzu graph, retrying briefly on lock contention (AIF3X-331 #36).
///
/// The graph is single-writer: while a watcher's per-reindex write holds the
/// lock (~milliseconds), a concurrent open fails with "Could not set lock on
/// file". That window is transient, so rather than hard-fail a reader/opener
/// immediately, poll with exponential backoff (1ms doubling to a 200ms cap) up
/// to `budget`, then surface the error. A genuinely stuck holder still fails
/// after the budget. Non-lock errors (e.g. real corruption) return immediately
/// so the caller's wipe/rebuild path isn't delayed.
fn open_kuzu_with_retry<T>(
    mut open: impl FnMut() -> Result<T>,
    budget: std::time::Duration,
) -> Result<T> {
    let start = std::time::Instant::now();
    let mut delay = std::time::Duration::from_millis(1);
    loop {
        match open() {
            Ok(v) => return Ok(v),
            Err(e) if is_lock_contention_error(&e) && start.elapsed() < budget => {
                let remaining = budget.saturating_sub(start.elapsed());
                std::thread::sleep(delay.min(remaining));
                delay = (delay * 2).min(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
}
/// The main entry point for the infigraph framework.
pub struct Infigraph {
    root: PathBuf,
    db_path: PathBuf,
    registry: std::sync::Arc<LanguageRegistry>,
    backend_kind: BackendKind,
    /// When set, all file paths and symbol IDs are prefixed with `{namespace}/`.
    /// Used for multi-repo indexing into a shared Neo4j DB to prevent collisions.
    namespace: Option<String>,
}

/// Which graph backend is active.
enum BackendKind {
    /// Embedded Kùzu (default).
    Kuzu(graph::KuzuBackend),
    /// Not yet initialized — `init()` or `init_read_only()` must be called.
    Uninit,
    /// Remote Neo4j sidecar via Bolt.
    #[cfg(feature = "neo4j")]
    Neo4j(graph::Neo4jBackend),
}

impl Infigraph {
    /// Backoff schedule (ms) for retrying a non-lock-contention graph open
    /// failure before concluding it's genuine corruption. See `init()`.
    const OPEN_RETRY_BACKOFF_MS: [u64; 3] = [200, 500, 1000];

    /// Open a project directory. Creates `.infigraph/` if it doesn't exist.
    pub fn open(root: &Path, registry: LanguageRegistry) -> Result<Self> {
        Self::open_shared(root, std::sync::Arc::new(registry))
    }

    /// Open a project directory using a registry already shared via `Arc`.
    /// Use this instead of `open` when indexing many repos in one run
    /// (e.g. `group index`/`group build`) to avoid rebuilding the language
    /// registry (~62 tree-sitter packs) once per repo.
    pub fn open_shared(root: &Path, registry: std::sync::Arc<LanguageRegistry>) -> Result<Self> {
        let root = root.canonicalize().context("invalid project root")?;
        let db_path = root.join(".infigraph").join("graph");
        Ok(Self {
            root,
            db_path,
            registry,
            backend_kind: BackendKind::Uninit,
            namespace: None,
        })
    }

    /// Initialize the graph store (creates DB on first run).
    /// On corruption, wipes the graph directory and retries.
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
            _ => match open_kuzu_with_retry(
                || graph::KuzuBackend::open(&self.db_path),
                std::time::Duration::from_secs(3),
            ) {
                Ok(kb) => {
                    self.backend_kind = BackendKind::Kuzu(kb);
                    Ok(())
                }
                Err(first_err) if is_lock_contention_error(&first_err) => {
                    // Another live process (e.g. a running `infigraph watch`) holds
                    // this database open -- not corruption. Wiping here would destroy
                    // a perfectly good graph out from under that process.
                    Err(first_err).with_context(|| {
                        "graph is locked by another infigraph process (e.g. a running \
                         `infigraph watch`) -- not corrupted, so it was left untouched. \
                         Run `infigraph watch-status` or try again in a moment."
                    })
                }
                Err(first_err) => {
                    // Some Kuzu IO errors (e.g. a short read while a concurrent
                    // writer is mid-checkpoint) look identical to genuine
                    // corruption at open time but resolve themselves once that
                    // writer finishes. Retry with backoff before concluding the
                    // graph is unrecoverable -- wiping destroys real data if the
                    // first failure was just a transient race, and a single
                    // fixed-delay retry isn't enough for a slower writer (e.g. a
                    // large SCIP import mid-checkpoint).
                    let mut last_err = first_err;
                    let mut recovered = None;
                    for delay_ms in Self::OPEN_RETRY_BACKOFF_MS {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        match graph::KuzuBackend::open(&self.db_path) {
                            Ok(kb) => {
                                recovered = Some(kb);
                                break;
                            }
                            Err(e) if is_lock_contention_error(&e) => {
                                // Now unambiguously another live process holding the
                                // db open, not a transient checkpoint race -- stop
                                // retrying and report it as such.
                                return Err(e).with_context(|| {
                                    "graph is locked by another infigraph process (e.g. a \
                                     running `infigraph watch`) -- not corrupted, so it was \
                                     left untouched. Run `infigraph watch-status` or try \
                                     again in a moment."
                                });
                            }
                            Err(e) => last_err = e,
                        }
                    }

                    if let Some(kb) = recovered {
                        self.backend_kind = BackendKind::Kuzu(kb);
                        Ok(())
                    } else {
                        eprintln!(
                            "[graph] open failed after {} attempts ({last_err}), wiping \
                             corrupt graph and rebuilding...",
                            Self::OPEN_RETRY_BACKOFF_MS.len() + 1
                        );
                        Self::wipe_graph(&self.db_path);
                        let kb = graph::KuzuBackend::open(&self.db_path).with_context(|| {
                            format!("graph still unreadable after wipe (was: {last_err})")
                        })?;
                        self.backend_kind = BackendKind::Kuzu(kb);
                        Ok(())
                    }
                }
            },
        }
    }

    fn wipe_graph(db_path: &Path) {
        let _ = std::fs::remove_dir_all(db_path);
        let _ = std::fs::remove_file(db_path);
        let wal = db_path.with_extension("wal");
        let _ = std::fs::remove_file(&wal);
    }

    /// Initialize the graph store in read-only mode.
    /// Safe for concurrent access while a watcher writes.
    ///
    /// Respects `INFIGRAPH_BACKEND` env var:
    /// - `neo4j`: connects to remote Neo4j sidecar (no local DB)
    /// - default: opens embedded Kùzu in read-only mode
    pub fn init_read_only(&mut self) -> Result<()> {
        let backend_env = std::env::var("INFIGRAPH_BACKEND").unwrap_or_else(|_| "kuzu".into());

        match backend_env.as_str() {
            #[cfg(feature = "neo4j")]
            "neo4j" => {
                let neo = graph::Neo4jBackend::connect_from_env()?;
                self.backend_kind = BackendKind::Neo4j(neo);
                Ok(())
            }
            #[cfg(not(feature = "neo4j"))]
            "neo4j" => {
                anyhow::bail!("neo4j backend requested but binary compiled without `neo4j` feature")
            }
            _ => {
                // Retry briefly on lock contention: a reader landing during a
                // watcher's per-reindex write window should wait it out, not
                // hard-fail (AIF3X-331 #36).
                let kb = open_kuzu_with_retry(
                    || graph::KuzuBackend::open_read_only(&self.db_path),
                    std::time::Duration::from_secs(3),
                )?;
                self.backend_kind = BackendKind::Kuzu(kb);
                Ok(())
            }
        }
    }

    /// Index all supported files in the project, building the graph.
    /// Skips files whose content hash matches the stored hash (incremental).
    pub fn index(&self) -> Result<IndexResult> {
        let backend = self.backend().context("call init() first")?;
        self.index_via_backend(backend)
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
                // Resolve the pack before hashing: the skip decision has to use the
                // same fingerprint `extract_file` will store, and that covers the
                // pack's extraction behavior as well as the file's bytes.
                let pack = self.registry.for_file_with_content(&rel_path, &source)?;
                let hash = pack.content_fingerprint(&source);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let pct = n * 100 / total;
                let prev_pct = (n - 1) * 100 / total;
                if (pct / 25) > (prev_pct / 25) || n == total {
                    eprintln!("Parsing: {}/{} ({}%)", n, total, pct);
                }
                if existing_hashes.get(&rel_path).map(|s| s.as_str()) == Some(hash.as_str()) {
                    return None;
                }
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
        self.backend().context("call init() first")?.stats()
    }

    /// Access the underlying graph store (for direct Kùzu queries).
    /// Returns None when using Neo4j backend or before init.
    pub fn store(&self) -> Option<&GraphStore> {
        match &self.backend_kind {
            BackendKind::Kuzu(kb) => Some(kb.inner()),
            _ => None,
        }
    }

    /// Access the graph backend (works for all backend types).
    /// Returns None only before init() / init_read_only().
    pub fn backend(&self) -> Option<&dyn graph::GraphBackend> {
        match &self.backend_kind {
            BackendKind::Kuzu(kb) => Some(kb),
            BackendKind::Uninit => None,
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
        // Mirror to the Neo4j backend so File nodes get f.repo = ns at write time.
        #[cfg(feature = "neo4j")]
        if let BackendKind::Neo4j(ref mut neo) = self.backend_kind {
            neo.set_write_namespace(ns);
        }
    }

    /// Scope Neo4j read queries to a single repo by its directory name.
    /// No-op for Kuzu (single-repo by design).
    #[cfg(feature = "neo4j")]
    pub fn set_repo_filter(&mut self, repo: &str) {
        if let BackendKind::Neo4j(ref mut neo) = self.backend_kind {
            neo.set_repo_filter(repo);
        }
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
        self.backend()
            .context("call init() first")?
            .upsert_file(&extraction)
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

        let backend = self.backend().context("call init() first")?;
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
        self.backend()
            .context("call init() first")?
            .remove_file(&rel)
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
        let backend = self.backend().context("call init() first")?;
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
        Ok(count)
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
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
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

    /// Regression test for a data-loss bug: `Infigraph::init()` used to
    /// treat every Kuzu open failure as corruption and wipe the graph --
    /// including plain lock contention from another live process (e.g. a
    /// running `infigraph watch`), destroying its data. `is_lock_contention_error`
    /// is the check that now distinguishes the two. A genuine two-OS-process
    /// reproduction isn't exercised here -- `Database::new()` doesn't
    /// conflict across two `Infigraph` instances in the *same* process, only
    /// across separate processes, and this codebase deliberately avoids
    /// fork()-based tests given the tokio/rayon runtime state that would be
    /// undefined in a forked child (same reasoning as `scip_enrich_exit_message`
    /// in infigraph-cli's index.rs, which tests extracted logic rather than
    /// the full spawn path for the same class of reason).
    #[test]
    fn is_lock_contention_error_matches_kuzu_lock_message() {
        let err = anyhow::anyhow!(
            "failed to open kuzu db: IO exception: Could not set lock on file : /repo/.infigraph/graph"
        );
        assert!(is_lock_contention_error(&err));
    }

    #[test]
    fn is_lock_contention_error_does_not_match_genuine_corruption() {
        let err = anyhow::anyhow!(
            "failed to open kuzu db: Runtime exception: Database ID for temporary file \
             '/repo/.infigraph/graph.wal.checkpoint' does not match the current database."
        );
        assert!(
            !is_lock_contention_error(&err),
            "a genuine format/ID mismatch must still be treated as corruption and wiped"
        );
    }

    // AIF3X-331 #36: open_kuzu_with_retry waits out a transient lock-contention
    // window (a watcher's per-reindex write) instead of hard-failing a reader.

    #[test]
    fn open_kuzu_retry_succeeds_after_transient_lock_contention() {
        use std::cell::Cell;
        // Fail with a lock error the first 3 attempts, then succeed — models a
        // reindex write that releases the lock a few polls in.
        let attempts = Cell::new(0);
        let result = open_kuzu_with_retry(
            || {
                let n = attempts.get() + 1;
                attempts.set(n);
                if n <= 3 {
                    Err(anyhow::anyhow!(
                        "failed to open kuzu db: IO exception: Could not set lock on file : /x/.infigraph/graph"
                    ))
                } else {
                    Ok(n)
                }
            },
            std::time::Duration::from_secs(2),
        );
        assert_eq!(
            result.unwrap(),
            4,
            "should retry past the transient lock window and succeed on the 4th open"
        );
    }

    #[test]
    fn open_kuzu_retry_surfaces_error_after_budget() {
        // Persistent lock contention must still fail once the budget expires,
        // so a genuinely stuck holder doesn't hang forever.
        let attempts = std::cell::Cell::new(0);
        let start = std::time::Instant::now();
        let result: Result<()> = open_kuzu_with_retry(
            || {
                attempts.set(attempts.get() + 1);
                Err(anyhow::anyhow!(
                    "failed to open kuzu db: IO exception: Could not set lock on file : /x/.infigraph/graph"
                ))
            },
            std::time::Duration::from_millis(50),
        );
        assert!(
            result.is_err(),
            "persistent lock contention must eventually error"
        );
        assert!(is_lock_contention_error(&result.unwrap_err()));
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(50),
            "should have waited out the full budget before failing"
        );
        assert!(attempts.get() >= 2, "should have retried at least once");
    }

    #[test]
    fn open_kuzu_retry_does_not_retry_non_lock_errors() {
        // Genuine corruption must return immediately (no wasted backoff) so the
        // caller's wipe/rebuild path isn't delayed.
        let attempts = std::cell::Cell::new(0);
        let result: Result<()> = open_kuzu_with_retry(
            || {
                attempts.set(attempts.get() + 1);
                Err(anyhow::anyhow!(
                    "failed to open kuzu db: Runtime exception: Database ID mismatch (corrupt)"
                ))
            },
            std::time::Duration::from_secs(5),
        );
        assert!(result.is_err());
        assert_eq!(
            attempts.get(),
            1,
            "a non-lock (corruption) error must not be retried"
        );
    }

    /// Regression test for a second data-loss bug in the same area: a Kuzu
    /// open failure that isn't lock contention (e.g. a short read while a
    /// concurrent writer is mid-checkpoint) used to be wiped immediately with
    /// no retry, even though the underlying file becomes readable again the
    /// instant that writer finishes. `init()` destroyed a real repo's graph
    /// this way -- the open failed with "Cannot read from file... 0 bytes",
    /// not a lock message, so it fell straight through to `wipe_graph`.
    #[test]
    fn init_recovers_from_transient_open_failure_without_wiping() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let db_path = root.join(".infigraph").join("graph");

        // Build a real graph with a marker symbol, then close it.
        {
            let store = GraphStore::open(&db_path).unwrap();
            let conn = store.connection().unwrap();
            conn.query(
                "CREATE (:Symbol {id: 'marker::survived', name: 'survived', kind: 'function', \
                 file: 'marker.rs', start_line: 0, end_line: 0, signature_hash: '', \
                 language: 'rust', visibility: 'public', parent: '', docstring: '', \
                 complexity: 0, parameters: '', return_type: ''})",
            )
            .unwrap();
        }
        let valid_bytes = std::fs::read(&db_path).unwrap();

        // Corrupt the file so the first open attempt fails, then heal it
        // shortly after -- well within init()'s 300ms retry delay -- to
        // simulate a concurrent writer that was mid-checkpoint.
        std::fs::write(&db_path, b"not a valid kuzu database file at all").unwrap();
        let healer_path = db_path.clone();
        let healer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::write(&healer_path, &valid_bytes).unwrap();
        });

        let registry = LanguageRegistry::new();
        let mut ig = Infigraph::open(root, registry).unwrap();
        let result = ig.init();
        healer.join().unwrap();

        assert!(
            result.is_ok(),
            "init() should recover once the transient failure heals: {result:?}"
        );

        let backend = ig.backend().unwrap();
        let rows = backend
            .raw_query("MATCH (s:Symbol) WHERE s.id = 'marker::survived' RETURN s.id")
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the pre-existing graph must survive a transient failure -- no wipe should occur"
        );
    }

    /// A single fixed-delay retry isn't enough for a slower concurrent writer
    /// (e.g. a large SCIP import still mid-checkpoint). Heal at ~600ms --
    /// past where a lone 300ms retry would already have given up and wiped,
    /// but within OPEN_RETRY_BACKOFF_MS's cumulative 200+500=700ms window --
    /// to prove the backoff schedule covers slower recoveries too.
    #[test]
    fn init_recovers_from_slower_transient_failure_via_backoff() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let db_path = root.join(".infigraph").join("graph");

        {
            let store = GraphStore::open(&db_path).unwrap();
            let conn = store.connection().unwrap();
            conn.query(
                "CREATE (:Symbol {id: 'marker::slow-heal', name: 'slow_heal', kind: 'function', \
                 file: 'marker.rs', start_line: 0, end_line: 0, signature_hash: '', \
                 language: 'rust', visibility: 'public', parent: '', docstring: '', \
                 complexity: 0, parameters: '', return_type: ''})",
            )
            .unwrap();
        }
        let valid_bytes = std::fs::read(&db_path).unwrap();

        std::fs::write(&db_path, b"not a valid kuzu database file at all").unwrap();
        let healer_path = db_path.clone();
        let healer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            std::fs::write(&healer_path, &valid_bytes).unwrap();
        });

        let registry = LanguageRegistry::new();
        let mut ig = Infigraph::open(root, registry).unwrap();
        let result = ig.init();
        healer.join().unwrap();

        assert!(
            result.is_ok(),
            "init() should recover via the backoff schedule: {result:?}"
        );

        let backend = ig.backend().unwrap();
        let rows = backend
            .raw_query("MATCH (s:Symbol) WHERE s.id = 'marker::slow-heal' RETURN s.id")
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "a slower-but-still-transient failure must not be wiped either"
        );
    }

    /// Companion to the test above: if the open failure is *not* transient
    /// (the file is durably corrupt, not just briefly unreadable), init()
    /// must still recover by wiping and rebuilding rather than looping
    /// forever or erroring out permanently.
    #[test]
    fn init_wipes_and_rebuilds_on_persistent_corruption() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let db_path = root.join(".infigraph").join("graph");

        {
            let store = GraphStore::open(&db_path).unwrap();
            let conn = store.connection().unwrap();
            conn.query(
                "CREATE (:Symbol {id: 'marker::wiped', name: 'wiped', kind: 'function', \
                 file: 'marker.rs', start_line: 0, end_line: 0, signature_hash: '', \
                 language: 'rust', visibility: 'public', parent: '', docstring: '', \
                 complexity: 0, parameters: '', return_type: ''})",
            )
            .unwrap();
        }
        // Corrupt permanently -- nothing heals this one.
        std::fs::write(&db_path, b"not a valid kuzu database file at all").unwrap();

        let registry = LanguageRegistry::new();
        let mut ig = Infigraph::open(root, registry).unwrap();
        let result = ig.init();

        assert!(
            result.is_ok(),
            "init() must recover from persistent corruption via wipe+rebuild: {result:?}"
        );
        let backend = ig.backend().unwrap();
        let rows = backend
            .raw_query("MATCH (s:Symbol) WHERE s.id = 'marker::wiped' RETURN s.id")
            .unwrap();
        assert_eq!(
            rows.len(),
            0,
            "persistent corruption should have been wiped, not silently ignored"
        );
    }
}
