use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;

use crate::embed::{self, EmbedProvider};

/// A search result with combined score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub symbol_id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub score: f32,
    pub bm25_score: f32,
    pub vector_score: f32,
    pub docstring: Option<String>,
}

/// BM25 parameters.
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Simple BM25 scorer over symbol text (name + docstring).
#[derive(Clone)]
pub struct BM25Index {
    /// symbol_id -> text
    docs: Vec<(String, String)>,
    /// term -> list of (doc_index, term_frequency)
    inverted: HashMap<String, Vec<(usize, f32)>>,
    avg_doc_len: f32,
}

impl BM25Index {
    /// Build a BM25 index from symbol (id, text) pairs.
    pub fn build(docs: Vec<(String, String)>) -> Self {
        let n = docs.len();
        let mut inverted: HashMap<String, Vec<(usize, f32)>> = HashMap::new();
        let mut total_len = 0usize;

        for (i, (_id, text)) in docs.iter().enumerate() {
            let tokens = tokenize(text);
            total_len += tokens.len();

            let mut tf_map: HashMap<&str, f32> = HashMap::new();
            for t in &tokens {
                *tf_map.entry(t.as_str()).or_default() += 1.0;
            }

            for (term, tf) in tf_map {
                inverted.entry(term.to_string()).or_default().push((i, tf));
            }
        }

        let avg_doc_len = if n > 0 {
            total_len as f32 / n as f32
        } else {
            1.0
        };

        Self {
            docs,
            inverted,
            avg_doc_len,
        }
    }

    /// Score all documents against a query. Returns (doc_index, score) sorted descending.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, f32)> {
        let query_tokens = tokenize(query);
        let n = self.docs.len() as f32;
        let mut scores = vec![0.0f32; self.docs.len()];

        for token in &query_tokens {
            if let Some(postings) = self.inverted.get(token.as_str()) {
                let df = postings.len() as f32;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

                for &(doc_idx, tf) in postings {
                    let doc_len = tokenize(&self.docs[doc_idx].1).len() as f32;
                    let tf_norm =
                        (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc_len / self.avg_doc_len));
                    scores[doc_idx] += idf * tf_norm;
                }
            }
        }

        let mut results: Vec<(usize, f32)> = scores
            .into_iter()
            .enumerate()
            .filter(|(_, s)| *s > 0.0)
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }

    pub fn doc_id(&self, idx: usize) -> &str {
        &self.docs[idx].0
    }

    pub fn doc_text(&self, idx: usize) -> &str {
        &self.docs[idx].1
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut buf = Vec::new();
        buf.push(1u8); // version
        buf.extend_from_slice(&self.avg_doc_len.to_le_bytes());
        buf.extend_from_slice(&(self.docs.len() as u32).to_le_bytes());
        for (id, text) in &self.docs {
            let id_b = id.as_bytes();
            buf.extend_from_slice(&(id_b.len() as u32).to_le_bytes());
            buf.extend_from_slice(id_b);
            let text_b = text.as_bytes();
            buf.extend_from_slice(&(text_b.len() as u32).to_le_bytes());
            buf.extend_from_slice(text_b);
        }
        buf.extend_from_slice(&(self.inverted.len() as u32).to_le_bytes());
        for (term, postings) in &self.inverted {
            let tb = term.as_bytes();
            buf.extend_from_slice(&(tb.len() as u32).to_le_bytes());
            buf.extend_from_slice(tb);
            buf.extend_from_slice(&(postings.len() as u32).to_le_bytes());
            for &(doc_idx, tf) in postings {
                buf.extend_from_slice(&(doc_idx as u32).to_le_bytes());
                buf.extend_from_slice(&tf.to_le_bytes());
            }
        }
        std::fs::write(path, &buf).map_err(|e| anyhow::anyhow!("write bm25 cache: {}", e))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|e| anyhow::anyhow!("read bm25 cache: {}", e))?;
        anyhow::ensure!(
            !data.is_empty() && data[0] == 1,
            "unsupported bm25 cache version"
        );
        anyhow::ensure!(data.len() >= 9, "bm25 cache too small");
        let avg_doc_len = f32::from_le_bytes(data[1..5].try_into().unwrap());
        let doc_count = u32::from_le_bytes(data[5..9].try_into().unwrap()) as usize;
        let mut pos = 9usize;
        let mut docs = Vec::with_capacity(doc_count);
        for _ in 0..doc_count {
            let id_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            let id = String::from_utf8_lossy(&data[pos..pos + id_len]).into_owned();
            pos += id_len;
            let text_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            let text = String::from_utf8_lossy(&data[pos..pos + text_len]).into_owned();
            pos += text_len;
            docs.push((id, text));
        }
        let term_count = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut inverted = HashMap::with_capacity(term_count);
        for _ in 0..term_count {
            let tl = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            let term = String::from_utf8_lossy(&data[pos..pos + tl]).into_owned();
            pos += tl;
            let pc = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            let mut postings = Vec::with_capacity(pc);
            for _ in 0..pc {
                let doc_idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                let tf = f32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
                pos += 4;
                postings.push((doc_idx, tf));
            }
            inverted.insert(term, postings);
        }
        Ok(Self {
            docs,
            inverted,
            avg_doc_len,
        })
    }
}

/// Pre-computed BM25 and vector scores before alpha blending.
pub struct RawScores {
    /// symbol_id -> normalized BM25 score
    pub bm25: HashMap<String, f32>,
    /// symbol_id -> normalized vector score
    pub vector: HashMap<String, f32>,
}

/// Compute BM25 and vector scores separately. Call once, then blend with
/// multiple alpha values via `combine_scores`.
///
/// When `hnsw_index_path` and `embeddings_path` are provided and a valid HNSW
/// index exists on disk, vector scoring uses the index (~1ms) instead of
/// brute-force scanning all embeddings (~20-30ms).
pub fn compute_raw_scores(
    query: &str,
    bm25_index: &BM25Index,
    embedder: &dyn EmbedProvider,
    symbol_embeddings: &[(String, Vec<f32>)],
    oversample: usize,
    hnsw_index_path: Option<&Path>,
    embeddings_path: Option<&Path>,
) -> Result<RawScores> {
    let bm25_results = bm25_index.search(query, oversample);
    let bm25_max = bm25_results
        .first()
        .map(|(_, s)| *s)
        .unwrap_or(1.0)
        .max(0.001);

    let mut bm25_map: HashMap<String, f32> = HashMap::new();
    for (idx, score) in &bm25_results {
        let id = bm25_index.doc_id(*idx).to_string();
        bm25_map.insert(id, score / bm25_max);
    }

    let query_embedding = embedder.embed(query)?;

    // HNSW only pays off above ~200K embeddings where brute-force exceeds index
    // load + search time. Below that, rayon dot-product is faster.
    const HNSW_THRESHOLD: usize = 200_000;
    let use_hnsw = symbol_embeddings.len() >= HNSW_THRESHOLD;
    let vec_scores = if use_hnsw {
        if let (Some(idx_path), Some(emb_path)) = (hnsw_index_path, embeddings_path) {
            match embed::search_hnsw(idx_path, emb_path, &query_embedding, oversample) {
                Ok(Some(candidates)) => {
                    let emb_lookup: HashMap<&str, &[f32]> = symbol_embeddings
                        .iter()
                        .map(|(id, v)| (id.as_str(), v.as_slice()))
                        .collect();
                    let mut reranked: Vec<(String, f32)> = candidates
                        .into_iter()
                        .filter_map(|r| {
                            emb_lookup
                                .get(r.id.as_str())
                                .map(|emb| (r.id, embed::cosine_similarity(&query_embedding, emb)))
                        })
                        .collect();
                    reranked.sort_unstable_by(|a, b| {
                        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    reranked.truncate(oversample);
                    reranked
                }
                _ => brute_force_vector_scores(&query_embedding, symbol_embeddings, oversample),
            }
        } else {
            brute_force_vector_scores(&query_embedding, symbol_embeddings, oversample)
        }
    } else {
        brute_force_vector_scores(&query_embedding, symbol_embeddings, oversample)
    };

    let vec_max = vec_scores
        .first()
        .map(|(_, s)| *s)
        .unwrap_or(1.0)
        .max(0.001);

    let mut vector_map: HashMap<String, f32> = HashMap::new();
    for (id, score) in &vec_scores {
        vector_map.insert(id.clone(), score / vec_max);
    }

    Ok(RawScores {
        bm25: bm25_map,
        vector: vector_map,
    })
}

fn brute_force_vector_scores(
    query_embedding: &[f32],
    symbol_embeddings: &[(String, Vec<f32>)],
    oversample: usize,
) -> Vec<(String, f32)> {
    let mut vec_scores: Vec<(String, f32)> = symbol_embeddings
        .par_iter()
        .map(|(id, emb)| {
            let sim = embed::cosine_similarity(query_embedding, emb);
            (id.clone(), sim)
        })
        .collect();
    vec_scores.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    vec_scores.truncate(oversample);
    vec_scores
}

/// Blend pre-computed raw scores with a given alpha. Returns sorted results.
pub fn combine_scores(raw: &RawScores, alpha: f32, limit: usize) -> Vec<SearchResult> {
    let all_ids: std::collections::HashSet<&String> =
        raw.bm25.keys().chain(raw.vector.keys()).collect();

    let mut results: Vec<SearchResult> = all_ids
        .into_iter()
        .map(|id| {
            let bm25 = raw.bm25.get(id).copied().unwrap_or(0.0);
            let vec = raw.vector.get(id).copied().unwrap_or(0.0);
            let score = (1.0 - alpha) * bm25 + alpha * vec;
            SearchResult {
                symbol_id: id.clone(),
                name: String::new(),
                kind: String::new(),
                file: String::new(),
                score,
                bm25_score: bm25,
                vector_score: vec,
                docstring: None,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);
    results
}

/// Hybrid search combining BM25 text relevance with vector similarity.
#[allow(clippy::too_many_arguments)]
pub fn hybrid_search(
    query: &str,
    bm25_index: &BM25Index,
    embedder: &dyn EmbedProvider,
    symbol_embeddings: &[(String, Vec<f32>)],
    limit: usize,
    alpha: f32, // 0.0 = pure BM25, 1.0 = pure vector
    hnsw_index_path: Option<&Path>,
    embeddings_path: Option<&Path>,
) -> Result<Vec<SearchResult>> {
    let raw = compute_raw_scores(
        query,
        bm25_index,
        embedder,
        symbol_embeddings,
        limit * 2,
        hnsw_index_path,
        embeddings_path,
    )?;
    Ok(combine_scores(&raw, alpha, limit))
}

/// Simple whitespace + punctuation tokenizer with lowercasing.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty() && s.len() > 1)
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// grep-like text search
// ---------------------------------------------------------------------------

/// A single matching line from a grep search.
#[derive(Debug, Clone)]
pub struct GrepMatch {
    /// Relative file path within the project.
    pub file: String,
    /// 1-based line number.
    pub line_number: usize,
    /// The full text of the matching line (trimmed of trailing newline).
    pub line_text: String,
}

/// Walk `root`, optionally filtering by a glob `file_pattern`, and search every
/// file for lines matching `pattern` (a regex).  Returns up to `limit` matches.
pub fn grep_search(
    root: &Path,
    pattern: &str,
    file_pattern: Option<&str>,
    limit: usize,
) -> Result<Vec<GrepMatch>> {
    let re =
        Regex::new(pattern).map_err(|e| anyhow::anyhow!("invalid regex '{}': {}", pattern, e))?;

    let glob_pat = file_pattern
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid file pattern: {}", e))?;

    let mut matches = Vec::new();
    walk_and_search(root, &re, &glob_pat, limit, &mut matches)?;
    Ok(matches)
}

fn walk_and_search(
    base: &Path,
    re: &Regex,
    glob_pat: &Option<glob::Pattern>,
    limit: usize,
    matches: &mut Vec<GrepMatch>,
) -> Result<()> {
    for result in crate::ignore_rules::walk_builder(base).build() {
        if matches.len() >= limit {
            return Ok(());
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if let Some(ref gp) = glob_pat {
            if !gp.matches(&rel) {
                continue;
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            if matches.len() >= limit {
                return Ok(());
            }
            if re.is_match(line) {
                matches.push(GrepMatch {
                    file: rel.clone(),
                    line_number: idx + 1,
                    line_text: line.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Read a range of lines [start_line..=end_line] (1-based) from a file.
/// Returns the source text of those lines concatenated.
pub fn read_lines_from_file(path: &Path, start_line: u32, end_line: u32) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;
    let lines: Vec<&str> = content.lines().collect();
    let start = (start_line as usize).saturating_sub(1);
    let end = (end_line as usize).min(lines.len());
    if start >= lines.len() {
        return Ok(String::new());
    }
    Ok(lines[start..end].join("\n"))
}
