//! Optional remote reranking via the Cohere Rerank API.
//!
//! Local-development add-on: everything Cohere-specific lives in this file so
//! callers only need a single `maybe_rerank(...)` hook line. Activated when
//! `COHERE_API_KEY` is set. Uses `rerank-v3.5` by default (override with
//! `INFIGRAPH_COHERE_RERANK_MODEL`). Best-effort second-stage reranker: on any
//! API failure, the local score ordering is kept.

use anyhow::{Context, Result};

use crate::search::SearchResult;

const ENDPOINT: &str = "https://api.cohere.com/v2/rerank";
/// Cohere recommends at most 1000 documents per request; we cap far lower
/// since only top search candidates are reranked.
const MAX_DOCUMENTS: usize = 200;

/// Whether Cohere reranking is enabled (`COHERE_API_KEY` set and non-empty).
pub fn cohere_enabled() -> bool {
    std::env::var("COHERE_API_KEY")
        .map(|k| !k.is_empty())
        .unwrap_or(false)
}

fn model() -> String {
    std::env::var("INFIGRAPH_COHERE_RERANK_MODEL").unwrap_or_else(|_| "rerank-v3.5".to_string())
}

/// Single-call hook for search pipelines: when `COHERE_API_KEY` is set, rerank
/// the top candidates of `results` in place (scores replaced with Cohere
/// relevance in `[0, 1]`). `docs` maps symbol IDs to embedding text; symbols
/// missing from it fall back to a `kind name in file` description. Best-effort:
/// on any API failure the local ordering is kept and a warning is printed.
pub fn maybe_rerank(
    query: &str,
    results: &mut Vec<SearchResult>,
    docs: &[(String, String)],
    limit: usize,
) {
    if !cohere_enabled() || results.is_empty() {
        return;
    }
    let candidate_count = results
        .len()
        .min(limit.saturating_mul(2).max(limit))
        .min(100);
    let doc_text: std::collections::HashMap<&str, &str> = docs
        .iter()
        .map(|(id, text)| (id.as_str(), text.as_str()))
        .collect();
    let texts: Vec<String> = results[..candidate_count]
        .iter()
        .map(|r| {
            doc_text
                .get(r.symbol_id.as_str())
                .map(|t| (*t).to_string())
                .unwrap_or_else(|| format!("{} {} in {}", r.kind, r.name, r.file))
        })
        .collect();
    match cohere_rerank(query, &texts, candidate_count) {
        Ok(ranked) => {
            let mut reordered: Vec<SearchResult> = ranked
                .iter()
                .filter_map(|(idx, score)| {
                    results.get(*idx).map(|r| {
                        let mut r = r.clone();
                        r.score = *score;
                        r
                    })
                })
                .collect();
            reordered.extend(results[candidate_count..].iter().cloned());
            *results = reordered;
        }
        Err(e) => eprintln!("warning: cohere rerank failed, using local ranking: {e}"),
    }
}

/// Rerank `documents` against `query` with the Cohere Rerank API.
///
/// Returns `(original_index, relevance_score)` pairs ordered from most to
/// least relevant, truncated to `top_n`. Relevance scores are in `[0, 1]`.
pub fn cohere_rerank(query: &str, documents: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
    let api_key = std::env::var("COHERE_API_KEY").context("COHERE_API_KEY not set")?;
    anyhow::ensure!(!documents.is_empty(), "cohere rerank: no documents");
    anyhow::ensure!(
        documents.len() <= MAX_DOCUMENTS,
        "cohere rerank: too many documents ({}, max {})",
        documents.len(),
        MAX_DOCUMENTS
    );

    let body = serde_json::json!({
        "model": model(),
        "query": query,
        "documents": documents,
        "top_n": top_n,
    });
    let resp = ureq::post(ENDPOINT)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| anyhow::anyhow!("cohere rerank request failed: {e}"))?;
    let json: serde_json::Value = resp
        .into_json()
        .context("cohere rerank: invalid JSON response")?;
    let results = json["results"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing 'results' in response"))?;

    let mut ranked = Vec::with_capacity(results.len());
    for item in results {
        let idx = item["index"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing 'index'"))?
            as usize;
        let score = item["relevance_score"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing 'relevance_score'"))?
            as f32;
        anyhow::ensure!(idx < documents.len(), "cohere rerank: index out of range");
        ranked.push((idx, score));
    }
    Ok(ranked)
}
