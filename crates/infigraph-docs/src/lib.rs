pub mod extract;
pub mod chunk;
pub mod store;
pub mod search;
pub mod watch;
pub mod embed;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use chunk::{Chunk, ChunkStrategy};
use extract::ExtractedDoc;
use store::DocStore;

pub mod links;

pub struct DocIndex {
    root: PathBuf,
    db_path: PathBuf,
    store: Option<DocStore>,
}

pub struct DocIndexResult {
    pub total_files: usize,
    pub indexed_files: usize,
    pub total_chunks: usize,
}

impl DocIndex {
    pub fn open(root: &Path) -> Result<Self> {
        let tg_dir = root.join(".infigraph");
        std::fs::create_dir_all(&tg_dir)?;
        let db_path = tg_dir.join("docs.kuzu");
        Ok(Self {
            root: root.to_path_buf(),
            db_path,
            store: None,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        let store = DocStore::open(&self.db_path)?;
        self.store = Some(store);
        Ok(())
    }

    pub fn store(&self) -> Option<&DocStore> {
        self.store.as_ref()
    }

    pub fn root(&self) -> &Path {
        &self.root
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
        let store = self.store.as_ref().context("call init() first")?;

        let files = self.collect_doc_files()?;
        let total = files.len();

        if total == 0 {
            return Ok(DocIndexResult {
                total_files: 0,
                indexed_files: 0,
                total_chunks: 0,
            });
        }

        let existing_hashes = store.get_doc_hashes().unwrap_or_default();

        let done = AtomicUsize::new(0);

        let results: Vec<(ExtractedDoc, Vec<Chunk>)> = files
            .par_iter()
            .filter_map(|path| {
                let rel = path
                    .strip_prefix(&self.root)
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
                Some((ExtractedDoc { file: rel, content_hash: hash, ..doc }, chunks))
            })
            .collect();

        let indexed = results.len();
        let total_chunks: usize = results.iter().map(|(_, c)| c.len()).sum();

        if !results.is_empty() {
            let docs: Vec<&ExtractedDoc> = results.iter().map(|(d, _)| d).collect();
            let chunks: Vec<&Chunk> = results.iter().flat_map(|(_, c)| c.iter()).collect();
            store.upsert_all_parquet(&docs, &chunks)?;
        }

        if total_chunks > 0 {
            let all_chunks: Vec<&Chunk> = results.iter().flat_map(|(_, c)| c.iter()).collect();
            let changed_files: Vec<&str> = results.iter().map(|(d, _)| d.file.as_str()).collect();
            embed::update_doc_embeddings(store, &self.root, &all_chunks, &changed_files)?;
        }

        // Extract links from indexed docs and create LINKS_TO edges
        if !results.is_empty() {
            let all_doc_ids: std::collections::HashSet<String> = {
                let existing = store.get_doc_hashes().unwrap_or_default();
                existing.keys().cloned().collect()
            };
            for (doc, _) in &results {
                links::extract_and_link_doc(store, doc, &all_doc_ids);
            }
        }

        Ok(DocIndexResult {
            total_files: total,
            indexed_files: indexed,
            total_chunks,
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
