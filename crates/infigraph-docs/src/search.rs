use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;

use infigraph_core::embed::{cosine_similarity, doc_embedder, load_embeddings_cached, search_hnsw};

use crate::backend::DocBackend;

#[derive(Debug, Clone)]
pub struct DocSearchResult {
    pub chunk_id: String,
    pub doc_file: String,
    pub heading: Option<String>,
    pub text: String,
    pub score: f32,
    pub bm25_score: f32,
    pub vector_score: f32,
    pub start_offset: usize,
    pub end_offset: usize,
    pub page: Option<usize>,
}

const K1: f32 = 1.2;
const B: f32 = 0.75;

pub struct DocBM25Index {
    docs: Vec<(String, String)>,
    inverted: HashMap<String, Vec<(usize, f32)>>,
    avg_doc_len: f32,
}

impl DocBM25Index {
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
}

pub fn hybrid_doc_search(
    query: &str,
    store: &dyn DocBackend,
    root: &Path,
    limit: usize,
    alpha: f32,
) -> Result<Vec<DocSearchResult>> {
    #[cfg(feature = "remote")]
    if is_remote_mode() {
        return hybrid_doc_search_remote(query, store, limit, alpha);
    }
    hybrid_doc_search_in_dir(query, store, &root.join(".infigraph"), limit, alpha)
}

pub fn hybrid_doc_search_in_dir(
    query: &str,
    store: &dyn DocBackend,
    artifact_dir: &Path,
    limit: usize,
    alpha: f32,
) -> Result<Vec<DocSearchResult>> {
    let chunks = store.get_all_chunks()?;

    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    let bm25_index = DocBM25Index::build(chunks.clone());
    let bm25_results = bm25_index.search(query, limit * 3);

    // Normalize BM25
    let max_bm25 = bm25_results
        .first()
        .map(|(_, s)| *s)
        .unwrap_or(1.0)
        .max(0.001);
    let bm25_scores: HashMap<usize, f32> = bm25_results
        .iter()
        .map(|(idx, s)| (*idx, s / max_bm25))
        .collect();

    // Vector search
    let emb_path = artifact_dir.join("docs_embeddings.bin");
    let hnsw_path = artifact_dir.join("docs_hnsw_index.usearch");

    let embedder = doc_embedder();
    let query_vec = embedder.embed(query)?;

    let vector_scores: HashMap<usize, f32> = if hnsw_path.exists() {
        // HNSW path
        if let Ok(Some(hnsw_results)) = search_hnsw(&hnsw_path, &emb_path, &query_vec, limit * 3) {
            let id_to_idx: HashMap<&str, usize> = chunks
                .iter()
                .enumerate()
                .map(|(i, (id, _))| (id.as_str(), i))
                .collect();
            hnsw_results
                .into_iter()
                .filter_map(|r| id_to_idx.get(r.id.as_str()).map(|&idx| (idx, r.score)))
                .collect()
        } else {
            brute_force_vector(&chunks, &emb_path, &query_vec, limit * 3)?
        }
    } else {
        brute_force_vector(&chunks, &emb_path, &query_vec, limit * 3)?
    };

    // Normalize vector
    let max_vec = vector_scores.values().cloned().fold(0.001f32, f32::max);

    // Combine
    let mut all_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    all_indices.extend(bm25_scores.keys());
    all_indices.extend(vector_scores.keys());

    let mut combined: Vec<(usize, f32, f32, f32)> = all_indices
        .into_iter()
        .map(|idx| {
            let b = bm25_scores.get(&idx).copied().unwrap_or(0.0);
            let v = vector_scores.get(&idx).copied().unwrap_or(0.0) / max_vec;
            let score = (1.0 - alpha) * b + alpha * v;
            (idx, score, b, v)
        })
        .collect();
    combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    combined.truncate(limit);

    // Fetch chunk details
    let chunk_ids: Vec<&str> = combined
        .iter()
        .map(|(idx, _, _, _)| chunks[*idx].0.as_str())
        .collect();
    let details = store.get_chunk_details(&chunk_ids)?;
    let detail_map: HashMap<&str, &crate::store::ChunkDetail> =
        details.iter().map(|d| (d.id.as_str(), d)).collect();

    let results = combined
        .into_iter()
        .filter_map(|(idx, score, bm25, vec_s)| {
            let chunk_id = &chunks[idx].0;
            let detail = detail_map.get(chunk_id.as_str())?;
            Some(DocSearchResult {
                chunk_id: chunk_id.clone(),
                doc_file: detail.doc_file.clone(),
                heading: detail.heading.clone(),
                text: detail.text.clone(),
                score,
                bm25_score: bm25,
                vector_score: vec_s,
                start_offset: detail.start_offset,
                end_offset: detail.end_offset,
                page: detail.page,
            })
        })
        .collect();

    Ok(results)
}

fn brute_force_vector(
    chunks: &[(String, String)],
    emb_path: &Path,
    query_vec: &[f32],
    limit: usize,
) -> Result<HashMap<usize, f32>> {
    let embeddings = load_embeddings_cached(emb_path).unwrap_or_default();
    let emb_map: HashMap<&str, &Vec<f32>> =
        embeddings.iter().map(|(id, v)| (id.as_str(), v)).collect();

    let id_to_idx: HashMap<&str, usize> = chunks
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();

    let mut scores: Vec<(usize, f32)> = emb_map
        .par_iter()
        .filter_map(|(id, vec)| {
            let idx = id_to_idx.get(id)?;
            let sim = cosine_similarity(query_vec, vec);
            Some((*idx, sim))
        })
        .collect();

    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scores.truncate(limit);
    Ok(scores.into_iter().collect())
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() > 1)
        .map(String::from)
        .collect()
}

#[cfg(feature = "remote")]
fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

#[cfg(feature = "remote")]
fn hybrid_doc_search_remote(
    query_str: &str,
    store: &dyn DocBackend,
    limit: usize,
    alpha: f32,
) -> Result<Vec<DocSearchResult>> {
    let chunks = store.get_all_chunks()?;
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    let bm25_index = DocBM25Index::build(chunks.clone());
    let bm25_results = bm25_index.search(query_str, limit * 3);

    let max_bm25 = bm25_results
        .first()
        .map(|(_, s)| *s)
        .unwrap_or(1.0)
        .max(0.001);
    let bm25_scores: HashMap<usize, f32> = bm25_results
        .iter()
        .map(|(idx, s)| (*idx, s / max_bm25))
        .collect();

    let id_to_idx: HashMap<&str, usize> = chunks
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();

    let vector_scores: HashMap<usize, f32> =
        if let Ok(pg) = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached() {
            let embedder = infigraph_core::embed::doc_embedder();
            if let Ok(query_vec) = embedder.embed(query_str) {
                pg.search_nearest(&query_vec, "doc_chunk", limit * 3)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(id, dist)| {
                        let score = 1.0 - dist;
                        id_to_idx.get(id.as_str()).map(|&idx| (idx, score))
                    })
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

    let max_vec = vector_scores.values().cloned().fold(0.001f32, f32::max);

    let mut all_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    all_indices.extend(bm25_scores.keys());
    all_indices.extend(vector_scores.keys());

    let mut combined: Vec<(usize, f32, f32, f32)> = all_indices
        .into_iter()
        .map(|idx| {
            let b = bm25_scores.get(&idx).copied().unwrap_or(0.0);
            let v = vector_scores.get(&idx).copied().unwrap_or(0.0) / max_vec;
            let score = (1.0 - alpha) * b + alpha * v;
            (idx, score, b, v)
        })
        .collect();
    combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    combined.truncate(limit);

    let chunk_ids: Vec<&str> = combined
        .iter()
        .map(|(idx, _, _, _)| chunks[*idx].0.as_str())
        .collect();
    let details = store.get_chunk_details(&chunk_ids)?;
    let detail_map: HashMap<&str, &crate::store::ChunkDetail> =
        details.iter().map(|d| (d.id.as_str(), d)).collect();

    let results = combined
        .into_iter()
        .filter_map(|(idx, score, bm25, vec_s)| {
            let chunk_id = &chunks[idx].0;
            let detail = detail_map.get(chunk_id.as_str())?;
            Some(DocSearchResult {
                chunk_id: chunk_id.clone(),
                doc_file: detail.doc_file.clone(),
                heading: detail.heading.clone(),
                text: detail.text.clone(),
                score,
                bm25_score: bm25,
                vector_score: vec_s,
                start_offset: detail.start_offset,
                end_offset: detail.end_offset,
                page: detail.page,
            })
        })
        .collect();

    Ok(results)
}
