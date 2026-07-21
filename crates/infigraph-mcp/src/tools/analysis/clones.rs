use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;

use super::super::helpers::open_prism;

pub fn tool_detect_clones(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let threshold = args
        .get("threshold")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.92) as f32;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let store_edges = args
        .get("store_edges")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let kinds_str = args
        .get("kinds")
        .and_then(|v| v.as_str())
        .unwrap_or("Function,Method");
    let kinds: Vec<&str> = kinds_str.split(',').map(str::trim).collect();

    let backend = prism.backend().context("not initialized")?;

    let syms = backend.symbols_with_docstring(Some(&kinds))?;

    if syms.len() < 2 {
        return Ok("Not enough symbols to compare. Run index_project first.".to_string());
    }

    let embedder = embed::best_embedder();
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let emb_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");

    let cached: std::collections::HashMap<String, Vec<f32>> = if emb_path.exists() {
        infigraph_core::embed::load_embeddings_cached(&emb_path)?
            .into_iter()
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    let symbol_vecs: Vec<(String, String, String, Vec<f32>)> = syms
        .iter()
        .map(|s| {
            let text = if !s.docstring.is_empty() {
                format!("{} {}: {}", s.kind, s.name, s.docstring)
            } else {
                format!("{} {}", s.kind, s.name)
            };
            let emb = cached
                .get(&s.id)
                .cloned()
                .unwrap_or_else(|| embedder.embed(&text).unwrap_or_default());
            (s.id.clone(), s.name.clone(), s.file.clone(), emb)
        })
        .filter(|(_, _, _, emb)| !emb.is_empty())
        .collect();

    let n = symbol_vecs.len();
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();

    for i in 0..n {
        for j in (i + 1)..n {
            if symbol_vecs[i].2 == symbol_vecs[j].2 {
                continue;
            }
            let sim =
                infigraph_core::embed::cosine_similarity(&symbol_vecs[i].3, &symbol_vecs[j].3);
            if sim >= threshold {
                pairs.push((sim, i, j));
            }
        }
    }

    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(limit);

    if pairs.is_empty() {
        return Ok(format!(
            "No clones found above threshold {:.2} across {} symbols ({}).",
            threshold, n, kinds_str
        ));
    }

    if store_edges && !pairs.is_empty() {
        for (score, i, j) in &pairs {
            let _ = backend.upsert_similar_edge(&symbol_vecs[*i].0, &symbol_vecs[*j].0, *score);
        }
    }

    let mut out = format!(
        "Clone detection: {} pairs found (threshold={:.2}, symbols={}, kinds={})\n\n",
        pairs.len(),
        threshold,
        n,
        kinds_str
    );

    for (score, i, j) in &pairs {
        let (id_a, name_a, file_a, _) = &symbol_vecs[*i];
        let (id_b, name_b, file_b, _) = &symbol_vecs[*j];
        out.push_str(&format!(
            "{:.3}  {} ({}) <-> {} ({})\n       {} vs {}\n",
            score, name_a, id_a, name_b, id_b, file_a, file_b
        ));
    }

    Ok(out)
}

pub fn tool_refactor(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let target = args.get("target").and_then(|v| v.as_str());
    let focus_str = args.get("focus").and_then(|v| v.as_str()).unwrap_or("all");
    let focus = infigraph_core::refactor::Focus::parse(focus_str);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let emb_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");
    let emb_ref = if emb_path.exists() {
        Some(emb_path.as_path())
    } else {
        None
    };

    let recs = infigraph_core::refactor::analyze(backend, emb_ref, target, focus, limit)?;
    Ok(infigraph_core::refactor::format_recommendations(
        &recs, target,
    ))
}
