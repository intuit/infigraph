use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_stats(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let stats = prism.stats()?;
    println!("{}", stats);
    Ok(())
}

pub(crate) fn cmd_languages(project_root: Option<&Path>) -> Result<()> {
    let registry = crate::full_registry(project_root)?;
    println!("Available languages:");
    for pack in registry.languages() {
        let backend = match &pack.backend {
            infigraph_core::lang::ParserBackend::TreeSitter { .. } => "tree-sitter",
            infigraph_core::lang::ParserBackend::Custom(_) => "grammar-plugin",
        };
        println!(
            "  {} ({}) [{}]",
            pack.name,
            pack.extensions.join(", "),
            backend
        );
    }
    Ok(())
}

pub(crate) fn cmd_symbols(root: &Path, file: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbols = gq.symbols_in_file(file)?;
    if symbols.is_empty() {
        println!(
            "No symbols found for '{}'. Run 'infigraph index' first.",
            file
        );
        return Ok(());
    }

    println!("Symbols in {}:", file);
    for s in &symbols {
        println!(
            "  {:>8} {:30} L{}-{}",
            s.kind, s.name, s.start_line, s.end_line
        );
    }
    Ok(())
}

pub(crate) fn cmd_index_manifests(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let results = infigraph_core::manifest::index_manifests(root, store)?;
    if results.is_empty() {
        println!("No manifests found.");
        return Ok(());
    }
    let total: usize = results.iter().map(|r| r.deps.len()).sum();
    println!(
        "Indexed {} manifests, {} dependencies:\n",
        results.len(),
        total
    );
    for r in &results {
        println!(
            "  {} [{}]: {} deps",
            r.manifest_file,
            r.ecosystem,
            r.deps.len()
        );
    }
    Ok(())
}

pub(crate) fn cmd_dependencies(root: &Path, ecosystem: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let mut deps = infigraph_core::manifest::query_deps(store)?;
    if let Some(eco) = ecosystem {
        deps.retain(|d| d.ecosystem == eco);
    }
    if deps.is_empty() {
        println!("No dependencies found. Run 'infigraph index-manifests' first.");
        return Ok(());
    }
    println!("Dependencies ({}):\n", deps.len());
    let mut cur_eco = String::new();
    for d in &deps {
        if d.ecosystem != cur_eco {
            println!("  [{}]", d.ecosystem);
            cur_eco = d.ecosystem.clone();
        }
        let dev_tag = if d.is_dev { " (dev)" } else { "" };
        println!("    {}@{}{}", d.name, d.version, dev_tag);
    }
    Ok(())
}

pub(crate) fn cmd_api_surface(root: &Path, file_filter: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let mut syms = gq.get_api_surface()?;
    if let Some(f) = file_filter {
        syms.retain(|s| s.file.contains(f));
    }

    println!("API Surface ({} symbols):\n", syms.len());
    let mut cur_file = String::new();
    for s in &syms {
        if s.file != cur_file {
            println!("  {}", s.file);
            cur_file = s.file.clone();
        }
        println!("    [{:<10}] L{:<5} {}", s.kind, s.line, s.name);
    }
    Ok(())
}

pub(crate) fn cmd_file_deps(root: &Path, file: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let deps = gq.get_file_deps(file)?;
    println!("File dependencies for '{}':\n", file);
    println!("  Imports ({}):", deps.imports.len());
    for f in &deps.imports {
        println!("    → {}", f);
    }
    if deps.imports.is_empty() {
        println!("    (none)");
    }
    println!("\n  Imported by ({}):", deps.imported_by.len());
    for f in &deps.imported_by {
        println!("    ← {}", f);
    }
    if deps.imported_by.is_empty() {
        println!("    (none)");
    }
    Ok(())
}

pub(crate) fn cmd_type_hierarchy(root: &Path, symbol: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let hier = gq.get_type_hierarchy(symbol, depth)?;
    println!("Type hierarchy for '{}':\n", hier.root_name);
    println!("  Ancestors ({}):", hier.ancestors.len());
    for a in &hier.ancestors {
        println!("    ↑ {} [{}]  ({})", a.name, a.kind, a.file);
    }
    if hier.ancestors.is_empty() {
        println!("    (none — root type)");
    }
    println!("\n  Descendants ({}):", hier.descendants.len());
    for d in &hier.descendants {
        println!("    ↓ {} [{}]  ({})", d.name, d.kind, d.file);
    }
    if hier.descendants.is_empty() {
        println!("    (none — leaf type)");
    }
    Ok(())
}

pub(crate) fn cmd_test_coverage(root: &Path, file_filter: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let mut cov = gq.get_test_coverage()?;
    if let Some(f) = file_filter {
        cov.covered.retain(|s| s.file.contains(f));
        cov.uncovered.retain(|s| s.file.contains(f));
        let total = cov.covered.len() + cov.uncovered.len();
        cov.coverage_pct = (cov.covered.len() * 100).checked_div(total).unwrap_or(0);
        cov.covered_count = cov.covered.len();
        cov.uncovered_count = cov.uncovered.len();
    }

    println!(
        "Test Coverage: {}%  ({} covered / {} uncovered)\n",
        cov.coverage_pct, cov.covered_count, cov.uncovered_count
    );

    if !cov.uncovered.is_empty() {
        println!("Uncovered ({}):", cov.uncovered.len());
        for s in cov.uncovered.iter().take(50) {
            println!("  ✗  {:<40} [{}]  {}", s.symbol_name, s.kind, s.file);
        }
        if cov.uncovered.len() > 50 {
            println!("  ... and {} more", cov.uncovered.len() - 50);
        }
    }
    Ok(())
}

pub(crate) fn cmd_watch(root: &Path, debounce: u64) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    println!(
        "Watching {} (debounce {}ms) — Ctrl-C to stop",
        root.display(),
        debounce
    );

    let (stop_tx, stop_rx) = std::sync::mpsc::channel();

    ctrlc::set_handler(move || {
        let _ = stop_tx.send(());
    })
    .ok();

    infigraph_core::watch::watch_project(&prism, debounce, stop_rx, |evt| {
        println!("[watch] {evt}");
    })?;

    println!("Watch stopped.");
    Ok(())
}

pub(crate) fn cmd_scip_import(root: &Path, index_path: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let abs_index = if index_path.is_absolute() {
        index_path.to_path_buf()
    } else {
        root.join(index_path)
    };

    println!("Importing SCIP index from {}", abs_index.display());
    let stats = infigraph_core::scip::import_scip_index(&abs_index, store)?;
    println!(
        "SCIP import complete:\n  files processed: {}\n  symbols added: {}\n  symbols enriched: {}\n  relations added: {}\n  references added: {}",
        stats.files_processed,
        stats.symbols_added,
        stats.symbols_enriched,
        stats.relations_added,
        stats.references_added,
    );
    Ok(())
}

pub(crate) fn cmd_index_docs(root: &Path) -> Result<()> {
    let start = std::time::Instant::now();
    let mut idx = infigraph_docs::DocIndex::open(root)?;
    idx.init()?;
    let result = idx.index()?;
    let elapsed = start.elapsed();
    println!(
        "Document indexing complete in {:.1}s\n  Files scanned: {}\n  Files indexed: {}\n  Chunks created: {}",
        elapsed.as_secs_f64(), result.total_files, result.indexed_files, result.total_chunks
    );
    if let Some(store) = idx.store() {
        let stats = store.stats()?;
        println!(
            "  Total documents in store: {}\n  Total chunks in store: {}",
            stats.document_count, stats.chunk_count
        );
    }
    Ok(())
}

pub(crate) fn cmd_reindex_docs(root: &Path) -> Result<()> {
    let start = std::time::Instant::now();
    let mut idx = infigraph_docs::DocIndex::open(root)?;
    let result = idx.reindex()?;
    let elapsed = start.elapsed();
    println!(
        "Document full reindex complete in {:.1}s\n  Files scanned: {}\n  Files indexed: {}\n  Chunks created: {}",
        elapsed.as_secs_f64(), result.total_files, result.indexed_files, result.total_chunks
    );
    Ok(())
}

pub(crate) fn cmd_clean_docs(root: &Path) -> Result<()> {
    let mut idx = infigraph_docs::DocIndex::open(root)?;
    idx.clean()?;
    println!("Document index cleaned.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_index_confluence(
    root: &Path,
    base_url: &str,
    space: &str,
    page_ids: Option<Vec<String>>,
    pat: Option<String>,
    email: Option<String>,
    api_token: Option<String>,
    follow_links: bool,
    follow_depth: usize,
    max_pages: usize,
) -> Result<()> {
    let client = if let Some(pat) = pat {
        infigraph_confluence::ConfluenceClient::new(base_url, &pat)
    } else if let (Some(email), Some(token)) = (email, api_token) {
        infigraph_confluence::ConfluenceClient::new_basic(base_url, &email, &token)
    } else {
        anyhow::bail!("Provide either --pat or both --email and --api-token for authentication");
    };

    let crawl = if follow_links {
        infigraph_confluence::CrawlOptions {
            follow_links: true,
            follow_depth,
            max_pages,
            same_space_only: true,
        }
    } else {
        infigraph_confluence::CrawlOptions::no_follow()
    };

    let start = std::time::Instant::now();
    let sync = infigraph_confluence::ConfluenceSync::new(client, space);

    let mut idx = infigraph_docs::DocIndex::open(root)?;
    idx.init()?;
    let store = idx.store().context("DocStore not initialized")?;

    let ids = page_ids.as_deref();
    let result = sync.sync_with_options(store, root, ids, &crawl)?;
    let elapsed = start.elapsed();

    println!(
        "Confluence sync complete in {:.1}s\n  Pages fetched: {}\n  Pages indexed: {}\n  Pages deleted: {}\n  Chunks created: {}\n  Links created: {}",
        elapsed.as_secs_f64(),
        result.pages_fetched,
        result.pages_indexed,
        result.pages_deleted,
        result.chunks_created,
        result.links_created,
    );

    let stats = store.stats()?;
    println!(
        "  Total documents in store: {}\n  Total chunks in store: {}",
        stats.document_count, stats.chunk_count
    );
    Ok(())
}
