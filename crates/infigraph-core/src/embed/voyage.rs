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
        })
    }

    fn request_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
            "output_dimension": self.dim,
        });
        let resp = ureq::post(Self::ENDPOINT)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("voyage embeddings request failed: {e}"))?;
        let json: serde_json::Value = resp
            .into_json()
            .context("voyage embeddings: invalid JSON response")?;
        let data = json["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing 'data' in response"))?;
        anyhow::ensure!(
            data.len() == texts.len(),
            "voyage embeddings: expected {} vectors, got {}",
            texts.len(),
            data.len()
        );
        // Order by 'index' to be robust against out-of-order responses.
        let mut out = vec![Vec::new(); texts.len()];
        for item in data {
            let idx = item["index"].as_u64().unwrap_or(0) as usize;
            let emb: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("voyage embeddings: missing 'embedding'"))?
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            anyhow::ensure!(
                emb.len() == self.dim,
                "voyage embeddings: expected dim {}, got {}",
                self.dim,
                emb.len()
            );
            anyhow::ensure!(idx < out.len(), "voyage embeddings: index out of range");
            out[idx] = emb;
        }
        Ok(out)
    }
}

impl EmbedProvider for VoyageEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(Self::BATCH_SIZE) {
            all.extend(self.request_batch(chunk)?);
        }
        Ok(all)
    }
}
