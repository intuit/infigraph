use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;

use infigraph_core::embed::{
    build_hnsw_index, doc_embedder, invalidate_embeddings_cache, invalidate_hnsw_cache,
    load_embeddings, save_embeddings,
};

use crate::backend::DocBackend;
use crate::chunk::Chunk;

pub fn update_doc_embeddings(
    store: &dyn DocBackend,
    root: &Path,
    new_chunks: &[&Chunk],
    changed_files: &[&str],
) -> Result<usize> {
    let tg_dir = root.join(".infigraph");
    let emb_path = tg_dir.join("docs_embeddings.bin");

    let mut existing: std::collections::HashMap<String, Vec<f32>> = load_embeddings(&emb_path)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let changed_set: std::collections::HashSet<&str> = changed_files.iter().copied().collect();

    // Remove embeddings for chunks belonging to changed files
    if !changed_set.is_empty() {
        existing.retain(|id, _| {
            let file = id.split("::chunk_").next().unwrap_or("");
            !changed_set.contains(file)
        });
    }

    // Embed new chunks with file path context for better semantic matching
    let to_embed: Vec<(&str, String)> = new_chunks
        .iter()
        .map(|c| {
            let file_context = doc_path_context(&c.doc_file);
            let text = match (&file_context, &c.heading) {
                (Some(ctx), Some(h)) => format!("{} > {}: {}", ctx, h, c.text),
                (Some(ctx), None) => format!("{}: {}", ctx, c.text),
                (None, Some(h)) => format!("{}: {}", h, c.text),
                (None, None) => c.text.clone(),
            };
            (c.id.as_str(), text)
        })
        .collect();

    if !to_embed.is_empty() {
        let embedder = doc_embedder();
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
                    .filter_map(|(i, (id, _))| vecs.get(i).map(|v| (id.to_string(), v.clone())))
                    .collect()
            })
            .collect();
        for batch in results {
            for (id, v) in batch {
                existing.insert(id, v);
            }
        }
    }

    // Also keep existing chunks that are still in the store
    let all_store_chunks = store.get_all_chunks().unwrap_or_default();
    let valid_ids: std::collections::HashSet<String> =
        all_store_chunks.into_iter().map(|(id, _)| id).collect();
    existing.retain(|id, _| valid_ids.contains(id));

    let embeddings: Vec<(String, Vec<f32>)> = existing.into_iter().collect();
    let count = embeddings.len();
    save_embeddings(&emb_path, &embeddings)?;

    // Build HNSW if above threshold or existing index
    const HNSW_THRESHOLD: usize = 200_000;
    let hnsw_path = tg_dir.join("docs_hnsw_index.usearch");
    if count >= HNSW_THRESHOLD || hnsw_path.exists() {
        invalidate_hnsw_cache();
        if let Err(e) = build_hnsw_index(&embeddings, &hnsw_path, &emb_path) {
            eprintln!(
                "warning: doc HNSW index build failed ({e}), vector search will use brute-force"
            );
        }
    }

    invalidate_embeddings_cache();
    Ok(count)
}

/// Store doc chunk embeddings in Postgres pgvector (remote mode).
/// Embeds new/changed chunks plus any store chunks missing from pgvector.
#[cfg(feature = "remote")]
pub fn update_doc_embeddings_remote(
    store: &dyn DocBackend,
    pg: &infigraph_core::meta::PostgresMetaStore,
    _new_chunks: &[&Chunk],
    changed_files: &[&str],
) -> Result<usize> {
    let existing_ids: std::collections::HashSet<String> = pg
        .all_embeddings("doc_chunk")
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let changed_set: std::collections::HashSet<&str> = changed_files.iter().copied().collect();

    // All chunks currently in the store (id, text)
    let all_store_chunks = store.get_all_chunks().unwrap_or_default();

    // Embed: (1) chunks from changed files, (2) chunks missing from pgvector
    let to_embed: Vec<(&str, &str)> = all_store_chunks
        .iter()
        .filter(|(id, _)| {
            let doc_file = id.split("::chunk_").next().unwrap_or("");
            if changed_set.contains(doc_file) {
                return true;
            }
            !existing_ids.contains(id.as_str())
        })
        .map(|(id, text)| (id.as_str(), text.as_str()))
        .collect();

    let mut all_embeddings: Vec<(String, Vec<f32>)> = Vec::new();

    if !to_embed.is_empty() {
        let embedder = doc_embedder();
        const BATCH: usize = 256;
        let results: Vec<Vec<(String, Vec<f32>)>> = to_embed
            .par_chunks(BATCH)
            .map(|chunk| {
                let emb = Arc::clone(&embedder);
                let texts: Vec<&str> = chunk.iter().map(|(_, t)| *t).collect();
                let vecs = emb.embed_batch(&texts).unwrap_or_default();
                chunk
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (id, _))| vecs.get(i).map(|v| (id.to_string(), v.clone())))
                    .collect()
            })
            .collect();
        all_embeddings = results.into_iter().flatten().collect();
    }

    let count = all_embeddings.len();
    if !all_embeddings.is_empty() {
        pg.upsert_embeddings_bulk(&all_embeddings, "doc_chunk")?;
    }

    // Clean up orphan doc_chunk embeddings
    let valid_ids: std::collections::HashSet<&str> =
        all_store_chunks.iter().map(|(id, _)| id.as_str()).collect();
    let orphans: Vec<String> = existing_ids
        .iter()
        .filter(|id| !valid_ids.contains(id.as_str()))
        .cloned()
        .collect();
    if !orphans.is_empty() {
        pg.delete_embeddings(&orphans)?;
    }

    Ok(count + valid_ids.len().saturating_sub(count))
}

fn doc_path_context(file: &str) -> Option<String> {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() <= 1 {
        return None;
    }
    let stem = parts
        .last()?
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(parts.last()?);
    let name = stem.replace(['_', '-'], " ");
    let dirs: Vec<&str> = parts[..parts.len() - 1]
        .iter()
        .filter(|p| {
            let lower = p.to_lowercase();
            !matches!(
                lower.as_str(),
                "src" | "doc" | "docs" | "documentation" | "resources"
            )
        })
        .copied()
        .collect();
    if dirs.is_empty() {
        Some(name)
    } else {
        let dir_path = dirs.join("/");
        Some(format!("{}/{}", dir_path, name))
    }
}
