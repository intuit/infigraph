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
/// Overall request deadline (connect + write + read). Reranking is a
/// best-effort second stage inside interactive search — never hang on it.
const REQUEST_TIMEOUT_SECS: u64 = 30;

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
/// Returns `Some(n)` when a Cohere rerank was applied — the first `n`
/// results had their scores replaced with Cohere relevance (the tail keeps
/// local scores, on a different scale) — so callers can re-apply any local
/// score adjustments (e.g. grep boosts) to that prefix. Returns `None` when
/// rerank was disabled, skipped, or failed (local scores left intact).
pub fn maybe_rerank(
    query: &str,
    results: &mut Vec<SearchResult>,
    docs: &[(String, String)],
    limit: usize,
) -> Option<usize> {
    if !cohere_enabled() || results.is_empty() {
        return None;
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
            // cohere_rerank guarantees a complete permutation of the
            // candidates (exact count, unique, in-range), so no candidate can
            // be dropped here.
            let mut reordered: Vec<SearchResult> = Vec::with_capacity(results.len());
            for (idx, score) in &ranked {
                let mut r = results[*idx].clone();
                r.score = *score;
                reordered.push(r);
            }
            reordered.extend(results[candidate_count..].iter().cloned());
            *results = reordered;
            Some(candidate_count)
        }
        Err(e) => {
            eprintln!("warning: cohere rerank failed, using local ranking: {e}");
            None
        }
    }
}

/// Rerank `documents` against `query` with the Cohere Rerank API.
///
/// Returns `(original_index, relevance_score)` pairs ordered from most to
/// least relevant. `top_n` must equal `documents.len()` — the result is a
/// complete permutation of the candidates. Relevance scores are in `[0, 1]`.
pub fn cohere_rerank(query: &str, documents: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
    let api_key = std::env::var("COHERE_API_KEY").context("COHERE_API_KEY not set")?;
    anyhow::ensure!(!documents.is_empty(), "cohere rerank: no documents");
    // Contract: callers must rerank the full candidate set. The response
    // validation below requires a complete permutation, so a smaller top_n
    // (a legitimate partial Cohere response) would be misreported as an
    // incomplete-response error.
    anyhow::ensure!(
        top_n == documents.len(),
        "cohere rerank: top_n ({top_n}) must equal document count ({})",
        documents.len()
    );
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
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build();
    let resp = agent
        .post(ENDPOINT)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| anyhow::anyhow!("cohere rerank request failed: {e}"))?;
    let json: serde_json::Value = resp
        .into_json()
        .context("cohere rerank: invalid JSON response")?;
    parse_rerank_response(&json, documents.len())
}

/// Parse and validate a Cohere rerank API response.
///
/// Every item must carry a valid, unique, in-range integer `index` and a
/// numeric `relevance_score`. Because requests always set
/// `top_n = documents.len()`, the response must rank every document — an
/// incomplete or oversized result set is rejected as malformed.
fn parse_rerank_response(json: &serde_json::Value, doc_count: usize) -> Result<Vec<(usize, f32)>> {
    let results = json["results"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing 'results' in response"))?;
    anyhow::ensure!(
        results.len() == doc_count,
        "cohere rerank: {} results for {} documents",
        results.len(),
        doc_count
    );

    let mut seen = vec![false; doc_count];
    let mut ranked = Vec::with_capacity(results.len());
    for item in results {
        let idx = item["index"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing or invalid 'index'"))?
            as usize;
        let score = item["relevance_score"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("cohere rerank: missing 'relevance_score'"))?
            as f32;
        anyhow::ensure!(idx < doc_count, "cohere rerank: index {idx} out of range");
        anyhow::ensure!(!seen[idx], "cohere rerank: duplicate index {idx}");
        seen[idx] = true;
        ranked.push((idx, score));
    }
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(items: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "results": items })
    }

    #[test]
    fn parses_valid_response() {
        let json = resp(serde_json::json!([
            { "index": 1, "relevance_score": 0.9 },
            { "index": 0, "relevance_score": 0.4 },
        ]));
        let ranked = parse_rerank_response(&json, 2).unwrap();
        assert_eq!(ranked, vec![(1, 0.9), (0, 0.4)]);
    }

    #[test]
    fn rejects_duplicate_index() {
        let json = resp(serde_json::json!([
            { "index": 0, "relevance_score": 0.9 },
            { "index": 0, "relevance_score": 0.4 },
        ]));
        let err = parse_rerank_response(&json, 2).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_index() {
        let json = resp(serde_json::json!([
            { "index": 7, "relevance_score": 0.9 },
        ]));
        let err = parse_rerank_response(&json, 1).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
    }

    #[test]
    fn rejects_missing_index_or_score() {
        let json = resp(serde_json::json!([{ "relevance_score": 0.9 }]));
        assert!(parse_rerank_response(&json, 1).is_err());
        let json = resp(serde_json::json!([{ "index": 0 }]));
        assert!(parse_rerank_response(&json, 1).is_err());
    }

    #[test]
    fn rejects_more_results_than_documents() {
        let json = resp(serde_json::json!([
            { "index": 0, "relevance_score": 0.9 },
            { "index": 1, "relevance_score": 0.8 },
            { "index": 2, "relevance_score": 0.7 },
        ]));
        assert!(parse_rerank_response(&json, 2).is_err());
    }

    #[test]
    fn maybe_rerank_returns_none_when_not_applied() {
        // Empty results short-circuit regardless of COHERE_API_KEY, so the
        // caller knows local score adjustments were left intact.
        let mut results: Vec<SearchResult> = Vec::new();
        assert_eq!(maybe_rerank("query", &mut results, &[], 10), None);
    }

    #[test]
    fn rejects_incomplete_response() {
        let json = resp(serde_json::json!([
            { "index": 2, "relevance_score": 0.9 },
        ]));
        let err = parse_rerank_response(&json, 3).unwrap_err();
        assert!(err.to_string().contains("1 results for 3"), "{err}");
    }
}
