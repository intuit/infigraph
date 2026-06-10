mod analysis;
pub mod bench;
pub mod bridges;
pub mod check;
pub mod cluster;
pub mod diff;
pub mod embed;
pub mod export;
pub mod extract;
pub mod graph;
pub mod lang;
pub mod learned;
pub mod manifest;
pub mod model;
pub mod multi;
pub mod patterns;
pub mod refactor;
mod report;
pub mod resolve;
pub mod review;
pub mod routes;
pub mod scip;
pub mod search;
pub mod security;
pub mod sequence;
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

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The main entry point for the infigraph framework.
pub struct Infigraph {
    root: PathBuf,
    db_path: PathBuf,
    registry: LanguageRegistry,
    store: Option<GraphStore>,
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
        })
    }

    /// Initialize the graph store (creates DB on first run).
    pub fn init(&mut self) -> Result<()> {
        let store = GraphStore::open(&self.db_path)?;
        self.store = Some(store);
        Ok(())
    }

    /// Index all supported files in the project, building the graph.
    /// Skips files whose content hash matches the stored hash (incremental).
    pub fn index(&self) -> Result<IndexResult> {
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
        if !extractions.is_empty() {
            if use_csv {
                if !existing_hashes.is_empty() {
                    // Incremental bulk: delete old data for changed files before CSV load
                    let conn = store.connection()?;
                    conn.query("BEGIN TRANSACTION")
                        .context("failed to begin delete transaction")?;
                    let file_list: Vec<String> = extractions
                        .iter()
                        .map(|e| format!("'{}'", escape_str(&e.file)))
                        .collect();
                    let files_in = file_list.join(", ");
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
                store.upsert_all_parquet(&extractions)?;
            } else {
                // Small incremental: per-file UNWIND (overhead acceptable for <100 files)
                let conn = store.connection()?;
                conn.query("BEGIN TRANSACTION")
                    .context("failed to begin index transaction")?;
                let file_list: Vec<String> = extractions
                    .iter()
                    .map(|e| format!("'{}'", escape_str(&e.file)))
                    .collect();
                let files_in = file_list.join(", ");
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
                // Folder upsert outside transaction — COPY FROM can't run inside explicit txn
                let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
                store.upsert_folders_bulk_conn(&conn, &file_paths)?;
            }
        }

        // Bulk-write folder hierarchy for CSV path — no explicit txn wrapper
        // because upsert_folders_bulk_conn may use COPY FROM which can't run inside explicit txn
        if use_csv {
            let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
            let conn = store.connection()?;
            store.upsert_folders_bulk_conn(&conn, &file_paths)?;
        }

        // Post-indexing: resolve cross-file call targets using full graph symbol table
        let resolve_stats =
            resolve::resolve_calls_incremental(store, &extractions, None).unwrap_or_else(|e| {
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
            total_files: total,
            indexed_files: indexed,
            extractions,
            resolve_stats,
        })
    }

    /// Get graph statistics.
    pub fn stats(&self) -> Result<graph::GraphStats> {
        let store = self.store.as_ref().context("call init() first")?;
        store.stats()
    }

    /// Access the underlying graph store (for direct queries).
    pub fn store(&self) -> Option<&GraphStore> {
        self.store.as_ref()
    }

    /// Access the language registry.
    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }

    /// Get the project root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Index (or re-index) a single file by its path on disk.
    /// Path may be absolute or relative to project root.
    pub fn index_file(&self, path: &Path) -> Result<()> {
        let store = self.store.as_ref().context("call init() first")?;
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
        store.upsert_file(&extraction)?;
        Ok(())
    }

    /// Index a batch of files by path, returning an IndexResult with all extractions.
    pub fn index_files(&self, paths: &[PathBuf]) -> Result<IndexResult> {
        let store = self.store.as_ref().context("call init() first")?;

        if paths.is_empty() {
            return Ok(IndexResult {
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
            });
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
                store.upsert_all_parquet(&extractions)?;
            } else {
                let conn = store.connection()?;
                store.upsert_all_bulk(&conn, &extractions)?;
            }

            let file_paths: Vec<&str> = extractions.iter().map(|e| e.file.as_str()).collect();
            let conn = store.connection()?;
            store.upsert_folders_bulk_conn(&conn, &file_paths)?;
        }

        let resolve_stats =
            resolve::resolve_calls_incremental(store, &extractions, None).unwrap_or_else(|e| {
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
        let store = self.store.as_ref().context("call init() first")?;
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        store.remove_file(&rel)
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
                !matches!(name.as_ref(), ".infigraph" | "node_modules" | "__pycache__" | ".tox")
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
