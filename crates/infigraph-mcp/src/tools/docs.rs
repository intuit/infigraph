use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};

use anyhow::{Context, Result};
use serde_json::Value;

use super::helpers::{find_infigraph_cli, open_prism, open_prism_read_only};

// ---------------------------------------------------------------------------
// Document indexing helpers and statics
// ---------------------------------------------------------------------------

pub fn open_doc_index(args: &Value) -> Result<infigraph_docs::DocIndex> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let mut idx = infigraph_docs::DocIndex::open(std::path::Path::new(path))?;
    idx.init()?;
    Ok(idx)
}

pub struct DocWatcherEntry {
    pub stop_tx: mpsc::Sender<()>,
    pub path: String,
}

pub static DOC_WATCHERS: Mutex<Option<HashMap<String, DocWatcherEntry>>> = Mutex::new(None);

pub fn init_doc_watchers() {
    let mut guard = DOC_WATCHERS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
}

pub fn get_doc_watchers() -> std::sync::MutexGuard<'static, Option<HashMap<String, DocWatcherEntry>>>
{
    DOC_WATCHERS.lock().unwrap()
}

pub fn is_doc_watching(path: &str) -> bool {
    let guard = DOC_WATCHERS.lock().unwrap();
    guard
        .as_ref()
        .is_some_and(|map| map.values().any(|e| e.path == path))
}

pub fn auto_start_doc_watch(path: &str) -> Option<String> {
    auto_start_doc_watch_inner(path, false)
}

pub fn auto_start_doc_watch_opportunistic(path: &str) -> Option<String> {
    auto_start_doc_watch_inner(path, true)
}

fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

fn auto_start_doc_watch_inner(path: &str, skip_disabled_check: bool) -> Option<String> {
    if is_remote_mode() {
        return None;
    }
    if !skip_disabled_check && super::watch::watchers_disabled() {
        return None;
    }
    let root = std::path::PathBuf::from(path).canonicalize().ok()?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    if is_doc_watching(&root_str) {
        return None;
    }

    if !root.join(".infigraph").join("docs.kuzu").exists() {
        return None;
    }

    let args = serde_json::json!({
        "path": path,
        "debounce_ms": 500
    });
    match tool_watch_docs(&args) {
        Ok(msg) => {
            eprintln!("[auto-watch] Started doc watcher for {root_str}");
            Some(msg)
        }
        Err(e) => {
            eprintln!("[auto-watch] Failed to start doc watcher: {e}");
            None
        }
    }
}

pub fn tool_review(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let llm = args.get("llm").and_then(|v| v.as_bool()).unwrap_or(false);
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(1000) as usize;
    let context = args.get("context").and_then(|v| v.as_str());
    let group = args.get("group").and_then(|v| v.as_str());

    let backend = prism
        .backend()
        .context("not initialized -- run 'infigraph index' first")?;
    let registry = infigraph_languages::bundled_registry()?;
    let root = prism.root().to_path_buf();

    let report = if let Some(group_name) = group {
        let multi_reg = infigraph_core::multi::Registry::load()?;
        infigraph_core::review::review_with_group(
            &root,
            base_ref,
            limit,
            &registry,
            backend,
            group_name,
            &multi_reg,
            infigraph_languages::bundled_registry,
        )?
    } else {
        infigraph_core::review::review(&root, base_ref, limit, &registry, backend)?
    };

    if !llm && !dry_run {
        return Ok(infigraph_core::review::format_review(&report));
    }

    let (prompt, result) =
        infigraph_core::review::llm::review_with_llm(&root, &report, backend, dry_run, context)?;

    if dry_run {
        return Ok(prompt);
    }

    match result {
        Some(r) => {
            let mut out = infigraph_core::review::format_review(&report);
            out.push_str(&infigraph_core::review::llm::format_llm_review(&r));
            Ok(out)
        }
        None => Ok(infigraph_core::review::format_review(&report)),
    }
}

pub fn tool_index_docs(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;

    if let Some(cli) = find_infigraph_cli() {
        let output = std::process::Command::new(&cli)
            .args(["index-docs"])
            .current_dir(path)
            .output()
            .with_context(|| format!("Failed to run {}", cli.display()))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "infigraph index-docs failed:\n{}",
                combined
            ));
        }
        auto_start_doc_watch(path);
        return Ok(combined);
    }

    let idx = open_doc_index(args)?;
    let result = idx.index()?;

    let mut out = format!(
        "Document indexing complete.\n  Files scanned: {}\n  Files indexed: {}\n  Chunks created: {}\n",
        result.total_files, result.indexed_files, result.total_chunks
    );

    if let Some(store) = idx.store() {
        let stats = store.stats()?;
        out.push_str(&format!(
            "  Total documents in store: {}\n  Total chunks in store: {}\n",
            stats.document_count, stats.chunk_count
        ));
    }

    auto_start_doc_watch(path);
    Ok(out)
}

pub fn tool_search_docs(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;

    let idx = open_doc_index(args)?;
    let store = idx.store().context("doc store not initialized")?;
    let root = PathBuf::from(path);

    let results = infigraph_docs::search::hybrid_doc_search(query, store, &root, limit, 0.5)?;

    if results.is_empty() {
        return Ok(
            "No document results found. Run index_docs first to index documents.".to_string(),
        );
    }

    // Pre-read text files to compute line numbers from byte offsets
    let mut file_contents: HashMap<String, String> = HashMap::new();
    for r in &results {
        if !file_contents.contains_key(&r.doc_file) {
            let full_path = if std::path::Path::new(&r.doc_file).is_absolute() {
                PathBuf::from(&r.doc_file)
            } else {
                root.join(&r.doc_file)
            };
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                file_contents.insert(r.doc_file.clone(), content);
            }
        }
    }

    let mut out = format!(
        "Document search: '{}' ({} results)\n\n",
        query,
        results.len()
    );
    for r in &results {
        out.push_str(&format!("{:.3}  {}", r.score, r.doc_file));
        if let Some(h) = &r.heading {
            out.push_str(&format!(" > {}", h));
        }

        if let Some(content) = file_contents.get(&r.doc_file) {
            let safe_start = {
                let pos = r.start_offset.min(content.len());
                content.floor_char_boundary(pos)
            };
            let safe_end = {
                let pos = r.end_offset.min(content.len());
                content.floor_char_boundary(pos)
            };
            let start_line = content[..safe_start].matches('\n').count() + 1;
            let end_line = content[..safe_end].matches('\n').count() + 1;
            out.push_str(&format!("  (lines {}-{})", start_line, end_line));
        } else if let Some(page) = r.page {
            out.push_str(&format!("  (page {})", page));
        }

        out.push('\n');
        let preview: String = r.text.chars().take(200).collect();
        out.push_str(&format!("      {}\n\n", preview.replace('\n', " ")));
    }

    Ok(out)
}

pub fn tool_clean_docs(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;

    if let Some(cli) = find_infigraph_cli() {
        let output = std::process::Command::new(&cli)
            .args(["clean-docs"])
            .current_dir(path)
            .output()
            .with_context(|| format!("Failed to run {}", cli.display()))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "infigraph clean-docs failed:\n{}",
                combined
            ));
        }
        return Ok(combined);
    }

    let mut idx = infigraph_docs::DocIndex::open(&PathBuf::from(path))?;
    idx.clean()?;
    Ok(
        "Document index cleaned. Removed: docs.kuzu, docs_embeddings.bin, docs_hnsw_index."
            .to_string(),
    )
}

pub fn tool_reindex_docs(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;

    if let Some(cli) = find_infigraph_cli() {
        let output = std::process::Command::new(&cli)
            .args(["reindex-docs"])
            .current_dir(path)
            .output()
            .with_context(|| format!("Failed to run {}", cli.display()))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "infigraph reindex-docs failed:\n{}",
                combined
            ));
        }
        return Ok(combined);
    }

    let mut idx = infigraph_docs::DocIndex::open(&PathBuf::from(path))?;
    let result = idx.reindex()?;
    Ok(format!(
        "Document full reindex complete.\n  Files scanned: {}\n  Files indexed: {}\n  Chunks created: {}\n",
        result.total_files, result.indexed_files, result.total_chunks
    ))
}

pub fn tool_index_confluence(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let base_url = args
        .get("base_url")
        .and_then(|v| v.as_str())
        .context("missing 'base_url'")?;
    let space = args
        .get("space")
        .and_then(|v| v.as_str())
        .context("missing 'space'")?;
    let pat = args.get("pat").and_then(|v| v.as_str());
    let email = args.get("email").and_then(|v| v.as_str());
    let api_token = args.get("api_token").and_then(|v| v.as_str());

    let client = if let Some(pat) = pat {
        infigraph_confluence::ConfluenceClient::new(base_url, pat)
    } else if let (Some(email), Some(token)) = (email, api_token) {
        infigraph_confluence::ConfluenceClient::new_basic(base_url, email, token)
    } else {
        anyhow::bail!(
            "No Confluence auth configured. Options:\n\
             1. Set CONFLUENCE_PAT environment variable\n\
             2. Pass 'pat' parameter directly\n\
             3. Pass 'email' + 'api_token' for basic auth\n\
             4. Use the Atlassian MCP connector to fetch pages, then call index_confluence_pages with the content"
        );
    };

    let page_ids: Option<Vec<String>> = args.get("page_ids").and_then(|v| {
        if let Some(arr) = v.as_array() {
            Some(
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
            )
        } else {
            v.as_str().map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
        }
    });

    let follow = args
        .get("follow_links")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let crawl = if follow {
        let depth = args
            .get("follow_depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;
        let max = args
            .get("max_pages")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;
        infigraph_confluence::CrawlOptions {
            follow_links: true,
            follow_depth: depth,
            max_pages: max,
            same_space_only: true,
        }
    } else {
        infigraph_confluence::CrawlOptions::no_follow()
    };

    let sync = infigraph_confluence::ConfluenceSync::new(client, space);
    let root = PathBuf::from(path);

    let mut idx = infigraph_docs::DocIndex::open(&root)?;
    idx.init()?;
    let store = idx.store().context("DocStore not initialized")?;

    let ids = page_ids.as_deref();
    let result = sync.sync_with_options(store, &root, ids, &crawl)?;

    let stats = store.stats()?;
    Ok(format!(
        "Confluence sync complete.\n  Space: {space}\n  Pages fetched: {}\n  Pages indexed: {}\n  Pages deleted: {}\n  Chunks created: {}\n  Links created: {}\n  Total documents in store: {}\n  Total chunks in store: {}",
        result.pages_fetched, result.pages_indexed, result.pages_deleted, result.chunks_created,
        result.links_created, stats.document_count, stats.chunk_count
    ))
}

pub fn tool_index_confluence_pages(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let space = args
        .get("space")
        .and_then(|v| v.as_str())
        .context("missing 'space'")?;
    let pages = args
        .get("pages")
        .and_then(|v| v.as_array())
        .context("missing 'pages' array")?;

    if pages.is_empty() {
        return Ok("No pages provided.".to_string());
    }

    let root = PathBuf::from(path);
    let mut idx = infigraph_docs::DocIndex::open(&root)?;
    idx.init()?;
    let store = idx.store().context("DocStore not initialized")?;

    let source_id = format!("confluence::{}", space);
    store.upsert_source(&source_id, "confluence", "", space)?;

    let mut docs = Vec::new();
    let mut all_chunks = Vec::new();

    for page in pages {
        let page_id = page
            .get("page_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let title = page
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");
        let content = page.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }

        let file_id = format!("confluence://{}/{}", space, page_id);
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(content.as_bytes());
            format!("{:x}", h.finalize())
        };

        let doc = infigraph_docs::extract::ExtractedDoc {
            file: file_id.clone(),
            title: Some(title.to_string()),
            content_hash: hash.clone(),
            format: infigraph_docs::extract::DocFormat::Markdown,
            text: content.to_string(),
            page_count: Some(1),
        };

        let chunks = infigraph_docs::chunk::chunk_document(
            &doc,
            &file_id,
            &hash,
            infigraph_docs::chunk::ChunkStrategy::HeadingBounded,
        );
        all_chunks.extend(chunks);
        docs.push(doc);
    }

    let indexed = docs.len();
    let chunks_created = all_chunks.len();

    if !docs.is_empty() {
        let doc_refs: Vec<&infigraph_docs::extract::ExtractedDoc> = docs.iter().collect();
        let chunk_refs: Vec<&infigraph_docs::chunk::Chunk> = all_chunks.iter().collect();
        store.upsert_docs(&doc_refs, &chunk_refs)?;

        for doc in &docs {
            store.link_doc_to_source(&doc.file, &source_id)?;
        }
    }

    if !all_chunks.is_empty() {
        let chunk_refs: Vec<&infigraph_docs::chunk::Chunk> = all_chunks.iter().collect();
        let changed_files: Vec<&str> = docs.iter().map(|d| d.file.as_str()).collect();
        infigraph_docs::embed::update_doc_embeddings(store, &root, &chunk_refs, &changed_files)?;
    }

    if !docs.is_empty() {
        let all_doc_ids: std::collections::HashSet<String> = {
            let existing = store.get_doc_hashes().unwrap_or_default();
            existing.keys().cloned().collect()
        };
        for doc in &docs {
            infigraph_docs::links::extract_and_link_doc(store, doc, &all_doc_ids);
        }
    }

    let stats = store.stats()?;
    Ok(format!(
        "Confluence pages indexed.\n  Space: {space}\n  Pages indexed: {indexed}\n  Chunks created: {chunks_created}\n  Total documents in store: {}\n  Total chunks in store: {}",
        stats.document_count, stats.chunk_count
    ))
}

pub fn tool_watch_docs(args: &Value) -> Result<String> {
    if is_remote_mode() {
        return Ok(
            "Doc watching is not supported in remote mode (Neo4j backend). \
             Reindexing is triggered via webhooks instead."
                .to_string(),
        );
    }
    init_doc_watchers();

    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let debounce_ms = args
        .get("debounce_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(2000);

    let root = PathBuf::from(path).canonicalize().context("invalid path")?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let watcher_id = format!(
        "docwatch-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    {
        let mut guard = DOC_WATCHERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.insert(
                watcher_id.clone(),
                DocWatcherEntry {
                    stop_tx,
                    path: root_str.clone(),
                },
            );
        }
    }

    let watcher_id_clone = watcher_id.clone();
    let log_prefix = watcher_id[..16.min(watcher_id.len())].to_string();
    std::thread::spawn(move || {
        if let Err(e) = infigraph_docs::watch::watch_docs(&root, debounce_ms, stop_rx, &log_prefix)
        {
            eprintln!("[{log_prefix}] watcher error: {e}");
        }
        let mut guard = DOC_WATCHERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(&watcher_id_clone);
        }
    });

    Ok(format!(
        "Document watcher started.\nID: {watcher_id}\nPath: {root_str}\nDebounce: {debounce_ms}ms\nUse stop_watch_docs to stop."
    ))
}

pub fn tool_stop_watch_docs(args: &Value) -> Result<String> {
    let watcher_id = args
        .get("watcher_id")
        .and_then(|v| v.as_str())
        .context("missing 'watcher_id'")?;
    let mut guard = DOC_WATCHERS.lock().unwrap();
    if let Some(map) = guard.as_mut() {
        if let Some(entry) = map.remove(watcher_id) {
            let _ = entry.stop_tx.send(());
            return Ok(format!(
                "Document watcher {watcher_id} stopped (was watching: {}).",
                entry.path
            ));
        }
    }
    Ok(format!("No active document watcher with ID {watcher_id}"))
}

pub fn tool_index_manifests(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;
    let results = infigraph_core::manifest::index_manifests(prism.root(), backend)?;
    if results.is_empty() {
        return Ok(
            "No manifests found (package.json, Cargo.toml, go.mod, pom.xml, etc.)".to_string(),
        );
    }
    let total: usize = results.iter().map(|r| r.deps.len()).sum();
    let mut out = format!(
        "Indexed {} manifests, {} dependencies total:\n\n",
        results.len(),
        total
    );
    for r in &results {
        out.push_str(&format!(
            "  {} [{}]: {} deps\n",
            r.manifest_file,
            r.ecosystem,
            r.deps.len()
        ));
    }

    // Create LINKS_TO edges from manifests to indexed docs via doc_urls
    if let Ok(doc_idx) = open_doc_index(args) {
        if let Some(doc_store) = doc_idx.store() {
            let all_doc_ids: std::collections::HashSet<String> = doc_store
                .get_doc_hashes()
                .unwrap_or_default()
                .keys()
                .cloned()
                .collect();
            let mut linked = 0usize;
            for r in &results {
                if !r.doc_urls.is_empty() {
                    infigraph_docs::links::link_manifest_doc_urls(
                        doc_store,
                        &r.manifest_file,
                        &r.doc_urls,
                        &all_doc_ids,
                    );
                    linked += r.doc_urls.len();
                }
            }
            if linked > 0 {
                out.push_str(&format!("\nLinked {} manifest doc URLs\n", linked));
            }
        }
    }

    Ok(out)
}
