use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;
use infigraph_core::search::BM25Index;

use super::docs::{open_doc_index, tool_search_docs};
use super::helpers::find_containing_symbol;

type SearchData = (Vec<Vec<String>>, Vec<(String, Vec<f32>)>);

struct SearchContext {
    db_path: PathBuf,
    db_mtime: SystemTime,
    rows: Arc<Vec<Vec<String>>>,
    bm25_unfiltered: Arc<BM25Index>,
    docs_unfiltered: Arc<Vec<(String, String)>>,
    symbol_embeddings: Arc<Vec<(String, Vec<f32>)>>,
}

static SEARCH_CTX: OnceLock<Mutex<Option<SearchContext>>> = OnceLock::new();

fn search_ctx_lock() -> &'static Mutex<Option<SearchContext>> {
    SEARCH_CTX.get_or_init(|| Mutex::new(None))
}

fn build_docs_from_rows(rows: &[Vec<String>]) -> Vec<(String, String)> {
    rows.iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            (id, text)
        })
        .collect()
}

struct CachedSearchData {
    rows: Arc<Vec<Vec<String>>>,
    bm25: Arc<BM25Index>,
    docs: Arc<Vec<(String, String)>>,
    symbol_embeddings: Arc<Vec<(String, Vec<f32>)>>,
}

fn is_remote_mode() -> bool {
    #[cfg(feature = "remote")]
    {
        std::env::var("INFIGRAPH_BACKEND")
            .map(|v| v == "neo4j")
            .unwrap_or(false)
    }
    #[cfg(not(feature = "remote"))]
    {
        false
    }
}

fn remote_cache_key() -> SystemTime {
    #[cfg(feature = "remote")]
    {
        use infigraph_core::meta::PostgresMetaStore;
        if let Ok(pg) = PostgresMetaStore::connect_from_env_cached() {
            let count = pg.embedding_count("symbol").unwrap_or(0) as u64;
            return std::time::UNIX_EPOCH + std::time::Duration::from_secs(count);
        }
    }
    std::time::UNIX_EPOCH
}

fn get_or_build_search_ctx(args: &Value) -> Result<CachedSearchData> {
    let raw_path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let path = super::helpers::resolve_project_path(raw_path);
    let tg_root = PathBuf::from(&path).join(".infigraph");
    let canon = tg_root.canonicalize().unwrap_or_else(|_| tg_root.clone());

    let is_remote = is_remote_mode();

    let mtime = if is_remote {
        remote_cache_key()
    } else {
        let emb_file = tg_root.join("embeddings.bin");
        std::fs::metadata(&emb_file)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    };

    {
        let guard = search_ctx_lock().lock().unwrap();
        if let Some(ctx) = guard.as_ref() {
            if ctx.db_path == canon && ctx.db_mtime == mtime {
                return Ok(CachedSearchData {
                    rows: Arc::clone(&ctx.rows),
                    bm25: Arc::clone(&ctx.bm25_unfiltered),
                    docs: Arc::clone(&ctx.docs_unfiltered),
                    symbol_embeddings: Arc::clone(&ctx.symbol_embeddings),
                });
            }
        }
    }

    let (rows, symbol_embeddings) = if is_remote {
        get_search_data_remote()?
    } else {
        get_search_data_local(args, &path)?
    };

    let docs = build_docs_from_rows(&rows);
    let bm25 = BM25Index::build(docs.clone());

    let rows = Arc::new(rows);
    let bm25 = Arc::new(bm25);
    let docs = Arc::new(docs);
    let symbol_embeddings = Arc::new(symbol_embeddings);

    let data = CachedSearchData {
        rows: Arc::clone(&rows),
        bm25: Arc::clone(&bm25),
        docs: Arc::clone(&docs),
        symbol_embeddings: Arc::clone(&symbol_embeddings),
    };

    let mut guard = search_ctx_lock().lock().unwrap();
    *guard = Some(SearchContext {
        db_path: canon,
        db_mtime: mtime,
        rows,
        bm25_unfiltered: bm25,
        docs_unfiltered: docs,
        symbol_embeddings,
    });

    Ok(data)
}

fn get_search_data_local(args: &Value, path: &str) -> Result<SearchData> {
    let prism = super::helpers::open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;
    let rows = backend.get_symbols_for_search()?;

    let docs = build_docs_from_rows(&rows);
    let embedder = embed::best_embedder();
    let emb_path = PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");
    let embeddings_map: HashMap<String, Vec<f32>> = if emb_path.exists() {
        embed::load_embeddings_cached(&emb_path)?
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
            embeddings_map
                .get(id)
                .cloned()
                .or_else(|| embedder.embed(text).ok())
                .map(|emb| (id.clone(), emb))
        })
        .collect();

    Ok((rows, symbol_embeddings))
}

#[allow(unused)]
fn get_search_data_remote() -> Result<SearchData> {
    #[cfg(feature = "remote")]
    {
        use infigraph_core::graph::{GraphBackend, Neo4jBackend};
        use infigraph_core::meta::PostgresMetaStore;

        let backend = Neo4jBackend::connect_from_env()?;
        let rows = backend.get_symbols_for_search()?;

        let pg = PostgresMetaStore::connect_from_env_cached()?;
        let symbol_embeddings = pg.all_embeddings("symbol")?;

        Ok((rows, symbol_embeddings))
    }
    #[cfg(not(feature = "remote"))]
    {
        anyhow::bail!("remote mode requires --features remote")
    }
}

pub fn tool_search(args: &Value) -> Result<String> {
    let scope = args.get("scope").and_then(|s| s.as_str()).unwrap_or("all");

    if scope == "docs" {
        return tool_search_docs(args);
    }

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
    let path = &super::helpers::resolve_project_path(
        args.get("path").and_then(|p| p.as_str()).unwrap_or("."),
    );
    let use_regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);

    let ctx = get_or_build_search_ctx(args)?;

    let rows = ctx.rows;
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

    let filtered_bm25;
    let filtered_docs;
    let (bm25_ref, docs_ref): (&BM25Index, &[(String, String)]) = if kind_filter.is_some() {
        filtered_docs = build_docs_from_rows(
            &filtered_rows
                .iter()
                .map(|r| (*r).clone())
                .collect::<Vec<_>>(),
        );
        filtered_bm25 = BM25Index::build(filtered_docs.clone());
        (&filtered_bm25, &filtered_docs)
    } else {
        (&ctx.bm25, &ctx.docs)
    };

    let embedder = embed::best_embedder();

    let filtered_symbol_embeddings;
    let symbol_embeddings_ref: &[(String, Vec<f32>)] = if kind_filter.is_some() {
        let ids: std::collections::HashSet<&str> =
            docs_ref.iter().map(|(id, _)| id.as_str()).collect();
        filtered_symbol_embeddings = ctx
            .symbol_embeddings
            .iter()
            .filter(|(id, _)| ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        &filtered_symbol_embeddings
    } else {
        &ctx.symbol_embeddings
    };

    // Compute raw scores once, blend with both alphas
    let oversample = limit * 2;
    let is_remote = is_remote_mode();
    let tg_dir = PathBuf::from(path).join(".infigraph");
    let hnsw_path = tg_dir.join("hnsw_index.usearch");
    let emb_path = tg_dir.join("embeddings.bin");
    let (hnsw_opt, emb_opt): (Option<&std::path::Path>, Option<&std::path::Path>) = if is_remote {
        (None, None)
    } else {
        (Some(hnsw_path.as_path()), Some(emb_path.as_path()))
    };
    let raw = infigraph_core::search::compute_raw_scores(
        query,
        bm25_ref,
        embedder.as_ref(),
        symbol_embeddings_ref,
        oversample,
        hnsw_opt,
        emb_opt,
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
            bm25_ref,
            embedder.as_ref(),
            symbol_embeddings_ref,
            esc_oversample,
            hnsw_opt,
            emb_opt,
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
                if let Ok(doc_results) = infigraph_docs::search::hybrid_doc_search(
                    query, doc_store, &root, doc_limit, 0.5,
                ) {
                    if !doc_results.is_empty() {
                        out.push_str("\n---\nDocument matches:\n");
                        for dr in &doc_results {
                            let heading = dr.heading.as_deref().unwrap_or("");
                            out.push_str(&format!(
                                "  [{}] {} (score: {:.2})\n",
                                dr.doc_file, heading, dr.score
                            ));
                            let snippet: String = dr.text.chars().take(200).collect();
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

    if !super::watch::is_watching(&root.to_string_lossy().replace('\\', "/")) {
        let lock_path = root.join(".infigraph").join("watch.lock");
        let cli_watching = {
            use fs2::FileExt;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&lock_path)
                .ok()
                .map(|f| {
                    let locked = f.try_lock_exclusive().is_err();
                    let _ = f.unlock();
                    locked
                })
                .unwrap_or(false)
        };
        if !cli_watching {
            if let Some(msg) = super::watch::auto_start_watch_opportunistic(path) {
                out.push_str(&format!("\n✓ Auto-started watcher: {msg}"));
            } else {
                out.push_str("\n⚠ No file watcher running — results may be stale. Run `infigraph watch` or re-index to refresh.");
            }
            super::docs::auto_start_doc_watch_opportunistic(path);
        }
    }

    Ok(out)
}

pub fn tool_search_symbols(args: &Value) -> Result<String> {
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
    let path = &super::helpers::resolve_project_path(
        args.get("path").and_then(|p| p.as_str()).unwrap_or("."),
    );

    let ctx = get_or_build_search_ctx(args)?;
    let rows = ctx.rows;

    if rows.is_empty() {
        return Ok("No symbols indexed. Run index_project first.".to_string());
    }

    let embedder = embed::best_embedder();

    let (hnsw_path, emb_path) = if is_remote_mode() {
        (None, None)
    } else {
        let tg_dir = PathBuf::from(path).join(".infigraph");
        (
            Some(tg_dir.join("hnsw_index.usearch")),
            Some(tg_dir.join("embeddings.bin")),
        )
    };
    let results = infigraph_core::search::hybrid_search(
        query,
        &ctx.bm25,
        embedder.as_ref(),
        &ctx.symbol_embeddings,
        limit,
        0.3,
        hnsw_path.as_deref(),
        emb_path.as_deref(),
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

pub fn tool_search_code(args: &Value) -> Result<String> {
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

pub fn tool_semantic_search(args: &Value) -> Result<String> {
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
    let kind_filter = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);
    let path = &super::helpers::resolve_project_path(
        args.get("path").and_then(|p| p.as_str()).unwrap_or("."),
    );

    let ctx = get_or_build_search_ctx(args)?;
    let rows = ctx.rows;

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

    let filtered_bm25_sem;
    let filtered_docs_sem;
    let (bm25_ref_sem, docs_ref_sem): (&BM25Index, &[(String, String)]) = if kind_filter.is_some() {
        filtered_docs_sem = build_docs_from_rows(
            &filtered_rows
                .iter()
                .map(|r| (*r).clone())
                .collect::<Vec<_>>(),
        );
        filtered_bm25_sem = BM25Index::build(filtered_docs_sem.clone());
        (&filtered_bm25_sem, &filtered_docs_sem)
    } else {
        (&ctx.bm25, &ctx.docs)
    };

    let embedder = embed::best_embedder();

    let filtered_sym_emb_sem;
    let sym_emb_ref_sem: &[(String, Vec<f32>)] = if kind_filter.is_some() {
        let ids: std::collections::HashSet<&str> =
            docs_ref_sem.iter().map(|(id, _)| id.as_str()).collect();
        filtered_sym_emb_sem = ctx
            .symbol_embeddings
            .iter()
            .filter(|(id, _)| ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        &filtered_sym_emb_sem
    } else {
        &ctx.symbol_embeddings
    };

    let tg_dir = PathBuf::from(path).join(".infigraph");
    let hnsw_path = tg_dir.join("hnsw_index.usearch");
    let emb_path = tg_dir.join("embeddings.bin");
    let (hnsw_opt, emb_opt): (Option<&std::path::Path>, Option<&std::path::Path>) =
        if is_remote_mode() {
            (None, None)
        } else {
            (Some(hnsw_path.as_path()), Some(emb_path.as_path()))
        };
    let results = infigraph_core::search::hybrid_search(
        query,
        bm25_ref_sem,
        embedder.as_ref(),
        sym_emb_ref_sem,
        limit,
        0.85,
        hnsw_opt,
        emb_opt,
    )?;

    let row_map: HashMap<&str, &Vec<String>> = filtered_rows
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
