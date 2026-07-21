use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;
use serde_json::json;

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

    let backend = prism.backend().context("graph not initialized")?;
    let symbols = backend.symbols_in_file(file)?;
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

pub(crate) fn cmd_skeleton(root: &Path, file: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let result = backend.skeleton(file)?;
    print!("{}", result);
    Ok(())
}

pub(crate) fn cmd_ingest(
    root: &Path,
    schema_id: Option<&str>,
    data_file: Option<&str>,
    source_dir: Option<&str>,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let schemas = infigraph_core::structured::discover_schemas(root)?;

    if schemas.is_empty() {
        println!("No structured schemas found.");
        println!("Create .toml schema files in .infigraph/structured-schemas/ or ~/.infigraph/structured-schemas/");
        return Ok(());
    }

    let sid = match schema_id {
        Some(id) => id,
        None => {
            println!("Available schemas:\n");
            for (path, schema) in &schemas {
                println!(
                    "  {} — {} (table: {}, {} columns, {} edges)\n    Source: {}\n",
                    schema.schema.schema_id,
                    schema.schema.name,
                    schema.schema.node_table,
                    schema.schema.columns.len(),
                    schema.schema.edges.len(),
                    path.display(),
                );
            }
            return Ok(());
        }
    };

    let (_, schema) = schemas
        .iter()
        .find(|(_, s)| s.schema.schema_id == sid)
        .context(format!("schema '{}' not found", sid))?;

    let backend = prism.backend().context("graph not initialized")?;

    if let Some(dir) = source_dir {
        let result =
            backend.ingest_structured_directory(&schema.schema, std::path::Path::new(dir))?;
        println!(
            "Ingested directory '{}' using schema '{}': {} nodes, {} edges",
            dir, sid, result.nodes_created, result.edges_created
        );
    } else {
        let file =
            data_file.context("--data-file or --source required when --schema is specified")?;
        let result = backend.ingest_structured_file(&schema.schema, std::path::Path::new(file))?;
        println!(
            "Ingested '{}' using schema '{}': {} nodes, {} edges",
            file, sid, result.nodes_created, result.edges_created
        );
    }
    Ok(())
}

pub(crate) fn cmd_index_manifests(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let backend = prism.backend().context("graph not initialized")?;
    let results = infigraph_core::manifest::index_manifests(root, backend)?;
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

    // Create LINKS_TO edges from manifests to indexed docs via doc_urls
    if let Ok(mut doc_idx) = infigraph_docs::DocIndex::open(root) {
        if doc_idx.init().is_ok() {
            if let Some(doc_store) = doc_idx.store() {
                let all_doc_ids: std::collections::HashSet<String> = doc_store
                    .get_doc_hashes()
                    .unwrap_or_default()
                    .keys()
                    .cloned()
                    .collect();
                for r in &results {
                    if !r.doc_urls.is_empty() {
                        infigraph_docs::links::link_manifest_doc_urls(
                            doc_store,
                            &r.manifest_file,
                            &r.doc_urls,
                            &all_doc_ids,
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn cmd_dependencies(root: &Path, ecosystem: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let backend = prism.backend().context("graph not initialized")?;
    let mut deps = infigraph_core::manifest::query_deps(backend)?;
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
    let backend = prism.backend().context("graph not initialized")?;
    let mut syms = backend.get_api_surface()?;
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
    let backend = prism.backend().context("graph not initialized")?;
    let deps = backend.get_file_deps(file)?;
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
    let backend = prism.backend().context("graph not initialized")?;
    let hier = backend.get_type_hierarchy(symbol, depth)?;
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
    let backend = prism.backend().context("graph not initialized")?;
    let mut cov = backend.get_test_coverage()?;
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
    // Hold exclusive lock for lifetime — signals liveness to ensure_watcher_running.
    let lock_path = root.join(".infigraph").join("watch.lock");
    let _lock = acquire_watch_lock(&lock_path)?;

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

    infigraph_core::watch::watch_project(root, bundled_registry, debounce, stop_rx, |evt| {
        println!("[watch] {evt}");
    })?;

    println!("Watch stopped.");
    Ok(())
}

pub(crate) fn cmd_watch_stop(root: &Path) -> Result<()> {
    let sentinel = root.join(".infigraph").join("watch.stop");
    let lock_path = root.join(".infigraph").join("watch.lock");

    if !watcher_is_alive(&lock_path) {
        println!("No watcher running.");
        return Ok(());
    }

    std::fs::write(&sentinel, b"")?;
    println!("Stop signal sent. Watcher will exit within ~1 second.");
    Ok(())
}

pub(crate) fn cmd_watch_status(root: &Path) -> Result<()> {
    let lock_path = root.join(".infigraph").join("watch.lock");

    if watcher_is_alive(&lock_path) {
        println!("Watcher is running.");
    } else {
        println!("No watcher running.");
    }
    Ok(())
}

pub(crate) fn watcher_is_alive(lock_path: &Path) -> bool {
    use fs2::FileExt;
    let file = match std::fs::OpenOptions::new()
        .create(false)
        .write(true)
        .truncate(false)
        .open(lock_path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

pub(crate) fn acquire_watch_lock(lock_path: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another watcher is already running"))?;
    Ok(file)
}

pub(crate) fn cmd_scip_import(root: &Path, index_path: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let abs_index = if index_path.is_absolute() {
        index_path.to_path_buf()
    } else {
        root.join(index_path)
    };

    println!("Importing SCIP index from {}", abs_index.display());
    let stats = backend.import_scip_index(&abs_index, Some(root))?;
    println!(
        "SCIP import complete:\n  files processed: {}\n  symbols added: {}\n  symbols enriched: {}\n  relations added: {}\n  references added: {}\n  corrections learned: {}",
        stats.files_processed,
        stats.symbols_added,
        stats.symbols_enriched,
        stats.relations_added,
        stats.references_added,
        stats.corrections_learned,
    );
    Ok(())
}

pub(crate) fn cmd_index_docs(root: &Path) -> Result<()> {
    let start = std::time::Instant::now();
    let mut idx = infigraph_docs::DocIndex::open(root)?;

    #[cfg(feature = "remote")]
    let is_remote = std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false);
    #[cfg(not(feature = "remote"))]
    let is_remote = false;

    if is_remote {
        idx.set_skip_file_embeddings(true);
    }

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

    #[cfg(feature = "remote")]
    if is_remote {
        let pg = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached()?;
        pg.init_schema()?;
        let store = idx.store().context("doc store not initialized")?;
        let chunk_refs: Vec<&infigraph_docs::chunk::Chunk> = result.new_chunks.iter().collect();
        let changed_refs: Vec<&str> = result.changed_files.iter().map(|s| s.as_str()).collect();
        let count = infigraph_docs::embed::update_doc_embeddings_remote(
            store,
            &pg,
            &chunk_refs,
            &changed_refs,
        )?;
        if count > 0 {
            println!("Saved {} doc embeddings to Postgres pgvector", count);
        }
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

pub(crate) fn cmd_list_files(root: &Path, glob: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let rows = backend.raw_query("MATCH (s:Symbol) RETURN DISTINCT s.file ORDER BY s.file")?;

    if rows.is_empty() {
        println!("No files indexed. Run 'infigraph index' first.");
        return Ok(());
    }

    let glob_pat = glob.unwrap_or("");
    let mut files: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.first().map(|s| s.as_str()))
        .filter(|f| glob_pat.is_empty() || infigraph_mcp::tools::helpers::glob_matches(glob_pat, f))
        .collect();
    files.dedup();

    println!("{} source files:", files.len());
    for f in &files {
        println!("  {}", f);
    }
    Ok(())
}

pub(crate) fn cmd_generate_test_context(
    root: &Path,
    file: Option<&str>,
    limit: usize,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let ctx = backend.generate_test_context(file, limit, None)?;

    println!("Test Generation Context\n");
    println!("Framework: {}", ctx.framework);

    if let Some(ref ex) = ctx.example_test {
        println!("\nExample Test (style reference):");
        println!(
            "  {} — {}:{}-{}",
            ex.name, ex.file, ex.start_line, ex.end_line
        );
        let file_path = root.join(&ex.file);
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (ex.start_line as usize).saturating_sub(1);
            let end = (ex.end_line as usize).min(lines.len());
            if start < end {
                for (i, line) in lines[start..end].iter().enumerate() {
                    println!("  {:4}  {}", start + i + 1, line);
                }
            }
        }
    }

    println!(
        "\nTargets ({} uncovered symbols, priority-ranked):\n",
        ctx.targets.len()
    );

    for (i, t) in ctx.targets.iter().enumerate() {
        println!(
            "{}. {} [{}] — {}:{}-{} (priority: {})",
            i + 1,
            t.name,
            t.kind,
            t.file,
            t.start_line,
            t.end_line,
            t.priority_score
        );
        if !t.visibility.is_empty() {
            println!("   visibility: {}", t.visibility);
        }
        if !t.parameters.is_empty() {
            println!("   params: {}", t.parameters);
        }
        if !t.return_type.is_empty() {
            println!("   returns: {}", t.return_type);
        }
        if t.complexity > 1 {
            println!("   complexity: {}", t.complexity);
        }
        if !t.callers.is_empty() {
            println!("   callers: {}", t.callers.join(", "));
        }
        if !t.callees.is_empty() {
            println!("   callees: {}", t.callees.join(", "));
        }
        if !t.branches.is_empty() {
            println!("   branches ({}):", t.branches.len());
            for b in &t.branches {
                let indent = "   ".repeat(b.depth as usize + 2);
                if b.condition.is_empty() {
                    println!("{}L{}: {}", indent, b.line, b.kind);
                } else {
                    println!("{}L{}: {} ({})", indent, b.line, b.kind, b.condition);
                }
            }
        }

        let file_path = root.join(&t.file);
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (t.start_line as usize).saturating_sub(1);
            let end = (t.end_line as usize).min(lines.len());
            if start < end {
                for (i, line) in lines[start..end].iter().enumerate() {
                    println!("   {:4}  {}", start + i + 1, line);
                }
            }
        }
        println!();
    }

    Ok(())
}

pub(crate) fn cmd_delete_project(root: &Path) -> Result<()> {
    let project_path = PathBuf::from(root);

    // Stop watcher before removing data
    let lock_path = project_path.join(".infigraph").join("watch.lock");
    if watcher_is_alive(&lock_path) {
        let sentinel = project_path.join(".infigraph").join("watch.stop");
        let _ = std::fs::write(&sentinel, b"");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Remove the .infigraph directory within the project
    let infigraph_dir = project_path.join(".infigraph");
    if infigraph_dir.exists() {
        std::fs::remove_dir_all(&infigraph_dir).context("failed to remove .infigraph directory")?;
    }

    // Unregister from the global registry
    use infigraph_core::multi::Registry;
    let mut registry = Registry::load()?;
    let canonical = project_path.canonicalize().unwrap_or(project_path.clone());
    let to_remove: Vec<String> = registry
        .repos
        .iter()
        .filter(|(_, entry)| {
            entry.path == project_path
                || entry.path == canonical
                || entry
                    .path
                    .canonicalize()
                    .map(|p| p == canonical)
                    .unwrap_or(false)
        })
        .map(|(name, _)| name.clone())
        .collect();

    for name in &to_remove {
        registry.repos.remove(name);
    }
    registry.save()?;

    if to_remove.is_empty() {
        println!(
            "Removed .infigraph directory from {}. (Project was not in the global registry.)",
            root.display()
        );
    } else {
        println!(
            "Removed .infigraph directory and unregistered '{}' from global registry.",
            to_remove.join(", ")
        );
    }
    Ok(())
}

pub(crate) fn cmd_memory_context(
    root: &Path,
    query: &str,
    file: Option<&str>,
    depth: &str,
    sources: &str,
    limit: usize,
) -> Result<()> {
    let path = root.to_string_lossy();
    let mut args = json!({
        "path": path.as_ref(),
        "query": query,
        "depth": depth,
        "sources": sources,
        "limit": limit,
    });
    if let Some(f) = file {
        args["file"] = json!(f);
    }
    let result = infigraph_mcp::tools::memory_context::tool_memory_context(&args)?;
    println!("{result}");
    Ok(())
}

pub(crate) fn cmd_consolidate_memory(root: &Path, threshold: f64) -> Result<()> {
    let path = root.to_string_lossy();
    let args = json!({
        "path": path.as_ref(),
        "threshold": threshold,
    });
    let result = infigraph_mcp::tools::session::tool_consolidate_memory(&args)?;
    println!("{result}");
    Ok(())
}

pub(crate) fn cmd_purge_sessions(root: &Path, days: u32) -> Result<()> {
    let path = root.to_string_lossy();
    let args = json!({
        "path": path.as_ref(),
        "older_than_days": days,
    });
    let result = infigraph_mcp::tools::session::tool_purge_sessions(&args)?;
    println!("{result}");
    Ok(())
}
