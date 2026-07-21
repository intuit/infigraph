pub mod backend;
pub mod chunk;
pub mod combined;
pub mod embed;
pub mod extract;
#[cfg(feature = "remote")]
pub mod neo4j_store;
pub mod search;
pub mod store;
pub mod watch;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use backend::DocBackend;
use chunk::{Chunk, ChunkStrategy};
use extract::ExtractedDoc;
use store::DocStore;

pub mod links;

fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

pub struct DocIndex {
    root: PathBuf,
    db_path: PathBuf,
    store: Option<Box<dyn DocBackend>>,
    skip_file_embeddings: bool,
}

pub struct DocIndexResult {
    pub total_files: usize,
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub bfs_discovered: usize,
    pub new_chunks: Vec<Chunk>,
    pub changed_files: Vec<String>,
}

impl DocIndex {
    pub fn open(root: &Path) -> Result<Self> {
        let tg_dir = root.join(".infigraph");
        if !is_remote_mode() {
            std::fs::create_dir_all(&tg_dir)?;
        }
        let db_path = tg_dir.join("docs.kuzu");
        Ok(Self {
            root: root.to_path_buf(),
            db_path,
            store: None,
            skip_file_embeddings: false,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        #[cfg(feature = "remote")]
        if is_remote_mode() {
            let neo = neo4j_store::Neo4jDocStore::connect_from_env()?;
            neo.init_schema()?;
            self.store = Some(Box::new(neo));
            return Ok(());
        }

        match DocStore::open(&self.db_path) {
            Ok(store) => {
                self.store = Some(Box::new(store));
                Ok(())
            }
            Err(first_err) => {
                eprintln!(
                    "[docs] open failed ({first_err}), wiping corrupt doc index and rebuilding..."
                );
                self.clean()?;
                let store = DocStore::open(&self.db_path).with_context(|| {
                    format!("docs kuzu still unreadable after wipe (was: {first_err})")
                })?;
                self.store = Some(Box::new(store));
                self.index()?;
                Ok(())
            }
        }
    }

    pub fn store(&self) -> Option<&dyn DocBackend> {
        self.store.as_deref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn set_skip_file_embeddings(&mut self, skip: bool) {
        self.skip_file_embeddings = skip;
    }

    pub fn clean(&mut self) -> Result<()> {
        self.store = None;
        let tg_dir = self.root.join(".infigraph");
        if self.db_path.is_dir() {
            let _ = std::fs::remove_dir_all(&self.db_path);
        } else {
            let _ = std::fs::remove_file(&self.db_path);
        }
        let _ = std::fs::remove_file(self.db_path.with_extension("wal"));
        let _ = std::fs::remove_file(self.db_path.with_extension("lock"));
        let _ = std::fs::remove_file(tg_dir.join("docs_embeddings.bin"));
        let _ = std::fs::remove_file(tg_dir.join("docs_hnsw_index.usearch"));
        let _ = std::fs::remove_file(tg_dir.join("docs_hnsw_index.meta"));
        infigraph_core::embed::invalidate_embeddings_cache();
        infigraph_core::embed::invalidate_hnsw_cache();
        Ok(())
    }

    pub fn reindex(&mut self) -> Result<DocIndexResult> {
        self.clean()?;
        self.init()?;
        self.index()
    }

    pub fn index(&self) -> Result<DocIndexResult> {
        let store = self.store.as_deref().context("call init() first")?;

        let files = self.collect_doc_files()?;
        let total = files.len();

        if total == 0 {
            return Ok(DocIndexResult {
                total_files: 0,
                indexed_files: 0,
                total_chunks: 0,
                bfs_discovered: 0,
                new_chunks: vec![],
                changed_files: vec![],
            });
        }

        let existing_hashes = store.get_doc_hashes().unwrap_or_default();

        let done = AtomicUsize::new(0);
        let root = &self.root;

        let results: Vec<(ExtractedDoc, Vec<Chunk>)> = files
            .par_iter()
            .filter_map(|path| {
                let rel = path
                    .strip_prefix(root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(path).ok()?;
                let hash = {
                    let mut h = Sha256::new();
                    h.update(&bytes);
                    format!("{:x}", h.finalize())
                };

                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                let pct = n * 100 / total;
                let prev_pct = (n - 1) * 100 / total;
                if (pct / 25) > (prev_pct / 25) || n == total {
                    eprintln!("Doc indexing: {}/{} ({}%)", n, total, pct);
                }

                if existing_hashes.get(&rel).map(|s| s.as_str()) == Some(hash.as_str()) {
                    return None;
                }

                let ext = path.extension()?.to_string_lossy().to_lowercase();
                let doc = extract::extract_document(path, &bytes, &ext).ok()?;
                let strategy = ChunkStrategy::for_extension(&ext);
                let chunks = chunk::chunk_document(&doc, &rel, &hash, strategy);
                Some((
                    ExtractedDoc {
                        file: rel,
                        content_hash: hash,
                        ..doc
                    },
                    chunks,
                ))
            })
            .collect();

        let indexed = results.len();
        let total_chunks: usize = results.iter().map(|(_, c)| c.len()).sum();

        if !results.is_empty() {
            let docs: Vec<&ExtractedDoc> = results.iter().map(|(d, _)| d).collect();
            let chunks: Vec<&Chunk> = results.iter().flat_map(|(_, c)| c.iter()).collect();
            store.upsert_docs(&docs, &chunks)?;
        }

        let result_chunks: Vec<Chunk> = results.iter().flat_map(|(_, c)| c.clone()).collect();
        let result_changed: Vec<String> = results.iter().map(|(d, _)| d.file.clone()).collect();

        if total_chunks > 0 && !self.skip_file_embeddings {
            let all_chunks: Vec<&Chunk> = results.iter().flat_map(|(_, c)| c.iter()).collect();
            let changed_files: Vec<&str> = results.iter().map(|(d, _)| d.file.as_str()).collect();
            #[cfg(feature = "remote")]
            if is_remote_mode() {
                if let Ok(pg) = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached() {
                    embed::update_doc_embeddings_remote(store, &pg, &all_chunks, &changed_files)?;
                } else {
                    eprintln!(
                        "Warning: remote mode but Postgres unavailable, skipping doc embeddings"
                    );
                }
            } else {
                embed::update_doc_embeddings(store, &self.root, &all_chunks, &changed_files)?;
            }
            #[cfg(not(feature = "remote"))]
            embed::update_doc_embeddings(store, &self.root, &all_chunks, &changed_files)?;
        }

        // Prune stale docs: remove entries for files that no longer exist on disk
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
                eprintln!("Doc pruning: removing {} stale doc(s)", stale.len());
                let stale_refs: Vec<&str> = stale.iter().map(|s| s.as_str()).collect();
                let _ = store.delete_docs_by_ids(&stale_refs);
            }
        }

        // Extract links from indexed docs and create LINKS_TO edges
        let mut all_doc_ids: HashSet<String> = {
            let existing = store.get_doc_hashes().unwrap_or_default();
            existing.keys().cloned().collect()
        };
        if !results.is_empty() {
            for (doc, _) in &results {
                links::extract_and_link_doc(store, doc, &all_doc_ids);
            }
        }

        // BFS: follow links to docs outside the doc root but within the repo
        let bfs_discovered = if let Some(repo_root) = find_repo_root(&self.root) {
            let n = self.bfs_follow_links(store, &mut all_doc_ids, &repo_root, 2, 50)?;
            if n > 0 {
                eprintln!("BFS: discovered and indexed {} doc(s) outside root", n);
            }
            n
        } else {
            0
        };

        Ok(DocIndexResult {
            total_files: total,
            indexed_files: indexed,
            total_chunks,
            bfs_discovered,
            new_chunks: result_chunks,
            changed_files: result_changed,
        })
    }

    fn collect_doc_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        self.walk_doc_dir(&self.root, &mut files)?;
        Ok(files)
    }

    fn walk_doc_dir(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        let ignore_dirs = [
            ".infigraph",
            ".git",
            "node_modules",
            "__pycache__",
            ".venv",
            "venv",
            "target",
            "build",
            "dist",
            ".tox",
        ];

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if path.is_dir() {
                if !ignore_dirs.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                    self.walk_doc_dir(&path, files)?;
                }
            } else if path.is_file() && is_document_file(&path) {
                files.push(path);
            }
        }
        Ok(())
    }

    fn bfs_follow_links(
        &self,
        store: &dyn DocBackend,
        indexed_docs: &mut HashSet<String>,
        repo_root: &Path,
        max_depth: usize,
        max_extra: usize,
    ) -> Result<usize> {
        let ignore_dirs = [
            ".infigraph",
            ".git",
            "node_modules",
            "__pycache__",
            ".venv",
            "venv",
            "target",
            "build",
            "dist",
            ".tox",
        ];
        let repo_root = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());
        let root_canonical = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let mut total_new = 0usize;
        let mut new_chunks = Vec::new();
        let mut changed_files = Vec::new();
        let mut frontier: Vec<PathBuf> = indexed_docs
            .iter()
            .filter_map(|rel| {
                let p = self.root.join(rel);
                p.canonicalize().ok().filter(|c| c.is_file())
            })
            .collect();

        for _depth in 0..max_depth {
            if frontier.is_empty() || total_new >= max_extra {
                break;
            }
            let mut next_frontier = Vec::new();

            for doc_path in &frontier {
                if total_new >= max_extra {
                    break;
                }
                let text = match std::fs::read_to_string(doc_path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let doc_file = doc_path.to_string_lossy();
                let extracted_links = links::extract_links(&text, &doc_file);

                for link in &extracted_links {
                    if total_new >= max_extra {
                        break;
                    }
                    let abs = match links::resolve_link_to_abs_path(&link.url, doc_path) {
                        Some(p) => p,
                        None => continue,
                    };
                    if !is_document_file(&abs) {
                        continue;
                    }
                    // Skip symlinks (check before canonicalize)
                    if let Ok(meta) = std::fs::symlink_metadata(&abs) {
                        if meta.file_type().is_symlink() {
                            continue;
                        }
                    }
                    let abs = abs.canonicalize().unwrap_or(abs);
                    if !abs.starts_with(&repo_root) {
                        continue;
                    }
                    // Check ignored dirs — only check path components relative to repo root
                    let rel_to_repo = abs.strip_prefix(&repo_root).unwrap_or(&abs);
                    let in_ignored = rel_to_repo.components().any(|c| {
                        if let std::path::Component::Normal(s) = c {
                            let s = s.to_string_lossy();
                            ignore_dirs.contains(&s.as_ref()) || s.starts_with('.')
                        } else {
                            false
                        }
                    });
                    if in_ignored {
                        continue;
                    }

                    // Build relative ID (relative to doc root for consistency, or absolute if outside)
                    let rel_id = if let Ok(rel) = abs.strip_prefix(&root_canonical) {
                        rel.to_string_lossy().replace('\\', "/")
                    } else {
                        abs.to_string_lossy().replace('\\', "/")
                    };

                    if indexed_docs.contains(&rel_id) {
                        continue;
                    }

                    // Index this file
                    let bytes = match std::fs::read(&abs) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    let hash = {
                        let mut h = Sha256::new();
                        h.update(&bytes);
                        format!("{:x}", h.finalize())
                    };
                    let ext = match abs.extension() {
                        Some(e) => e.to_string_lossy().to_lowercase(),
                        None => continue,
                    };
                    let doc = match extract::extract_document(&abs, &bytes, &ext) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };
                    let strategy = ChunkStrategy::for_extension(&ext);
                    let doc = ExtractedDoc {
                        file: rel_id.clone(),
                        content_hash: hash.clone(),
                        ..doc
                    };
                    let chunks = chunk::chunk_document(&doc, &rel_id, &hash, strategy);

                    let docs_ref = vec![&doc];
                    let chunks_ref: Vec<&Chunk> = chunks.iter().collect();
                    if store.upsert_docs(&docs_ref, &chunks_ref).is_ok() {
                        indexed_docs.insert(rel_id.clone());
                        changed_files.push(rel_id);
                        new_chunks.extend(chunks);
                        next_frontier.push(abs);
                        total_new += 1;
                    }
                }
            }
            frontier = next_frontier;
        }

        if !new_chunks.is_empty() {
            let chunk_refs: Vec<&Chunk> = new_chunks.iter().collect();
            let changed_file_refs: Vec<&str> = changed_files.iter().map(String::as_str).collect();
            embed::update_doc_embeddings(store, &self.root, &chunk_refs, &changed_file_refs)?;
        }

        // Re-run link extraction for all docs (newly discovered may link to each other)
        if total_new > 0 {
            let all_hashes = store.get_doc_hashes().unwrap_or_default();
            let all_ids: HashSet<String> = all_hashes.keys().cloned().collect();
            for doc_id in all_ids.iter() {
                let doc_path = if doc_id.starts_with('/') {
                    PathBuf::from(doc_id)
                } else {
                    self.root.join(doc_id)
                };
                if let Ok(text) = std::fs::read_to_string(&doc_path) {
                    let doc = ExtractedDoc {
                        file: doc_id.clone(),
                        title: None,
                        content_hash: String::new(),
                        format: extract::DocFormat::Markdown,
                        text,
                        page_count: None,
                    };
                    links::extract_and_link_doc(store, &doc, &all_ids);
                }
            }
        }

        Ok(total_new)
    }
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn is_document_file(path: &Path) -> bool {
    let ext = match path.extension() {
        Some(e) => e.to_string_lossy().to_lowercase(),
        None => return false,
    };
    matches!(
        ext.as_str(),
        "md" | "markdown"
            | "txt"
            | "rst"
            | "adoc"
            | "org"
            | "pdf"
            | "docx"
            | "pptx"
            | "xlsx"
            | "rtf"
            | "html"
            | "htm"
            | "epub"
            | "xml"
            | "xsl"
            | "xsd"
            | "svg"
            | "plist"
    )
}
