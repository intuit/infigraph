use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};

use crate::model::Symbol;

struct CachedEmbeddings {
    path: PathBuf,
    modified: std::time::SystemTime,
    data: Vec<(String, Vec<f32>)>,
}

static EMBEDDINGS_CACHE: OnceLock<Mutex<Option<CachedEmbeddings>>> = OnceLock::new();

fn cache_lock() -> &'static Mutex<Option<CachedEmbeddings>> {
    EMBEDDINGS_CACHE.get_or_init(|| Mutex::new(None))
}

/// Load embeddings with process-lifetime caching. Returns cached data if the
/// file hasn't been modified since last load. Falls back to `load_embeddings`
/// on any cache miss.
pub fn load_embeddings_cached(path: &Path) -> Result<Vec<(String, Vec<f32>)>> {
    let meta = std::fs::metadata(path).context("stat embeddings file")?;
    let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let guard = cache_lock().lock().unwrap();
    if let Some(cached) = guard.as_ref() {
        if cached.path == canon && cached.modified == mtime {
            return Ok(cached.data.clone());
        }
    }
    drop(guard);

    let data = load_embeddings(path)?;
    let mut guard = cache_lock().lock().unwrap();
    *guard = Some(CachedEmbeddings {
        path: canon,
        modified: mtime,
        data: data.clone(),
    });
    Ok(data)
}

/// Invalidate the embeddings cache (call after save_embeddings or update_embeddings).
pub fn invalidate_embeddings_cache() {
    if let Ok(mut guard) = cache_lock().lock() {
        *guard = None;
    }
}

/// Embedding engine trait. Implementations can use ONNX, API calls, etc.
pub trait EmbedProvider: Send + Sync {
    fn dimension(&self) -> usize;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut results = self.embed_batch(&[text])?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("embedding returned no results"))
    }
}

/// Build a text representation of a symbol for embedding.
pub fn symbol_text(sym: &Symbol) -> String {
    let mut text = format!("{} {} {}", sym.kind.as_str(), sym.name, sym.language);
    if let Some(doc) = &sym.docstring {
        if !doc.is_empty() {
            text.push_str(": ");
            text.push_str(doc);
        }
    }
    text
}

/// Build a rich text representation of a symbol for embedding, including file context.
pub fn rich_symbol_text(kind: &str, name: &str, file: &str, language: &str, doc: &str) -> String {
    rich_symbol_text_full(kind, name, file, language, doc, "", "")
}

/// Extended rich text representation with parameter and return type info.
pub fn rich_symbol_text_full(
    kind: &str,
    name: &str,
    file: &str,
    language: &str,
    doc: &str,
    params: &str,
    ret: &str,
) -> String {
    let path_context = path_to_context(file);
    let mut text = format!("{kind} {name}");
    if !params.is_empty() {
        text.push_str(params);
    }
    if !ret.is_empty() {
        text.push_str(" -> ");
        text.push_str(ret);
    }
    text.push_str(" in ");
    text.push_str(&path_context);
    if !language.is_empty() {
        text.push(' ');
        text.push_str(language);
    }
    if !doc.is_empty() {
        text.push_str(": ");
        text.push_str(doc);
    }
    text
}

/// Extract meaningful context from a file path by filtering out common directory names.
pub fn path_to_context(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() <= 3 {
        return file.to_string();
    }
    let filename = parts.last().unwrap_or(&"");
    let meaningful: Vec<&str> = parts
        .iter()
        .filter(|p| {
            let lower = p.to_lowercase();
            !matches!(
                lower.as_str(),
                "src" | "source" | "lib" | "include" | "_h" | "test" | "tests" | "benchmark"
            )
        })
        .copied()
        .collect();
    if meaningful.len() <= 4 {
        meaningful.join("/")
    } else {
        let last4 = &meaningful[meaningful.len() - 4..];
        if last4.contains(filename) {
            last4.join("/")
        } else {
            format!("{}/{}", last4[1..].join("/"), filename)
        }
    }
}

/// Dot-product similarity — equivalent to cosine when vectors are L2-normalized.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Lightweight, zero-dependency embedder using character n-gram hashing.
///
/// Produces fixed-size vectors by hashing character trigrams into buckets
/// using a deterministic hash. No model download, no ML framework.
/// Quality is lower than neural embeddings but sufficient for code search
/// where symbol names and docstrings carry strong lexical signal.
pub struct TrigramEmbedder {
    dim: usize,
}

impl TrigramEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Default for TrigramEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl EmbedProvider for TrigramEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| trigram_embed(t, self.dim)).collect())
    }
}

/// Hash a string into a fixed-size vector using character trigrams.
fn trigram_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    if chars.len() < 3 {
        // For very short strings, hash unigrams and bigrams too
        for c in &chars {
            let h = fnv1a(&[*c as u8]) as usize % dim;
            vec[h] += 1.0;
        }
        if chars.len() == 2 {
            let bigram = format!("{}{}", chars[0], chars[1]);
            let h = fnv1a(bigram.as_bytes()) as usize % dim;
            vec[h] += 1.0;
        }
    } else {
        for window in chars.windows(3) {
            let trigram: String = window.iter().collect();
            let h = fnv1a(trigram.as_bytes()) as usize % dim;
            vec[h] += 1.0;
        }
    }

    // Also hash whole tokens (split on non-alphanumeric)
    for token in lower.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if token.len() > 1 {
            let h = fnv1a(token.as_bytes()) as usize % dim;
            vec[h] += 0.5; // Lower weight for whole tokens
        }
    }

    // L2 normalize
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }

    vec
}

/// Neural-quality embedder using Model2Vec (distilled sentence transformer).
///
/// Downloads `minishlab/potion-base-8M` (~15MB) on first use from HuggingFace Hub.
/// Pure Rust inference — no ONNX, no C++ deps, no GPU needed.
/// 256-dim embeddings with much better semantic quality than trigrams.
pub struct Model2VecEmbedder {
    model: model2vec_rs::model::StaticModel,
}

impl Model2VecEmbedder {
    /// Initialize from bundled model in models/potion-base-8M/.
    pub fn new() -> Result<Self> {
        // Walk up from the executable or manifest dir to find the models/ folder
        let model_dir = Self::find_model_dir()?;
        let model = model2vec_rs::model::StaticModel::from_pretrained(model_dir, None, None, None)?;
        Ok(Self { model })
    }

    fn find_model_dir() -> Result<std::path::PathBuf> {
        // 1. Check env var override
        if let Ok(p) = std::env::var("INFIGRAPH_MODEL_DIR") {
            let pb = std::path::PathBuf::from(p);
            if pb.exists() {
                return Ok(pb);
            }
        }
        // 2. Check ~/.infigraph/models/ (installed by `infigraph install`)
        if let Some(home) = dirs_next::home_dir() {
            let installed = home
                .join(".infigraph")
                .join("models")
                .join("potion-base-8M");
            if installed.join("model.safetensors").exists() {
                return Ok(installed);
            }
        }
        // 3. Walk up from current exe to find models/potion-base-8M/
        let start =
            std::env::current_exe().unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
        let mut dir = start.as_path();
        loop {
            let candidate = dir.join("models/potion-base-8M");
            if candidate.join("model.safetensors").exists() {
                return Ok(candidate);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
        // 4. Walk up from cwd
        let cwd = std::env::current_dir()?;
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("models/potion-base-8M");
            if candidate.join("model.safetensors").exists() {
                return Ok(candidate);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
        anyhow::bail!(
            "models/potion-base-8M not found; set INFIGRAPH_MODEL_DIR or run from repo root"
        )
    }
}

impl EmbedProvider for Model2VecEmbedder {
    fn dimension(&self) -> usize {
        256 // potion-base-8M outputs 256-dim
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        Ok(self.model.encode(&owned))
    }
}

static CODE_EMBEDDER: OnceLock<Arc<dyn EmbedProvider>> = OnceLock::new();
static DOC_EMBEDDER: OnceLock<Arc<dyn EmbedProvider>> = OnceLock::new();

/// Factory: select Model2Vec if available, otherwise fall back to TrigramEmbedder.
pub fn init_embedder() -> Arc<dyn EmbedProvider> {
    match Model2VecEmbedder::new() {
        Ok(m) => Arc::new(m),
        Err(e) => {
            eprintln!("warning: Model2Vec unavailable ({e}), using trigram fallback");
            Arc::new(TrigramEmbedder::default())
        }
    }
}

/// Singleton lazy-init code embedder (Arc-based, shared across threads).
pub fn code_embedder() -> Arc<dyn EmbedProvider> {
    Arc::clone(CODE_EMBEDDER.get_or_init(init_embedder))
}

/// Singleton lazy-init doc embedder (Arc-based, shared across threads).
pub fn doc_embedder() -> Arc<dyn EmbedProvider> {
    Arc::clone(DOC_EMBEDDER.get_or_init(init_embedder))
}

/// Create the best available embedder: Model2Vec if possible, fallback to trigram.
pub fn best_embedder() -> Box<dyn EmbedProvider> {
    match Model2VecEmbedder::new() {
        Ok(m) => Box::new(m),
        Err(e) => {
            eprintln!("warning: Model2Vec unavailable ({e}), using trigram fallback");
            Box::new(TrigramEmbedder::default())
        }
    }
}

/// Count the number of embeddings in the binary file at `root/.infigraph/embeddings.bin`.
pub fn embedding_count(root: &Path) -> usize {
    let path = root.join(".infigraph").join("embeddings.bin");
    let Ok(file) = std::fs::File::open(&path) else {
        return 0;
    };
    let mut r = BufReader::new(file);
    let mut buf4 = [0u8; 4];
    if r.read_exact(&mut buf4).is_err() {
        return 0;
    }
    u32::from_le_bytes(buf4) as usize
}

/// Save symbol embeddings to a binary file.
/// Format: [count:u32] then for each entry: [id_len:u32][id_bytes][dim:u32][f32 * dim]
pub fn save_embeddings(path: &Path, embeddings: &[(String, Vec<f32>)]) -> Result<()> {
    let file = std::fs::File::create(path).context("create embeddings file")?;
    let mut w = BufWriter::new(file);
    w.write_all(&(embeddings.len() as u32).to_le_bytes())?;
    for (id, vec) in embeddings {
        let id_bytes = id.as_bytes();
        w.write_all(&(id_bytes.len() as u32).to_le_bytes())?;
        w.write_all(id_bytes)?;
        w.write_all(&(vec.len() as u32).to_le_bytes())?;
        for &v in vec {
            w.write_all(&v.to_le_bytes())?;
        }
    }
    drop(w);
    invalidate_embeddings_cache();
    Ok(())
}

/// Load symbol embeddings from a binary file using memory-mapped I/O.
pub fn load_embeddings(path: &Path) -> Result<Vec<(String, Vec<f32>)>> {
    let file = std::fs::File::open(path).context("open embeddings file")?;
    let mmap = unsafe { memmap2::Mmap::map(&file) }.context("mmap embeddings file")?;
    let data = &mmap[..];

    anyhow::ensure!(data.len() >= 4, "embeddings file too small");
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut result = Vec::with_capacity(count);
    let mut pos = 4usize;

    for _ in 0..count {
        anyhow::ensure!(pos + 4 <= data.len(), "truncated embeddings file");
        let id_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        anyhow::ensure!(pos + id_len <= data.len(), "truncated embeddings file");
        let id = std::str::from_utf8(&data[pos..pos + id_len])
            .context("invalid utf8 in embedding id")?
            .to_string();
        pos += id_len;
        anyhow::ensure!(pos + 4 <= data.len(), "truncated embeddings file");
        let dim = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let float_bytes = dim * 4;
        anyhow::ensure!(pos + float_bytes <= data.len(), "truncated embeddings file");
        let vec: Vec<f32> = data[pos..pos + float_bytes]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        pos += float_bytes;
        result.push((id, vec));
    }
    Ok(result)
}

/// Incrementally update embeddings for changed files.
///
/// Loads existing embeddings, re-embeds symbols in `changed_files`, removes orphans,
/// and saves back. If `changed_files` is empty, treats all symbols as changed (full rebuild).
pub fn update_embeddings(
    backend: &dyn crate::graph::GraphBackend,
    root: &Path,
    changed_files: &[&str],
) -> Result<usize> {
    use rayon::prelude::*;
    use std::sync::Arc;

    let rows = backend.raw_query("MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.language, s.parameters, s.return_type")?;
    if rows.is_empty() {
        return Ok(0);
    }

    let emb_path = root.join(".infigraph").join("embeddings.bin");
    let mut existing: std::collections::HashMap<String, Vec<f32>> = load_embeddings(&emb_path)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let changed_set: std::collections::HashSet<&str> = changed_files.iter().copied().collect();

    let to_embed: Vec<(String, String)> = rows
        .iter()
        .filter_map(|row| {
            let id = &row[0];
            let file = row.get(3).map(|s| s.as_str()).unwrap_or("");
            if !changed_set.is_empty() && !changed_set.contains(file) && existing.contains_key(id) {
                return None;
            }
            let name = &row[1];
            let kind = &row[2];
            let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
            let lang = row.get(5).map(|s| s.as_str()).unwrap_or("");
            let params = row.get(6).map(|s| s.as_str()).unwrap_or("");
            let ret = row.get(7).map(|s| s.as_str()).unwrap_or("");
            let text = rich_symbol_text_full(kind, name, file, lang, doc, params, ret);
            Some((id.clone(), text))
        })
        .collect();

    if !to_embed.is_empty() {
        let embedder: Arc<Box<dyn EmbedProvider>> = Arc::new(best_embedder());
        const BATCH: usize = 256;
        let results: Vec<Vec<(String, Vec<f32>)>> = to_embed
            .par_chunks(BATCH)
            .map(|chunk| {
                let emb = Arc::clone(&embedder);
                let texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
                let vecs = emb.embed_batch(&texts).unwrap_or_default();
                chunk
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (id, _))| vecs.get(i).map(|v| (id.clone(), v.clone())))
                    .collect()
            })
            .collect();
        for batch in results {
            for (id, v) in batch {
                existing.insert(id, v);
            }
        }
    }

    let all_ids: std::collections::HashSet<String> = rows.iter().map(|r| r[0].clone()).collect();
    existing.retain(|id, _| all_ids.contains(id));

    let symbol_embeddings: Vec<(String, Vec<f32>)> = existing.into_iter().collect();
    let count = symbol_embeddings.len();
    save_embeddings(&emb_path, &symbol_embeddings)?;

    // Below 200K symbols, brute-force rayon dot-product is faster than HNSW.
    // Build/rebuild HNSW only when above threshold OR when an existing index
    // needs to stay current after incremental updates.
    const HNSW_THRESHOLD: usize = 200_000;
    let hnsw_path = root.join(".infigraph").join("hnsw_index.usearch");
    let should_build = count >= HNSW_THRESHOLD || hnsw_path.exists();
    if should_build {
        invalidate_hnsw_cache();
        if let Err(e) = build_hnsw_index(&symbol_embeddings, &hnsw_path, &emb_path) {
            eprintln!("warning: HNSW index build failed ({e}), vector search will use brute-force");
        }
    }

    Ok(count)
}

/// Update embeddings for remote mode — reads symbols from GraphBackend, stores in Postgres pgvector.
///
/// Scoped: when `changed_files` is non-empty, only fetches symbols from those files for embedding.
/// Orphan cleanup uses a lightweight ID-only query instead of fetching all vectors.
#[cfg(feature = "postgres")]
pub fn update_embeddings_remote(
    backend: &dyn crate::graph::GraphBackend,
    pg: &crate::meta::PostgresMetaStore,
    changed_files: &[&str],
) -> Result<usize> {
    use rayon::prelude::*;
    use std::sync::Arc;

    let existing_ids: std::collections::HashSet<String> = pg
        .all_embedding_ids("symbol")
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Fetch only symbols from changed files (or all on first index / full reindex)
    let rows = if changed_files.is_empty() || existing_ids.is_empty() {
        backend.raw_query(
            "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.language, s.parameters, s.return_type",
        )?
    } else {
        let file_list = changed_files
            .iter()
            .map(|f| format!("'{}'", f.replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");
        backend.raw_query(&format!(
            "MATCH (s:Symbol) WHERE s.file IN [{}] \
             RETURN s.id, s.name, s.kind, s.file, s.docstring, s.language, s.parameters, s.return_type",
            file_list
        ))?
    };

    if rows.is_empty() && existing_ids.is_empty() {
        return Ok(0);
    }

    let changed_set: std::collections::HashSet<&str> = changed_files.iter().copied().collect();
    let to_embed: Vec<(String, String)> = rows
        .iter()
        .filter_map(|row| {
            let id = &row[0];
            if existing_ids.contains(id) && !changed_set.is_empty() {
                let file = row.get(3).map(|s| s.as_str()).unwrap_or("");
                if !changed_set.contains(file) {
                    return None;
                }
            }
            let name = &row[1];
            let kind = &row[2];
            let file = row.get(3).map(|s| s.as_str()).unwrap_or("");
            let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
            let lang = row.get(5).map(|s| s.as_str()).unwrap_or("");
            let params = row.get(6).map(|s| s.as_str()).unwrap_or("");
            let ret = row.get(7).map(|s| s.as_str()).unwrap_or("");
            let text = rich_symbol_text_full(kind, name, file, lang, doc, params, ret);
            Some((id.clone(), text))
        })
        .collect();

    if to_embed.is_empty() {
        return Ok(existing_ids.len());
    }

    let embedder: Arc<Box<dyn EmbedProvider>> = Arc::new(best_embedder());
    const BATCH: usize = 256;
    let results: Vec<Vec<(String, Vec<f32>)>> = to_embed
        .par_chunks(BATCH)
        .map(|chunk| {
            let emb = Arc::clone(&embedder);
            let texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
            let vecs = emb.embed_batch(&texts).unwrap_or_default();
            chunk
                .iter()
                .enumerate()
                .filter_map(|(i, (id, _))| vecs.get(i).map(|v| (id.clone(), v.clone())))
                .collect()
        })
        .collect();

    let all: Vec<(String, Vec<f32>)> = results.into_iter().flatten().collect();
    let count = all.len();
    pg.upsert_embeddings_bulk(&all, "symbol")?;

    // Clean up orphan embeddings — cheap ID-only query from Neo4j
    let valid_ids: std::collections::HashSet<String> = backend
        .raw_query("MATCH (s:Symbol) RETURN s.id")?
        .into_iter()
        .filter_map(|r| r.into_iter().next())
        .collect();
    let orphans: Vec<String> = existing_ids
        .into_iter()
        .filter(|id| !valid_ids.contains(id))
        .collect();
    if !orphans.is_empty() {
        pg.delete_embeddings(&orphans)?;
    }

    Ok(count + valid_ids.len().saturating_sub(count))
}

// ---------------------------------------------------------------------------
// HNSW index (usearch) — optional acceleration for vector search
// ---------------------------------------------------------------------------

use usearch::{Index as UsearchIndex, IndexOptions, MetricKind, ScalarKind};

const HNSW_CONNECTIVITY: usize = 32;
const HNSW_EXPANSION_ADD: usize = 200;
const HNSW_EXPANSION_SEARCH: usize = 256;
const HNSW_OVERSAMPLE: usize = 20;

static HNSW_CACHE: OnceLock<Mutex<Option<CachedHnsw>>> = OnceLock::new();

struct CachedHnsw {
    path: PathBuf,
    modified: std::time::SystemTime,
    index: UsearchIndex,
    id_map: Vec<String>,
}

fn hnsw_cache_lock() -> &'static Mutex<Option<CachedHnsw>> {
    HNSW_CACHE.get_or_init(|| Mutex::new(None))
}

fn hnsw_opts(dim: usize) -> IndexOptions {
    IndexOptions {
        dimensions: dim,
        metric: MetricKind::IP,
        quantization: ScalarKind::F32,
        connectivity: HNSW_CONNECTIVITY,
        expansion_add: HNSW_EXPANSION_ADD,
        ..IndexOptions::default()
    }
}

/// Build an HNSW index from embeddings and save to disk.
/// Returns the number of vectors indexed.
pub fn build_hnsw_index(
    embeddings: &[(String, Vec<f32>)],
    index_path: &Path,
    embeddings_path: &Path,
) -> Result<usize> {
    if embeddings.is_empty() {
        return Ok(0);
    }

    let dim = embeddings[0].1.len();
    let n = embeddings.len();
    let threads = std::thread::available_parallelism()
        .map(|t| t.get())
        .unwrap_or(4);

    let index =
        UsearchIndex::new(&hnsw_opts(dim)).map_err(|e| anyhow::anyhow!("usearch create: {e}"))?;
    index
        .reserve(n)
        .map_err(|e| anyhow::anyhow!("usearch reserve: {e}"))?;

    let index = std::sync::Arc::new(index);
    let chunk_size = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (chunk_idx, chunk) in embeddings.chunks(chunk_size).enumerate() {
            let idx = std::sync::Arc::clone(&index);
            let offset = chunk_idx * chunk_size;
            s.spawn(move || {
                for (i, (_, v)) in chunk.iter().enumerate() {
                    let _ = idx.add((offset + i) as u64, v);
                }
            });
        }
    });

    let path_str = index_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 index path"))?;
    index
        .save(path_str)
        .map_err(|e| anyhow::anyhow!("usearch save: {e}"))?;

    let emb_mtime = std::fs::metadata(embeddings_path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let sidecar_path = index_path.with_extension("meta");
    write_binary_sidecar(&sidecar_path, emb_mtime, n, dim, embeddings)?;

    invalidate_hnsw_cache();
    Ok(n)
}

/// Invalidate the HNSW cache.
pub fn invalidate_hnsw_cache() {
    if let Ok(mut guard) = hnsw_cache_lock().lock() {
        *guard = None;
    }
}

fn write_binary_sidecar(
    path: &Path,
    emb_mtime: std::time::SystemTime,
    count: usize,
    dim: usize,
    embeddings: &[(String, Vec<f32>)],
) -> Result<()> {
    let mtime_secs = emb_mtime
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut buf = Vec::with_capacity(1 + 4 + 4 + 8 + count * 40);
    buf.push(1u8); // version
    buf.extend_from_slice(&(count as u32).to_le_bytes());
    buf.extend_from_slice(&(dim as u32).to_le_bytes());
    buf.extend_from_slice(&mtime_secs.to_le_bytes());
    for (id, _) in embeddings {
        let id_bytes = id.as_bytes();
        buf.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(id_bytes);
    }
    std::fs::write(path, &buf).context("write binary hnsw sidecar")?;
    Ok(())
}

struct SidecarData {
    emb_mtime_secs: u64,
    dim: usize,
    ids: Vec<String>,
}

fn read_sidecar(bytes: &[u8]) -> Result<SidecarData> {
    if bytes.is_empty() {
        anyhow::bail!("empty sidecar file");
    }
    if bytes[0] == b'{' {
        // JSON fallback for pre-upgrade sidecars
        let sidecar: serde_json::Value =
            serde_json::from_slice(bytes).context("parse json hnsw sidecar")?;
        let mtime = sidecar["emb_mtime_secs"].as_u64().unwrap_or(0);
        let dim = sidecar["dim"].as_u64().unwrap_or(256) as usize;
        let ids = sidecar["ids"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        return Ok(SidecarData {
            emb_mtime_secs: mtime,
            dim,
            ids,
        });
    }
    // Binary format: [version:u8] [count:u32] [dim:u32] [emb_mtime_secs:u64] foreach: [id_len:u32] [id_bytes]
    anyhow::ensure!(bytes[0] == 1, "unsupported sidecar version {}", bytes[0]);
    anyhow::ensure!(bytes.len() >= 17, "sidecar too small for header");
    let count = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
    let dim = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;
    let emb_mtime_secs = u64::from_le_bytes(bytes[9..17].try_into().unwrap());
    let mut ids = Vec::with_capacity(count);
    let mut pos = 17usize;
    for _ in 0..count {
        anyhow::ensure!(pos + 4 <= bytes.len(), "truncated sidecar");
        let id_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        anyhow::ensure!(pos + id_len <= bytes.len(), "truncated sidecar id");
        let id = String::from_utf8_lossy(&bytes[pos..pos + id_len]).into_owned();
        pos += id_len;
        ids.push(id);
    }
    Ok(SidecarData {
        emb_mtime_secs,
        dim,
        ids,
    })
}

/// Query result from HNSW search.
pub struct HnswResult {
    pub id: String,
    pub score: f32,
}

fn query_index(
    index: &UsearchIndex,
    id_map: &[String],
    query: &[f32],
    top_k: usize,
) -> Result<Vec<HnswResult>> {
    let fetch_k = top_k * HNSW_OVERSAMPLE;
    let results = index
        .search(query, fetch_k)
        .map_err(|e| anyhow::anyhow!("usearch search: {e}"))?;
    let out: Vec<HnswResult> = results
        .keys
        .iter()
        .zip(results.distances.iter())
        .filter_map(|(&key, &dist)| {
            let idx = key as usize;
            id_map.get(idx).map(|id| HnswResult {
                id: id.clone(),
                score: 1.0 - dist,
            })
        })
        .collect();
    Ok(out)
}

/// Search the HNSW index, returning top-k results by inner product similarity.
/// Returns None if no valid index exists (caller should fall back to brute-force).
pub fn search_hnsw(
    index_path: &Path,
    embeddings_path: &Path,
    query: &[f32],
    top_k: usize,
) -> Result<Option<Vec<HnswResult>>> {
    let sidecar_path = index_path.with_extension("meta");
    if !index_path.exists() || !sidecar_path.exists() {
        return Ok(None);
    }

    let emb_mtime_secs = std::fs::metadata(embeddings_path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let canon = index_path
        .canonicalize()
        .unwrap_or_else(|_| index_path.to_path_buf());
    let idx_mtime = std::fs::metadata(index_path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);

    // Try cache first
    let guard = hnsw_cache_lock().lock().unwrap();
    if let Some(cached) = guard.as_ref() {
        if cached.path == canon && cached.modified == idx_mtime {
            return Ok(Some(query_index(
                &cached.index,
                &cached.id_map,
                query,
                top_k,
            )?));
        }
    }
    drop(guard);

    // Cache miss — load sidecar and validate freshness
    let sidecar_bytes = std::fs::read(&sidecar_path).context("read hnsw sidecar")?;
    let sidecar = read_sidecar(&sidecar_bytes)?;
    if sidecar.emb_mtime_secs != emb_mtime_secs {
        return Ok(None);
    }
    let id_map = sidecar.ids;

    let dim = sidecar.dim;
    let path_str = index_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 index path"))?;
    let index = UsearchIndex::new(&hnsw_opts(dim))
        .map_err(|e| anyhow::anyhow!("usearch create for load: {e}"))?;
    index
        .view(path_str)
        .map_err(|e| anyhow::anyhow!("usearch view: {e}"))?;
    index.change_expansion_search(HNSW_EXPANSION_SEARCH);

    let out = query_index(&index, &id_map, query, top_k)?;

    let mut guard = hnsw_cache_lock().lock().unwrap();
    *guard = Some(CachedHnsw {
        path: canon,
        modified: idx_mtime,
        index,
        id_map,
    });

    Ok(Some(out))
}

/// FNV-1a hash (deterministic, fast, good distribution).
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
