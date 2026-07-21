use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;
use std::path::Path;

pub(crate) fn cmd_search(root: &Path, query: &str, limit: usize, alpha: f32) -> Result<()> {
    use infigraph_core::embed;

    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let rows =
        backend.raw_query("MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring")?;

    if rows.is_empty() {
        println!("No symbols found. Run 'infigraph index' first.");
        return Ok(());
    }

    let docs: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let name = &row[1];
            let kind = &row[2];
            let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
            let text = if doc.is_empty() {
                format!("{} {}", kind, name)
            } else {
                format!("{} {}: {}", kind, name, doc)
            };
            (id, text)
        })
        .collect();

    let emb_path = root.join(".infigraph").join("embeddings.bin");
    let bm25_cache_path = root.join(".infigraph").join("bm25_cache.bin");
    let emb_mtime = std::fs::metadata(&emb_path).and_then(|m| m.modified()).ok();
    let cache_mtime = std::fs::metadata(&bm25_cache_path)
        .and_then(|m| m.modified())
        .ok();
    let cache_fresh = match (emb_mtime, cache_mtime) {
        (Some(e), Some(c)) => c >= e,
        _ => false,
    };
    let bm25_index = if cache_fresh {
        match infigraph_core::search::BM25Index::load(&bm25_cache_path) {
            Ok(idx) => idx,
            Err(_) => {
                let idx = infigraph_core::search::BM25Index::build(docs.clone());
                let _ = idx.save(&bm25_cache_path);
                idx
            }
        }
    } else {
        let idx = infigraph_core::search::BM25Index::build(docs.clone());
        let _ = idx.save(&bm25_cache_path);
        idx
    };

    let embedder = embed::best_embedder();

    let symbol_embeddings: Vec<(String, Vec<f32>)> = if emb_path.exists() {
        embed::load_embeddings_cached(&emb_path)?
    } else {
        eprintln!("hint: run 'infigraph index' to cache embeddings for faster search");
        docs.iter()
            .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
            .collect()
    };

    let hnsw_path = root.join(".infigraph").join("hnsw_index.usearch");
    let results = infigraph_core::search::hybrid_search(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        limit,
        alpha,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    if results.is_empty() {
        println!("No results for '{}'", query);
        return Ok(());
    }

    println!("Results for '{}' (alpha={:.1}):", query, alpha);
    for r in &results {
        if let Some(row) = rows.iter().find(|row| row[0] == r.symbol_id) {
            println!(
                "  {:.3} (bm25:{:.2} vec:{:.2})  {:>8} {:30} {}",
                r.score, r.bm25_score, r.vector_score, row[2], row[1], row[3]
            );
            let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
            if !doc.is_empty() {
                let preview: String = doc.chars().take(80).collect();
                println!("         {}", preview);
            }
        }
    }

    Ok(())
}

pub(crate) fn cmd_search_code(
    root: &Path,
    pattern: &str,
    file_pattern: Option<&str>,
    limit: usize,
) -> Result<()> {
    let root = root.canonicalize().context("invalid project root")?;

    let matches = infigraph_core::search::grep_search(&root, pattern, file_pattern, limit)?;

    if matches.is_empty() {
        println!("No matches for '{}'", pattern);
        return Ok(());
    }

    println!("{} match(es):", matches.len());
    for m in &matches {
        println!("  {}:{}: {}", m.file, m.line_number, m.line_text);
    }

    Ok(())
}

pub(crate) fn cmd_snippet(root: &Path, symbol_id: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let detail = backend
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let file_path = prism.root().join(&detail.file);
    let snippet = infigraph_core::search::read_lines_from_file(
        &file_path,
        detail.start_line,
        detail.end_line,
    )?;

    println!(
        "// {} {} ({}:L{}-{})",
        detail.kind, detail.name, detail.file, detail.start_line, detail.end_line
    );
    println!("{}", snippet);

    Ok(())
}

pub(crate) fn cmd_find_refs(root: &Path, symbol: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let backend = prism.backend().context("graph not initialized")?;
    let refs = backend.find_all_references(symbol)?;
    if refs.is_empty() {
        println!("No references found for '{}'", symbol);
        return Ok(());
    }
    println!("References to '{}' ({} total):\n", symbol, refs.len());
    for r in &refs {
        println!("  {}:{:<6} in {}", r.file, r.line, r.caller_name);
    }
    Ok(())
}

pub(crate) fn cmd_search_docs(root: &Path, query: &str, limit: usize) -> Result<()> {
    let mut idx = infigraph_docs::DocIndex::open(root)?;
    idx.init()?;
    let store = idx.store().context("doc store not initialized")?;

    let results = infigraph_docs::search::hybrid_doc_search(query, store, root, limit, 0.5)?;
    if results.is_empty() {
        println!("No results. Run 'infigraph index-docs' first.");
        return Ok(());
    }
    for r in &results {
        let heading = r.heading.as_deref().unwrap_or("(no heading)");
        println!("{:.3}  {} > {}", r.score, r.doc_file, heading);
        let snippet: String = r.text.chars().take(120).collect();
        println!("      {}\n", snippet);
    }
    Ok(())
}
