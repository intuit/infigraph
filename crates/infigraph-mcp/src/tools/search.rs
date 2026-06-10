use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;

use super::docs::{open_doc_index, tool_search_docs};
use super::helpers::{find_containing_symbol, open_prism};

pub(crate) fn tool_search(args: &Value) -> Result<String> {
    let scope = args
        .get("scope")
        .and_then(|s| s.as_str())
        .unwrap_or("all");

    if scope == "docs" {
        return tool_search_docs(args);
    }

    let prism = open_prism(args)?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
    let kind_filter = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);
    let file_pattern = args.get("file_pattern").and_then(|f| f.as_str());
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let use_regex = args
        .get("regex")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.start_line, s.end_line",
    )?;

    if rows.is_empty() {
        return Ok("No symbols indexed. Run index_project first.".to_string());
    }

    let filtered_rows: Vec<&Vec<String>> = match &kind_filter {
        Some(k) => rows
            .iter()
            .filter(|row| row[2].to_lowercase() == *k)
            .collect(),
        None => rows.iter().collect(),
    };

    if filtered_rows.is_empty() {
        return Ok(format!(
            "No symbols found with kind '{}'.",
            kind_filter.unwrap_or_default()
        ));
    }

    let docs: Vec<(String, String)> = filtered_rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            (id, text)
        })
        .collect();

    let bm25_index = infigraph_core::search::BM25Index::build(docs.clone());
    let embedder = embed::best_embedder();
    let emb_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");
    let symbol_embeddings: Vec<(String, Vec<f32>)> = if emb_path.exists() {
        let all: std::collections::HashMap<String, Vec<f32>> =
            embed::load_embeddings_cached(&emb_path)?
                .into_iter()
                .collect();
        docs.iter()
            .filter_map(|(id, text)| {
                all.get(id)
                    .cloned()
                    .or_else(|| embedder.embed(text).ok())
                    .map(|emb| (id.clone(), emb))
            })
            .collect()
    } else {
        docs.iter()
            .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
            .collect()
    };

    // Compute raw scores once, blend with both alphas
    let oversample = limit * 2;
    let tg_dir = std::path::PathBuf::from(path).join(".infigraph");
    let hnsw_path = tg_dir.join("hnsw_index.usearch");
    let raw = infigraph_core::search::compute_raw_scores(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        oversample,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    let keyword_results = infigraph_core::search::combine_scores(&raw, 0.3, limit);
    let semantic_results = infigraph_core::search::combine_scores(&raw, 0.85, limit);

    // Merge: keep max score per symbol_id
    let mut merged: std::collections::HashMap<String, infigraph_core::search::SearchResult> =
        std::collections::HashMap::new();
    for r in keyword_results.into_iter().chain(semantic_results) {
        merged
            .entry(r.symbol_id.clone())
            .and_modify(|existing| {
                if r.score > existing.score {
                    *existing = r.clone();
                }
            })
            .or_insert(r);
    }

    // Run grep search
    let root = PathBuf::from(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));
    let grep_pattern = if use_regex {
        args.get("pattern")
            .and_then(|p| p.as_str())
            .unwrap_or(query)
            .to_string()
    } else {
        query
            .chars()
            .flat_map(|c| {
                if r"\.+*?()|[]{}^$-".contains(c) {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect::<String>()
    };
    let grep_results =
        infigraph_core::search::grep_search(&root, &grep_pattern, file_pattern, limit)
            .unwrap_or_default();

    // Build interval index for grep-to-symbol correlation
    let intervals: Vec<(&str, usize, usize, &str)> = rows
        .iter()
        .filter_map(|row| {
            let start: usize = row.get(5)?.parse().ok()?;
            let end: usize = row.get(6)?.parse().ok()?;
            Some((row[3].as_str(), start, end, row[0].as_str()))
        })
        .collect();

    // Correlate grep matches to symbols
    let mut grep_by_symbol: std::collections::HashMap<
        String,
        Vec<&infigraph_core::search::GrepMatch>,
    > = std::collections::HashMap::new();
    let mut grep_standalone: Vec<&infigraph_core::search::GrepMatch> = Vec::new();
    for gm in &grep_results {
        if let Some(sym_id) = find_containing_symbol(&intervals, &gm.file, gm.line_number) {
            if let Some(sr) = merged.get_mut(sym_id) {
                sr.score += 0.05;
            }
            grep_by_symbol
                .entry(sym_id.to_string())
                .or_default()
                .push(gm);
        } else {
            grep_standalone.push(gm);
        }
    }

    // Sort merged results
    let mut symbol_results: Vec<infigraph_core::search::SearchResult> =
        merged.into_values().collect();
    symbol_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Auto-escalate if results are weak
    let top_score = symbol_results.first().map(|r| r.score).unwrap_or(0.0);
    if (top_score < 0.4 || symbol_results.len() < 3) && limit < 100 {
        let esc_limit = (limit * 3).min(100);
        let esc_oversample = esc_limit * 2;
        let raw2 = infigraph_core::search::compute_raw_scores(
            query,
            &bm25_index,
            embedder.as_ref(),
            &symbol_embeddings,
            esc_oversample,
            Some(&hnsw_path),
            Some(&emb_path),
        )?;
        let kw2 = infigraph_core::search::combine_scores(&raw2, 0.3, esc_limit);
        let sem2 = infigraph_core::search::combine_scores(&raw2, 0.85, esc_limit);

        let mut esc_merged: std::collections::HashMap<
            String,
            infigraph_core::search::SearchResult,
        > = symbol_results
            .into_iter()
            .map(|r| (r.symbol_id.clone(), r))
            .collect();
        for r in kw2.into_iter().chain(sem2) {
            esc_merged
                .entry(r.symbol_id.clone())
                .and_modify(|existing| {
                    if r.score > existing.score {
                        *existing = r.clone();
                    }
                })
                .or_insert(r);
        }
        symbol_results = esc_merged.into_values().collect();
        symbol_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    symbol_results.truncate(limit);

    // Build row lookup
    let row_map: std::collections::HashMap<&str, &Vec<String>> =
        rows.iter().map(|row| (row[0].as_str(), row)).collect();

    // Format output
    let mut out = format!(
        "Search: '{}' ({} symbol results, {} text matches)\n\n",
        query,
        symbol_results.len(),
        grep_standalone.len()
    );

    for r in &symbol_results {
        if let Some(row) = row_map.get(r.symbol_id.as_str()) {
            let lines = match (
                row.get(5).filter(|s| !s.is_empty()),
                row.get(6).filter(|s| !s.is_empty()),
            ) {
                (Some(s), Some(e)) => format!(":L{}-{}", s, e),
                (Some(s), None) => format!(":L{}", s),
                _ => String::new(),
            };
            out.push_str(&format!(
                "{:.3}  {} {} ({}{})\n",
                r.score, row[2], row[1], row[3], lines
            ));
            if let Some(doc) = row.get(4).filter(|s| !s.is_empty()) {
                let preview: String = doc.chars().take(120).collect();
                out.push_str(&format!("       \"{}\"\n", preview));
            }
            if let Some(gms) = grep_by_symbol.get(r.symbol_id.as_str()) {
                for gm in gms.iter().take(3) {
                    out.push_str(&format!(
                        "       grep: {}:{}: {}\n",
                        gm.file,
                        gm.line_number,
                        gm.line_text.trim()
                    ));
                }
            }
        }
    }

    if !grep_standalone.is_empty() {
        out.push_str("\n---\nText matches:\n");
        for gm in grep_standalone.iter().take(limit) {
            out.push_str(&format!(
                "{}:{}: {}\n",
                gm.file,
                gm.line_number,
                gm.line_text.trim()
            ));
        }
    }

    // scope="all": append document results
    if scope == "all" {
        if let Ok(doc_idx) = open_doc_index(args) {
            if let Some(doc_store) = doc_idx.store() {
                let doc_limit = (limit / 2).max(5);
                if let Ok(doc_results) =
                    infigraph_docs::search::hybrid_doc_search(
                        query,
                        doc_store,
                        &root,
                        doc_limit,
                        0.5,
                    )
                {
                    if !doc_results.is_empty() {
                        out.push_str("\n---\nDocument matches:\n");
                        for dr in &doc_results {
                            let heading = dr.heading.as_deref().unwrap_or("");
                            out.push_str(&format!(
                                "  [{}] {} (score: {:.2})\n",
                                dr.doc_file, heading, dr.score
                            ));
                            let snippet: String =
                                dr.text.chars().take(200).collect();
                            if !snippet.is_empty() {
                                out.push_str(&format!("    {}\n", snippet));
                            }
                        }
                    }
                }
            }
        }
    }

    if out.ends_with("\n\n") {
        out.push_str(&format!("No results for '{}'", query));
    }

    Ok(out)
}

pub(crate) fn tool_search_symbols(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.start_line, s.end_line",
    )?;

    if rows.is_empty() {
        return Ok("No symbols indexed. Run index_project first.".to_string());
    }

    let docs: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            (id, text)
        })
        .collect();

    let bm25_index = infigraph_core::search::BM25Index::build(docs.clone());
    let embedder = embed::best_embedder();
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let emb_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");
    let symbol_embeddings: Vec<(String, Vec<f32>)> = if emb_path.exists() {
        embed::load_embeddings_cached(&emb_path)?
    } else {
        docs.iter()
            .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
            .collect()
    };

    let hnsw_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("hnsw_index.usearch");
    let results = infigraph_core::search::hybrid_search(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        limit,
        0.3,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    let mut out = String::new();
    for r in &results {
        if let Some(row) = rows.iter().find(|row| row[0] == r.symbol_id) {
            let lines = match (
                row.get(5).filter(|s| !s.is_empty()),
                row.get(6).filter(|s| !s.is_empty()),
            ) {
                (Some(s), Some(e)) => format!(":L{}-{}", s, e),
                (Some(s), None) => format!(":L{}", s),
                _ => String::new(),
            };
            out.push_str(&format!(
                "{:.3}  {} {} ({}{})\n",
                r.score, row[2], row[1], row[3], lines
            ));
        }
    }
    if out.is_empty() {
        out = format!("No results for '{}'", query);
    }
    Ok(out)
}

pub(crate) fn tool_search_code(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let pattern = args
        .get("pattern")
        .and_then(|p| p.as_str())
        .context("missing 'pattern'")?;
    let file_pattern = args.get("file_pattern").and_then(|f| f.as_str());
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(50) as usize;

    let root = PathBuf::from(path).canonicalize().context("invalid path")?;

    let matches = infigraph_core::search::grep_search(&root, pattern, file_pattern, limit)?;

    if matches.is_empty() {
        return Ok(format!("No matches for '{}'", pattern));
    }

    let mut out = format!("{} match(es):\n", matches.len());
    for m in &matches {
        out.push_str(&format!("{}:{}: {}\n", m.file, m.line_number, m.line_text));
    }
    Ok(out)
}

pub(crate) fn tool_semantic_search(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
    let kind_filter = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.start_line",
    )?;

    if rows.is_empty() {
        return Ok("No symbols indexed. Run index_project first.".to_string());
    }

    // Apply kind filter before building index
    let filtered_rows: Vec<&Vec<String>> = match &kind_filter {
        Some(k) => rows
            .iter()
            .filter(|row| row[2].to_lowercase() == *k)
            .collect(),
        None => rows.iter().collect(),
    };

    if filtered_rows.is_empty() {
        return Ok(format!(
            "No symbols found with kind '{}'.",
            kind_filter.unwrap_or_default()
        ));
    }

    let docs: Vec<(String, String)> = filtered_rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            (id, text)
        })
        .collect();

    // Build BM25 index (used lightly at alpha=0.15 — mostly semantic)
    let bm25_index = infigraph_core::search::BM25Index::build(docs.clone());
    let embedder = embed::best_embedder();

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let emb_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");

    // Load or compute embeddings for filtered set
    let all_embeddings: std::collections::HashMap<String, Vec<f32>> = if emb_path.exists() {
        infigraph_core::embed::load_embeddings_cached(&emb_path)?
            .into_iter()
            .collect()
    } else {
        docs.iter()
            .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
            .collect()
    };

    let symbol_embeddings: Vec<(String, Vec<f32>)> = docs
        .iter()
        .filter_map(|(id, text)| {
            all_embeddings
                .get(id)
                .cloned()
                .or_else(|| embedder.embed(text).ok())
                .map(|emb| (id.clone(), emb))
        })
        .collect();

    // alpha=0.85: heavily vector-weighted for semantic meaning
    let hnsw_path = std::path::PathBuf::from(path)
        .join(".infigraph")
        .join("hnsw_index.usearch");
    let results = infigraph_core::search::hybrid_search(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        limit,
        0.85,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    let row_map: std::collections::HashMap<&str, &Vec<String>> = filtered_rows
        .iter()
        .map(|row| (row[0].as_str(), *row))
        .collect();

    let mut out = format!("Semantic search: '{}'\n\n", query);
    for r in &results {
        if let Some(row) = row_map.get(r.symbol_id.as_str()) {
            let line = row.get(5).map(|s| s.as_str()).unwrap_or("?");
            let doc = row
                .get(4)
                .filter(|s| !s.is_empty())
                .map(|s| format!("\n     {}", s.chars().take(120).collect::<String>()))
                .unwrap_or_default();
            out.push_str(&format!(
                "{:.3}  {} {} ({}:{}){}\n",
                r.score, row[2], row[1], row[3], line, doc
            ));
        }
    }
    if out.trim_end().ends_with('\'') {
        out.push_str("No results found.");
    }
    Ok(out)
}
