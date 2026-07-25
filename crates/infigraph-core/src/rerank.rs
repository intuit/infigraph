//! Optional remote reranking via the Cohere Rerank API.
//!
//! Activated when `COHERE_API_KEY` is set. Uses `rerank-v3.5` by default
//! (override with `INFIGRAPH_COHERE_RERANK_MODEL`). Callers should treat this
//! as a best-effort second-stage reranker: on any API failure, fall back to
//! the local score ordering.

use anyhow::{Context, Result};

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
