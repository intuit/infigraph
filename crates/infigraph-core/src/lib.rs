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
    /// Open a project directory. Creates `.infigraph/` if it doesn't exist.
    pub fn open(root: &Path, registry: LanguageRegistry) -> Result<Self> {
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
            _ => match graph::KuzuBackend::open(&self.db_path) {
                Ok(kb) => {
                    self.backend_kind = BackendKind::Kuzu(kb);
                    Ok(())
                }
                Err(first_err) => {
                    eprintln!(
                        "[graph] open failed ({first_err}), wiping corrupt graph and rebuilding..."
                    );
                    Self::wipe_graph(&self.db_path);
                    let kb = graph::KuzuBackend::open(&self.db_path).with_context(|| {
                        format!("graph still unreadable after wipe (was: {first_err})")
                    })?;
                    self.backend_kind = BackendKind::Kuzu(kb);
                    Ok(())
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
                let kb = graph::KuzuBackend::open_read_only(&self.db_path)?;
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
