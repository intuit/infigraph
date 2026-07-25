//! Remote embedder using the Voyage AI embeddings API.
//!
//! Local-development add-on: everything Voyage-specific lives in this file so
//! upstream `embed/mod.rs` only carries two one-line hooks in its factory
//! functions. Activated when `VOYAGE_API_KEY` is set. Uses `voyage-code-3` by
//! default (override with `INFIGRAPH_VOYAGE_MODEL`). Requests 256-dimensional
//! output (Matryoshka truncation) so vectors stay compatible with the local
//! embeddings.bin format, the HNSW index, and the Postgres `vector(256)` column.

use anyhow::{Context, Result};

use super::EmbedProvider;

pub struct VoyageEmbedder {
    agent: ureq::Agent,
    api_key: String,
    model: String,
    dim: usize,
}

impl VoyageEmbedder {
    /// Dimension pinned to 256 to match the pgvector schema (`vector(256)`).
    pub const OUTPUT_DIMENSION: usize = 256;
    const ENDPOINT: &'static str = "https://api.voyageai.com/v1/embeddings";
    /// Voyage allows up to 1000 inputs per request; stay well under token limits.
    const BATCH_SIZE: usize = 128;
    /// Bound each API request so a hung connection cannot stall indexing.
    const REQUEST_TIMEOUT_SECS: u64 = 60;

    /// Build from `VOYAGE_API_KEY`; returns None when the key is not set.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("VOYAGE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        let model =
            std::env::var("INFIGRAPH_VOYAGE_MODEL").unwrap_or_else(|_| "voyage-code-3".to_string());
        Some(Self {
            api_key,
            model,
            dim: Self::OUTPUT_DIMENSION,
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(Self::REQUEST_TIMEOUT_SECS))
                .build(),
        })
    }

    fn request_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
            "output_dimension": self.dim,
        });
        let resp = self
            .agent
            .post(Self::ENDPOINT)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("voyage embeddings request failed: {e}"))?;
        let json: serde_json::Value = resp
            .into_json()
            .context("voyage embeddings: invalid JSON response")?;
        parse_embeddings_response(&json, texts.len(), self.dim)
    }
}

/// Parse a Voyage embeddings API response into vectors ordered by `index`.
///
/// Strict about malformed responses: every item must carry a valid, unique,
/// in-range integer `index`, every embedding coordinate must be numeric, and
/// every output slot must be populated.
fn parse_embeddings_response(
    json: &serde_json::Value,
    expected: usize,
    dim: usize,
) -> Result<Vec<Vec<f32>>> {
    let data = json["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing 'data' in response"))?;
    anyhow::ensure!(
        data.len() == expected,
        "voyage embeddings: expected {} vectors, got {}",
        expected,
        data.len()
    );
    // Order by 'index' to be robust against out-of-order responses.
    let mut out: Vec<Option<Vec<f32>>> = vec![None; expected];
    for item in data {
        let idx = item["index"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing or invalid 'index'"))?
            as usize;
        anyhow::ensure!(
            idx < expected,
            "voyage embeddings: index {idx} out of range"
        );
        anyhow::ensure!(
            out[idx].is_none(),
            "voyage embeddings: duplicate index {idx}"
        );
        let arr = item["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing 'embedding'"))?;
        let mut emb = Vec::with_capacity(arr.len());
        for v in arr {
            let f = v
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("voyage embeddings: non-numeric embedding value"))?
                as f32;
            anyhow::ensure!(
                f.is_finite(),
                "voyage embeddings: non-finite embedding value"
            );
            emb.push(f);
        }
        anyhow::ensure!(
            emb.len() == dim,
            "voyage embeddings: expected dim {}, got {}",
            dim,
            emb.len()
        );
        out[idx] = Some(emb);
    }
    out.into_iter()
        .enumerate()
        .map(|(i, v)| {
            v.ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing vector for index {i}"))
        })
        .collect()
}

impl EmbedProvider for VoyageEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    fn identity(&self) -> String {
        format!("voyage:{}:{}", self.model, self.dim)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(Self::BATCH_SIZE) {
            all.extend(self.request_batch(chunk)?);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(items: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "data": items })
    }

    #[test]
    fn parses_out_of_order_response() {
        let json = resp(serde_json::json!([
            { "index": 1, "embedding": [3.0, 4.0] },
            { "index": 0, "embedding": [1.0, 2.0] },
        ]));
        let out = parse_embeddings_response(&json, 2, 2).unwrap();
        assert_eq!(out, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn rejects_missing_index() {
        let json = resp(serde_json::json!([
            { "embedding": [1.0, 2.0] },
            { "index": 1, "embedding": [3.0, 4.0] },
        ]));
        let err = parse_embeddings_response(&json, 2, 2).unwrap_err();
        assert!(err.to_string().contains("index"), "{err}");
    }

    #[test]
    fn rejects_duplicate_index() {
        let json = resp(serde_json::json!([
            { "index": 0, "embedding": [1.0, 2.0] },
            { "index": 0, "embedding": [3.0, 4.0] },
        ]));
        let err = parse_embeddings_response(&json, 2, 2).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_index() {
        let json = resp(serde_json::json!([
            { "index": 0, "embedding": [1.0, 2.0] },
            { "index": 5, "embedding": [3.0, 4.0] },
        ]));
        let err = parse_embeddings_response(&json, 2, 2).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
    }

    #[test]
    fn rejects_non_numeric_values() {
        let json = resp(serde_json::json!([
            { "index": 0, "embedding": [1.0, "oops"] },
        ]));
        let err = parse_embeddings_response(&json, 1, 2).unwrap_err();
        assert!(err.to_string().contains("non-numeric"), "{err}");
    }

    #[test]
    fn rejects_non_finite_values() {
        // 1e100 overflows f32 to +inf after narrowing.
        let json = resp(serde_json::json!([
            { "index": 0, "embedding": [1.0, 1e100] },
        ]));
        let err = parse_embeddings_response(&json, 1, 2).unwrap_err();
        assert!(err.to_string().contains("non-finite"), "{err}");
    }

    #[test]
    fn rejects_wrong_count_and_dim() {
        let json = resp(serde_json::json!([
            { "index": 0, "embedding": [1.0, 2.0] },
        ]));
        assert!(parse_embeddings_response(&json, 2, 2).is_err());
        assert!(parse_embeddings_response(&json, 1, 3).is_err());
    }
}
