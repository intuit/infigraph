use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use infigraph_core::embed;
use infigraph_core::graph::{SessionStore, SessionData};
use infigraph_core::multi::{self, Registry};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use std::sync::Arc;

struct WatcherEntry {
    stop_tx: mpsc::Sender<()>,
    path: String,
    /// Files that changed and have cross-file calls — need full reindex
    pending_reindex: Arc<Mutex<Vec<String>>>,
}

// Global registry of active watchers
static WATCHERS: Mutex<Option<HashMap<String, WatcherEntry>>> = Mutex::new(None);

fn get_watchers() -> std::sync::MutexGuard<'static, Option<HashMap<String, WatcherEntry>>> {
    WATCHERS.lock().unwrap()
}

fn init_watchers() {
    let mut guard = WATCHERS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
}

fn is_watching(path: &str) -> bool {
    let guard = WATCHERS.lock().unwrap();
    guard
        .as_ref()
        .is_some_and(|map| map.values().any(|e| e.path == path))
}

fn auto_start_watch(path: &str) -> Option<String> {
    let root = std::path::PathBuf::from(path).canonicalize().ok()?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    if is_watching(&root_str) {
        return None;
    }

    let args = serde_json::json!({
        "path": path,
        "auto_resolve": true,
        "debounce_ms": 500
    });
    match tool_watch_project(&args) {
        Ok(msg) => {
            eprintln!("[auto-watch] Started watcher for {root_str}");
            Some(msg)
        }
        Err(e) => {
            eprintln!("[auto-watch] Failed to start watcher: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Document indexing helpers and statics
// ---------------------------------------------------------------------------

fn open_doc_index(args: &Value) -> Result<infigraph_docs::DocIndex> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let mut idx = infigraph_docs::DocIndex::open(std::path::Path::new(path))?;
    idx.init()?;
    Ok(idx)
}

struct DocWatcherEntry {
    stop_tx: mpsc::Sender<()>,
    path: String,
}

static DOC_WATCHERS: Mutex<Option<HashMap<String, DocWatcherEntry>>> = Mutex::new(None);

fn init_doc_watchers() {
    let mut guard = DOC_WATCHERS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
}

// ---------------------------------------------------------------------------
// New tool implementations
// ---------------------------------------------------------------------------

fn tool_review(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let llm = args
        .get("llm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(1000) as usize;
    let context = args.get("context").and_then(|v| v.as_str());
    let group = args.get("group").and_then(|v| v.as_str());

    let store = prism
        .store()
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
            store,
            group_name,
            &multi_reg,
            infigraph_languages::bundled_registry,
        )?
    } else {
        infigraph_core::review::review(&root, base_ref, limit, &registry, store)?
    };

    if !llm && !dry_run {
        return Ok(infigraph_core::review::format_review(&report));
    }

    let (prompt, result) =
        infigraph_core::review::llm::review_with_llm(&root, &report, store, dry_run, context)?;

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

fn tool_index_docs(args: &Value) -> Result<String> {
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

    Ok(out)
}

fn tool_search_docs(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .unwrap_or(10) as usize;

    let idx = open_doc_index(args)?;
    let store = idx.store().context("doc store not initialized")?;
    let root = PathBuf::from(path);

    let results = infigraph_docs::search::hybrid_doc_search(query, store, &root, limit, 0.5)?;

    if results.is_empty() {
        return Ok("No document results found. Run index_docs first to index documents.".to_string());
    }

    // Pre-read text files to compute line numbers from byte offsets
    let mut file_contents: HashMap<String, String> = HashMap::new();
    for r in &results {
        if !file_contents.contains_key(&r.doc_file) {
            let full_path = root.join(&r.doc_file);
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

fn tool_clean_docs(args: &Value) -> Result<String> {
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

fn tool_reindex_docs(args: &Value) -> Result<String> {
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

fn tool_index_confluence(args: &Value) -> Result<String> {
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

fn tool_index_confluence_pages(args: &Value) -> Result<String> {
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
        let content = page
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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
        store.upsert_all_parquet(&doc_refs, &chunk_refs)?;

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

fn tool_watch_docs(args: &Value) -> Result<String> {
    init_doc_watchers();

    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let debounce_ms = args
        .get("debounce_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(2000);

    let root = PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;
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
        if let Err(e) =
            infigraph_docs::watch::watch_docs(&root, debounce_ms, stop_rx, &log_prefix)
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

fn tool_stop_watch_docs(args: &Value) -> Result<String> {
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

mod web;

fn main() -> Result<()> {
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global();

    // Check for --ui flag
    let args: Vec<String> = std::env::args().collect();
    let ui_enabled = args
        .iter()
        .any(|a| a == "--ui" || a.starts_with("--ui=") || a == "--mcp");
    let port: u16 = args
        .iter()
        .find(|a| a.starts_with("--port="))
        .and_then(|a| a.strip_prefix("--port="))
        .and_then(|p| p.parse().ok())
        .unwrap_or(9749);

    let mcp_mode = args.iter().any(|a| a == "--mcp");

    if ui_enabled {
        if web::start_ui_server(port) {
            eprintln!("Infigraph UI running at http://localhost:{}", port);
            eprintln!("Open: http://localhost:{}/?path=/your/project", port);
        } else {
            eprintln!(
                "Infigraph UI port {} already in use — skipping UI (MCP active)",
                port
            );
        }
        if !mcp_mode {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    }

    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": format!("Parse error: {e}") }
                    }),
                )?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = match method {
            "initialize" => handle_initialize(&id),
            "tools/list" => handle_tools_list(&id),
            "tools/call" => handle_tools_call(&id, &request),
            "notifications/initialized" | "notifications/cancelled" => continue,
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") }
            }),
        };

        write_response(&stdout, response)?;
    }

    // If UI mode is active, keep process alive after stdin EOF (web server still serving)
    if ui_enabled {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    Ok(())
}

fn write_response(stdout: &io::Stdout, response: Value) -> Result<()> {
    let msg = serde_json::to_string(&response)?;
    let mut out = stdout.lock();
    out.write_all(msg.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn handle_initialize(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "infigraph",
                "version": "0.1.0"
            }
        }
    })
}

fn tool_def(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required
        }
    })
}

fn p(path: bool, symbol: bool, file: bool, extra: Value) -> Value {
    let mut obj = serde_json::Map::new();
    if path {
        obj.insert(
            "path".into(),
            json!({"type":"string","description":"Project root path"}),
        );
    }
    if symbol {
        obj.insert(
            "symbol_id".into(),
            json!({"type":"string","description":"Symbol ID (e.g. 'auth.py::authenticate')"}),
        );
    }
    if file {
        obj.insert(
            "file".into(),
            json!({"type":"string","description":"Relative file path"}),
        );
    }
    if let Some(extra_obj) = extra.as_object() {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    Value::Object(obj)
}

fn build_tools_list() -> Vec<Value> {
    vec![
        tool_def("index_project", "REQUIRED FIRST STEP: Parse all source files and build the code knowledge graph. Must run before any other infigraph tool. Auto-indexes 60+ languages.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("search", "PRIMARY: Unified code search — finds symbols by name, meaning, or text pattern in one call. Runs keyword-hybrid (BM25+vector) AND semantic-hybrid AND regex grep together, merges and deduplicates results. Auto-escalates limits when results are weak. Use this INSTEAD OF grep/ripgrep/find and INSTEAD OF search_symbols/semantic_search/search_code for all code search.",
            p(true,false,false,json!({"query":{"type":"string","description":"Search query (symbol name, natural language, or text pattern)"},"limit":{"type":"integer","default":20},"kind":{"type":"string","description":"Optional: filter by symbol kind (Function, Method, Class, etc.)"},"file_pattern":{"type":"string","description":"Optional: glob to restrict text search (e.g. '*.py')"}})), &["path","query"]),
        tool_def("search_symbols", "Advanced: Find symbols by name with keyword-weighted hybrid search (alpha=0.3). Prefer the unified `search` tool for most use cases.",
            p(true,false,false,json!({"query":{"type":"string","description":"Search query"},"limit":{"type":"integer","default":10}})), &["path","query"]),
        tool_def("query_graph", "Advanced: Execute Cypher query against code knowledge graph. Use for complex cross-cutting queries not covered by other tools. Full Cypher support.",
            p(true,false,false,json!({"cypher":{"type":"string","description":"Cypher query string"}})), &["path","cypher"]),
        tool_def("get_symbols_in_file", "PRIMARY: List all symbols in a file. Use INSTEAD OF reading entire files to find what's defined. Returns functions, classes, methods, variables with line numbers.",
            p(true,false,true,json!({})), &["path","file"]),
        tool_def("get_stats", "Graph statistics: total symbols, modules, call edges, inheritance edges, contains edges.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("detect_dead_code", "PRIMARY: Find unreachable functions/methods with zero callers. Use INSTEAD OF manual analysis for dead code cleanup. Excludes entry points and test fixtures.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("trace_callers", "PRIMARY: Find all direct callers of a symbol. Use INSTEAD OF grep for 'who calls this function'. Returns caller symbol IDs, files, and line numbers.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("trace_callees", "PRIMARY: Find all symbols called by a given symbol. Use INSTEAD OF reading function body to find calls. Returns callee symbol IDs, files, and line numbers.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("transitive_impact", "PRIMARY: Find all symbols transitively affected by changes to a symbol. Use BEFORE any refactor to understand blast radius. Follows CALLS edges in reverse.",
            p(true,true,false,json!({"depth":{"type":"integer","default":5}})), &["path","symbol_id"]),
        tool_def("search_code", "Advanced: Regex text search across all project files. Supports file pattern filters. Prefer the unified `search` tool for most use cases.",
            p(true,false,false,json!({"pattern":{"type":"string"},"file_pattern":{"type":"string"},"limit":{"type":"integer","default":50}})), &["path","pattern"]),
        tool_def("get_code_snippet", "PRIMARY: Get source code for a symbol by ID. Use INSTEAD OF reading files to view function/class source. Returns exact source with context.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("get_architecture", "PRIMARY: Codebase architecture overview. Use FIRST when onboarding to a new project. Returns language breakdown, hotspot files, hub functions, entry points.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("detect_changes", "PRIMARY: Map git changes to affected symbols and blast radius. Use INSTEAD OF git diff + manual tracing. Shows exactly which functions changed and what depends on them.",
            p(true,false,false,json!({"base":{"type":"string","default":"HEAD"},"depth":{"type":"integer","default":3}})), &["path"]),
        tool_def("list_projects", "List all indexed projects from the global registry.",
            json!({}), &[]),
        tool_def("delete_project", "Remove a project's .infigraph directory and unregister from global registry.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("list_languages", "List all 60+ supported programming languages and their file extensions.",
            json!({}), &[]),
        tool_def("get_graph_schema", "Show graph schema: node types, edge types, counts, and property names.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("symbol_context", "PRIMARY: Complete context for a symbol in one call — callers, callees, parent scope, file, kind, docstring. Use BEFORE modifying any function to understand its role.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("group_list", "List all repo groups and their members.",
            json!({}), &[]),
        tool_def("group_create", "Create a new repo group for organizing related repos (e.g. microservices).",
            json!({"name":{"type":"string","description":"Group name"}}), &["name"]),
        tool_def("group_add", "Add a repository to a group.",
            json!({"group_name":{"type":"string"},"repo_name":{"type":"string"},"path":{"type":"string"}}), &["group_name","repo_name"]),
        tool_def("group_query", "Run a Cypher query across all repos in a group.",
            json!({"group_name":{"type":"string"},"cypher":{"type":"string"}}), &["group_name","cypher"]),
        tool_def("group_sync", "Extract HTTP contracts from all repos in a group.",
            json!({"group_name":{"type":"string"}}), &["group_name"]),
        tool_def("group_contracts", "List HTTP contracts discovered in a group.",
            json!({"group_name":{"type":"string"}}), &["group_name"]),
        tool_def("group_deps", "PRIMARY: Detect cross-service HTTP dependencies within a group. Scans code for URL strings and matches to known routes in other services.",
            json!({"group_name":{"type":"string"}}), &["group_name"]),
        tool_def("group_index", "PRIMARY: Index (or reindex) all repos in a group in one call. Use for batch indexing microservice repos.",
            json!({"group_name":{"type":"string"},"full":{"type":"boolean","default":false,"description":"Clean and rebuild from scratch"}}), &["group_name"]),
        tool_def("group_link", "Link cross-service HTTP dependencies as CALLS_SERVICE edges in each caller repo's graph. Run after group_sync + group_deps. Enables cross-repo call graph traversal.",
            json!({"group_name":{"type":"string"}}), &["group_name"]),
        tool_def("detect_clusters", "Louvain community detection on the call graph to discover functional modules.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("export_graph", "Export the code graph as cypher, graphml, or json.",
            p(true,false,false,json!({"format":{"type":"string","enum":["cypher","graphml","json"]}})), &["path","format"]),
        tool_def("visualize", "Generate interactive HTML graph visualization using vis.js.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("visualize_symbol", "Generate a focused HTML subgraph centered on one symbol. Traverses callers, callees, and inheritance up to `depth` hops. Root symbol highlighted in gold. Much faster than full visualize for large codebases.",
            p(true,true,false,json!({"depth":{"type":"integer","default":2,"description":"Hop depth from the symbol (2 = callers+callees of callers+callees)"}})), &["path","symbol_id"]),
        tool_def("detect_routes", "PRIMARY: Detect HTTP routes/endpoints. Use INSTEAD OF grep for route decorators. Supports Flask, FastAPI, Express, NestJS, Spring, Gin, Actix, etc.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("scip_import", "Import a SCIP index.scip to enrich the graph with compiler-grade symbols, spans, and relationships.",
            p(true,false,false,json!({"index":{"type":"string","default":"index.scip"}})), &["path"]),
        tool_def("index_manifests", "Parse package manifests (package.json, Cargo.toml, go.mod, pom.xml, requirements.txt, Gemfile, composer.json, pubspec.yaml, *.csproj) and store dependencies in the graph.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("get_dependencies", "PRIMARY: List external dependencies. Use INSTEAD OF reading package.json/Cargo.toml/go.mod manually. Filter by ecosystem (npm/cargo/pip/maven/gem/nuget/go/composer/pub).",
            p(true,false,false,json!({"ecosystem":{"type":"string"}})), &["path"]),
        tool_def("find_all_references", "PRIMARY: Find every location where a symbol is referenced. Use INSTEAD OF grep for rename/refactor safety. Returns file, line, and calling context.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("get_api_surface", "PRIMARY: Public API surface — all public symbols and HTTP routes in one call. Use INSTEAD OF reading every file to find public interfaces.",
            p(true,false,true,json!({})), &["path"]),
        tool_def("get_file_deps", "PRIMARY: File-level import graph. Use INSTEAD OF reading imports manually. Shows what this file imports and what imports it.",
            p(true,false,true,json!({})), &["path","file"]),
        tool_def("get_type_hierarchy", "PRIMARY: Full inheritance tree. Use INSTEAD OF grep for class hierarchy. Returns ancestors and descendants of a class/interface.",
            p(true,true,false,json!({"depth":{"type":"integer","default":5}})), &["path","symbol_id"]),
        tool_def("get_test_coverage", "PRIMARY: Test coverage analysis — covered %, uncovered symbols. Use to find untested code before writing tests.",
            p(true,false,true,json!({})), &["path"]),
        tool_def("get_complexity", "PRIMARY: Cyclomatic complexity metrics. Use to find complex/hard-to-maintain functions. Shows per-symbol scores, hotspots above threshold, and file averages.",
            p(true,false,false,json!({"threshold":{"type":"integer","default":10,"description":"Flag symbols at or above this complexity (default: 10)"},"file":{"type":"string","description":"Optional: filter to a specific file"}})), &["path"]),
        tool_def("detect_security_issues", "PRIMARY: Security vulnerability scan. Use INSTEAD OF manual grep for security patterns. Detects SQL injection, hardcoded secrets, eval/exec, path traversal, SSRF, XXE, weak crypto, command injection, XSS, open redirect. Returns file, line, severity, fix.",
            p(true,false,false,json!({"severity":{"type":"string","description":"Filter: CRITICAL, HIGH, MEDIUM, LOW (default: all)"},"category":{"type":"string","description":"Filter by category e.g. SqlInjection, HardcodedSecret, WeakCrypto"}})), &["path"]),
        tool_def("semantic_diff", "PRIMARY: Symbol-level diff between git refs. Use INSTEAD OF git diff for understanding what changed. Shows added/removed/moved/signature-changed symbols, not line noise.",
            p(true,false,false,json!({"old_ref":{"type":"string","default":"HEAD~1","description":"Old git ref (commit, branch, tag)"},"new_ref":{"type":"string","default":"HEAD","description":"New git ref (default: HEAD)"}})), &["path"]),
        tool_def("watch_project", "Start a background file watcher that auto-reindexes changed files. Returns immediately with a watcher ID. Detects when changed files have cross-file call edges and warns (or auto-resolves with auto_resolve=true) so call resolution stays accurate. Use get_watch_status to check for pending reindexes.",
            p(true,false,false,json!({"debounce_ms":{"type":"integer","default":500,"description":"Debounce interval in ms before reindexing a changed file"},"auto_resolve":{"type":"boolean","default":false,"description":"If true, automatically runs full index_project when cross-file call edges are affected by a change"}})), &["path"]),
        tool_def("stop_watch", "Stop a running file watcher started by watch_project.",
            p(false,false,false,json!({"watcher_id":{"type":"string","description":"Watcher ID returned by watch_project"}})), &["watcher_id"]),
        tool_def("get_watch_status", "Check the status of running watchers. Shows pending files that need a full reindex due to cross-file call edge changes. Omit watcher_id to list all watchers.",
            p(false,false,false,json!({"watcher_id":{"type":"string","description":"Specific watcher ID to check (optional — omit to list all)"}})), &[]),
        tool_def("detect_bridges", "PRIMARY: Find cross-language boundaries — FFI, JNI, cgo, gRPC, P/Invoke, ctypes, WASM, COM. Use to map how languages interact in polyglot projects.",
            p(true,false,false,json!({"kind":{"type":"string","description":"Filter by kind: FFI, JNI, CGO, GRPC, P_INVOKE, CTYPES, WASM, COM (default: all)"}})), &["path"]),
        tool_def("semantic_search", "Advanced: Find code by meaning using semantic-weighted hybrid search (alpha=0.85). Prefer the unified `search` tool for most use cases.",
            p(true,false,false,json!({"query":{"type":"string","description":"Natural language description of what you're looking for"},"limit":{"type":"integer","default":10},"kind":{"type":"string","description":"Optional: filter by symbol kind (Function, Method, Class, etc.)"}})), &["path","query"]),
        tool_def("get_doc_context", "PRIMARY: Full documentation context for a symbol — signature, docstring, source, callers, callees, file. One call replaces get_code_snippet + trace_callers + trace_callees. Use BEFORE modifying any function.",
            p(true,true,false,json!({})), &["path","symbol_id"]),
        tool_def("detect_clones", "PRIMARY: Find near-duplicate functions using vector similarity. Use to identify copy-paste code and refactoring opportunities. Stores SIMILAR_TO edges for later querying.",
            p(true,false,false,json!({"threshold":{"type":"number","default":0.92,"description":"Similarity threshold 0.0-1.0 (default: 0.92). Lower = more results but more false positives."},"limit":{"type":"integer","default":20,"description":"Max clone pairs to return"},"kinds":{"type":"string","default":"Function,Method","description":"Comma-separated symbol kinds to check (default: Function,Method)"},"store_edges":{"type":"boolean","default":true,"description":"Write SIMILAR_TO edges to graph for later querying"}})), &["path"]),
        tool_def("refactor", "PRIMARY: Analyze code for refactoring opportunities — file size, complexity hotspots, coupling (fan-in/fan-out), near-duplicate functions, dead code. Returns ranked recommendations with impact/effort scores. Use instead of manually running detect_clones + get_complexity + detect_dead_code separately.",
            p(true,false,false,json!({"target":{"type":"string","description":"File path or symbol name to analyze (default: whole project)"},"focus":{"type":"string","enum":["all","complexity","duplication","coupling","size"],"default":"all","description":"Focus area: all, complexity, duplication, coupling, size"},"limit":{"type":"integer","default":10,"description":"Max recommendations to return"}})), &["path"]),
        tool_def("git_summary", "PRIMARY: Symbol-level commit history. Use INSTEAD OF git log for understanding recent changes. Shows which functions were added/removed/modified per commit, not just file names.",
            p(true,false,false,json!({"n_commits":{"type":"integer","default":10,"description":"Number of recent commits to summarize (default: 10)"},"author":{"type":"string","description":"Optional: filter by author name/email"},"file":{"type":"string","description":"Optional: filter to a specific file path"}})), &["path"]),
        tool_def("list_files", "PRIMARY: List all source files in project. Use INSTEAD OF find/ls/glob for file discovery. Supports glob patterns (e.g. '*.rs', 'src/**').",
            p(true,false,false,json!({"glob":{"type":"string","description":"Optional glob pattern to filter files (e.g. '*.rs', 'src/**')"}})), &["path"]),
        tool_def("generate_sequence_diagram", "PRIMARY: Generate Mermaid sequence diagram from call graph. Use to visualize control flow through a function. Participants = files, messages = calls.",
            p(true,true,false,json!({"depth":{"type":"integer","default":3,"description":"Max call depth to traverse (default: 3)"}})), &["path","symbol_id"]),
        tool_def("save_session", "Save session context to a dedicated session DB for cross-session continuity. Stores Session node + semantic embedding. Multiple calls per day merge: summary/pending_tasks/constraints/assumptions/blockers overwrite, decisions append, files_touched union. Use `narrative` for full session story — written to .infigraph/sessions/session_YYYY-MM-DD.md and embedded for semantic search.",
            p(true,false,false,json!({
                "summary":{"type":"string","description":"Brief summary of what was accomplished this session"},
                "pending_tasks":{"type":"string","description":"Tasks remaining / next steps"},
                "decisions":{"type":"string","description":"Structured decisions: 'Goal: X. Decision: Y. Why: Z. Invalidates-if: W.' Use | to separate multiple decisions"},
                "files_touched":{"type":"string","description":"Comma-separated list of files modified"},
                "constraints":{"type":"string","description":"What was tried and failed: 'Tried: X. Failed because: Y. Do not retry unless: Z.'"},
                "assumptions":{"type":"string","description":"What current approach depends on: 'Assumes: X. If X changes: Y.'"},
                "blockers":{"type":"string","description":"Stuck items needing human input or external dependency"},
                "narrative":{"type":"string","description":"Full session story: what was explored, found, reasoned, decided, and why. Raw chronological dump. Appended to .infigraph/sessions/session_YYYY-MM-DD.md with timestamp. Use for rich context recovery in future sessions."}
            })), &["path","summary"]),
        tool_def("get_latest_session", "Retrieve recent session context from graph DB. Call at START of every new session to resume where you left off. Returns summary, pending tasks, decisions, files touched, and linked file details. Use limit>1 to see session history.",
            p(true,false,false,json!({"limit":{"type":"integer","default":1,"description":"Number of recent sessions to return (default: 1)"}})), &["path"]),
        tool_def("purge_sessions", "Delete sessions older than specified days. Use to clean up old session history.",
            p(true,false,false,json!({
                "older_than_days":{"type":"integer","default":30,"description":"Delete sessions older than this many days (default: 30)"}
            })), &["path"]),
        tool_def("search_sessions", "Semantic search across past sessions. Finds sessions by meaning, not just keywords. Returns matching sessions ranked by relevance with summaries and narrative file paths.",
            p(true,false,false,json!({
                "query":{"type":"string","description":"Natural language query to search sessions (e.g. 'authentication refactoring', 'VB6 grammar debugging')"},
                "limit":{"type":"integer","default":5,"description":"Max results to return (default: 5)"}
            })), &["path","query"]),
        tool_def("review", "PR review: auto-detects PR type and scope. Runs: semantic diff, blast radius, affected tests, API surface, security scan, complexity, dead code, clones. Set llm=true for LLM-augmented review.",
            json!({"path": {"type": "string"}, "base_ref": {"type": "string", "description": "Git ref (default HEAD~1)"}, "llm": {"type": "boolean"}, "dry_run": {"type": "boolean"}, "limit": {"type": "integer"}, "context": {"type": "string"}, "group": {"type": "string"}}), &["path"]),
        tool_def("index_docs", "Index documents (PDF, DOCX, PPTX, XLSX, Markdown, TXT, RST, HTML) into a document graph. Incremental — skips unchanged files.",
            json!({"path": {"type": "string"}}), &["path"]),
        tool_def("search_docs", "Search indexed documents by meaning or keywords. Returns matching chunks with file, heading, page, and text snippet.",
            json!({"path": {"type": "string"}, "query": {"type": "string"}, "limit": {"type": "integer"}}), &["path", "query"]),
        tool_def("clean_docs", "Delete document index, embeddings, and HNSW index.",
            json!({"path": {"type": "string"}}), &["path"]),
        tool_def("reindex_docs", "Force full document reindex from scratch.",
            json!({"path": {"type": "string"}}), &["path"]),
        tool_def("index_confluence", "Fetch and index Confluence pages into the document graph. Supports incremental sync. Requires PAT or email+api_token auth.",
            json!({"path": {"type": "string"}, "base_url": {"type": "string"}, "space": {"type": "string"}, "page_ids": {"type": "array", "items": {"type": "string"}}, "pat": {"type": "string"}, "email": {"type": "string"}, "api_token": {"type": "string"}, "follow_links": {"type": "boolean"}, "follow_depth": {"type": "integer"}, "max_pages": {"type": "integer"}}), &["path", "base_url", "space"]),
        tool_def("index_confluence_pages", "Index pre-fetched Confluence page content. Pass array of pages with page_id, title, content fields.",
            json!({"path": {"type": "string"}, "space": {"type": "string"}, "pages": {"type": "array", "items": {"type": "object", "properties": {"page_id": {"type": "string"}, "title": {"type": "string"}, "content": {"type": "string"}}}}}), &["path", "space", "pages"]),
        tool_def("watch_docs", "Start background watcher that auto-reindexes changed documents.",
            json!({"path": {"type": "string"}, "debounce_ms": {"type": "integer"}}), &["path"]),
        tool_def("stop_watch_docs", "Stop a running document file watcher.",
            json!({"watcher_id": {"type": "string"}}), &["watcher_id"]),
    ]
}

fn handle_tools_list(id: &Value) -> Value {
    let tools = build_tools_list();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools
        }
    })
}

fn handle_tools_call(id: &Value, request: &Value) -> Value {
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    log_activity(tool_name, &args);

    let result = match tool_name {
        "index_project" => tool_index_project(&args),
        "search" => tool_search(&args),
        "search_symbols" => tool_search_symbols(&args),
        "query_graph" => tool_query_graph(&args),
        "get_symbols_in_file" => tool_get_symbols_in_file(&args),
        "get_stats" => tool_get_stats(&args),
        "detect_dead_code" => tool_detect_dead_code(&args),
        "trace_callers" => tool_trace_callers(&args),
        "trace_callees" => tool_trace_callees(&args),
        "transitive_impact" => tool_transitive_impact(&args),
        "search_code" => tool_search_code(&args),
        "get_code_snippet" => tool_get_code_snippet(&args),
        "get_architecture" => tool_get_architecture(&args),
        "detect_changes" => tool_detect_changes(&args),
        "list_projects" => tool_list_projects(&args),
        "delete_project" => tool_delete_project(&args),
        "list_languages" => tool_list_languages(&args),
        "get_graph_schema" => tool_get_graph_schema(&args),
        "symbol_context" => tool_symbol_context(&args),
        "group_list" => tool_group_list(&args),
        "group_create" => tool_group_create(&args),
        "group_add" => tool_group_add(&args),
        "group_query" => tool_group_query(&args),
        "group_sync" => tool_group_sync(&args),
        "group_contracts" => tool_group_contracts(&args),
        "group_deps" => tool_group_deps(&args),
        "group_index" => tool_group_index(&args),
        "group_link" => tool_group_link(&args),
        "detect_clusters" => tool_detect_clusters(&args),
        "export_graph" => tool_export_graph(&args),
        "visualize" => tool_visualize(&args),
        "visualize_symbol" => tool_visualize_symbol(&args),
        "detect_routes" => tool_detect_routes(&args),
        "scip_import" => tool_scip_import(&args),
        "index_manifests" => tool_index_manifests(&args),
        "get_dependencies" => tool_get_dependencies(&args),
        "find_all_references" => tool_find_all_references(&args),
        "get_api_surface" => tool_get_api_surface(&args),
        "get_file_deps" => tool_get_file_deps(&args),
        "get_type_hierarchy" => tool_get_type_hierarchy(&args),
        "get_test_coverage" => tool_get_test_coverage(&args),
        "get_complexity" | "analyze_complexity" => tool_get_complexity(&args),
        "detect_security_issues" => tool_detect_security_issues(&args),
        "semantic_diff" => tool_semantic_diff(&args),
        "watch_project" => tool_watch_project(&args),
        "stop_watch" => tool_stop_watch(&args),
        "get_watch_status" => tool_get_watch_status(&args),
        "detect_bridges" => tool_detect_bridges(&args),
        "semantic_search" => tool_semantic_search(&args),
        "get_doc_context" => tool_get_doc_context(&args),
        "detect_clones" => tool_detect_clones(&args),
        "refactor" => tool_refactor(&args),
        "git_summary" => tool_git_summary(&args),
        "list_files" => tool_list_files(&args),
        "generate_sequence_diagram" => tool_generate_sequence_diagram(&args),
        "save_session" => tool_save_session(&args),
        "get_latest_session" => tool_get_latest_session(&args),
        "purge_sessions" => tool_purge_sessions(&args),
        "search_sessions" => tool_search_sessions(&args),
        "review" => tool_review(&args),
        "index_docs" => tool_index_docs(&args),
        "search_docs" => tool_search_docs(&args),
        "clean_docs" => tool_clean_docs(&args),
        "reindex_docs" => tool_reindex_docs(&args),
        "index_confluence" => tool_index_confluence(&args),
        "index_confluence_pages" => tool_index_confluence_pages(&args),
        "watch_docs" => tool_watch_docs(&args),
        "stop_watch_docs" => tool_stop_watch_docs(&args),
        _ => Err(anyhow::anyhow!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": content }]
            }
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("Error: {e}") }],
                "isError": true
            }
        }),
    }
}

fn open_prism(args: &Value) -> Result<Infigraph> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&PathBuf::from(path), registry)?;
    prism.init()?;
    Ok(prism)
}

fn open_session_store(args: &Value) -> Result<SessionStore> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    SessionStore::open(&PathBuf::from(path))
}

fn find_infigraph_cli() -> Option<std::path::PathBuf> {
    // Check same directory as this binary first
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent()?.join("infigraph");
        if sibling.exists() {
            return Some(sibling);
        }
    }
    // Fall back to PATH
    if let Ok(out) = std::process::Command::new("which")
        .arg("infigraph")
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}

fn tool_index_project(args: &Value) -> Result<String> {
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let full = args.get("full").and_then(|f| f.as_bool()).unwrap_or(false);

    if let Some(cli) = find_infigraph_cli() {
        let mut cmd = std::process::Command::new(&cli);
        cmd.arg("index").current_dir(path);
        if full {
            cmd.arg("--full");
        }

        let output = cmd
            .output()
            .with_context(|| format!("Failed to run {}", cli.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        if !output.status.success() {
            return Err(anyhow::anyhow!("infigraph index failed:\n{}", combined));
        }
        let mut out = combined;
        if let Some(msg) = auto_start_watch(path) {
            out.push_str(&format!("\n{}", msg));
        }
        return Ok(out);
    }

    // Fallback: run inline if CLI not found
    let prism = open_prism(args)?;
    let result = prism.index()?;

    let mut out = format!(
        "Indexed {}/{} files\n",
        result.indexed_files, result.total_files
    );
    let mut by_lang: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for ext in &result.extractions {
        let entry = by_lang.entry(&ext.language).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += ext.symbols.len();
    }
    for (lang, (files, symbols)) in &by_lang {
        out.push_str(&format!(
            "  {}: {} files, {} symbols\n",
            lang, files, symbols
        ));
    }
    if result.resolve_stats.total_calls > 0 {
        out.push_str(&format!("{}\n", result.resolve_stats));
    }
    if let Some(store) = prism.store() {
        let root = std::path::PathBuf::from(path);
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        match embed::update_embeddings(store, &root, &changed) {
            Ok(n) => out.push_str(&format!("Saved {} embeddings\n", n)),
            Err(e) => out.push_str(&format!("warning: embedding update failed: {e}\n")),
        }
    }
    let stats = prism.stats()?;
    out.push_str(&format!("\n{}", stats));
    if let Some(msg) = auto_start_watch(path) {
        out.push_str(&format!("\n{}", msg));
    }
    Ok(out)
}

fn find_containing_symbol<'a>(
    intervals: &'a [(&str, usize, usize, &str)],
    file: &str,
    line: usize,
) -> Option<&'a str> {
    intervals.iter().find_map(|(f, start, end, id)| {
        if *f == file && *start <= line && line <= *end {
            Some(*id)
        } else {
            None
        }
    })
}

fn tool_search(args: &Value) -> Result<String> {
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
    let grep_pattern = query
        .chars()
        .flat_map(|c| {
            if r"\.+*?()|[]{}^$-".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect::<String>();
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

    if out.ends_with("\n\n") {
        out.push_str(&format!("No results for '{}'", query));
    }

    Ok(out)
}

fn tool_search_symbols(args: &Value) -> Result<String> {
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

fn tool_query_graph(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let cypher = args
        .get("cypher")
        .and_then(|c| c.as_str())
        .context("missing 'cypher'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(cypher)?;
    let mut out = String::new();
    for row in &rows {
        out.push_str(&row.join(" | "));
        out.push('\n');
    }
    if out.is_empty() {
        out = "(no results)".to_string();
    }
    Ok(out)
}

fn tool_get_symbols_in_file(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let file = args
        .get("file")
        .and_then(|f| f.as_str())
        .context("missing 'file'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbols = gq.symbols_in_file(file)?;
    let mut out = String::new();
    for s in &symbols {
        out.push_str(&format!(
            "{:>8} {:30} L{}-{}\n",
            s.kind, s.name, s.start_line, s.end_line
        ));
    }
    if out.is_empty() {
        out = format!("No symbols found for '{}'. Run index_project first.", file);
    }
    Ok(out)
}

fn tool_get_stats(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let stats = prism.stats()?;
    Ok(format!("{}", stats))
}

fn tool_detect_dead_code(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN s.name, s.kind, s.file ORDER BY s.file, s.name",
    )?;

    let entry_points = ["main", "__init__", "setUp", "tearDown"];
    let dead: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| !entry_points.contains(&row[0].as_str()))
        .collect();

    if dead.is_empty() {
        return Ok("No dead code found.".to_string());
    }

    let mut out = format!("Potentially dead code ({} symbols):\n", dead.len());
    for row in &dead {
        out.push_str(&format!("  {} {} ({})\n", row[1], row[0], row[2]));
    }

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    match save_analysis(path, "dead_code", &out) {
        Ok(receipt) => Ok(receipt),
        Err(_) => Ok(out),
    }
}

fn tool_trace_callers(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let callers = gq.callers_of(symbol_id)?;
    if callers.is_empty() {
        return Ok(format!("No callers found for '{}'", symbol_id));
    }
    Ok(callers.join("\n"))
}

fn tool_trace_callees(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let callees = gq.callees_of(symbol_id)?;
    if callees.is_empty() {
        return Ok(format!("No callees found for '{}'", symbol_id));
    }
    Ok(callees.join("\n"))
}

fn tool_transitive_impact(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(5) as u32;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let impacted = gq.transitive_impact(symbol_id, depth)?;
    if impacted.is_empty() {
        return Ok(format!("No symbols affected by changes to '{}'", symbol_id));
    }

    let mut out = String::new();
    for row in &impacted {
        out.push_str(&format!("{} {} ({})\n", row.kind, row.name, row.file));
    }
    Ok(out)
}

fn tool_search_code(args: &Value) -> Result<String> {
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

fn tool_get_code_snippet(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let file_path = prism.root().join(&detail.file);
    let snippet = infigraph_core::search::read_lines_from_file(
        &file_path,
        detail.start_line,
        detail.end_line,
    )?;

    let mut out = format!(
        "// {} {} ({}:L{}-{})\n",
        detail.kind, detail.name, detail.file, detail.start_line, detail.end_line
    );
    out.push_str(&snippet);
    Ok(out)
}

fn tool_get_architecture(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    build_architecture_report(&gq)
}

fn build_architecture_report(gq: &infigraph_core::graph::GraphQuery) -> Result<String> {
    let mut out = String::new();

    // 1. Language breakdown
    out.push_str("=== Language Breakdown ===\n");
    let lang_rows =
        gq.raw_query("MATCH (m:Module) RETURN m.language, count(m) ORDER BY count(m) DESC")?;
    if lang_rows.is_empty() {
        out.push_str("  (no modules indexed)\n");
    } else {
        for row in &lang_rows {
            out.push_str(&format!("  {:>20}: {} files\n", row[0], row[1]));
        }
    }

    // 2. Total symbols by kind
    out.push_str("\n=== Symbols by Kind ===\n");
    let kind_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.kind, count(s) ORDER BY count(s) DESC")?;
    if kind_rows.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for row in &kind_rows {
            out.push_str(&format!("  {:>20}: {}\n", row[0], row[1]));
        }
    }

    // 3. Hotspots: files with most symbols
    out.push_str("\n=== Hotspot Files (most symbols) ===\n");
    let hotspot_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10")?;
    if hotspot_rows.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for (i, row) in hotspot_rows.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:60} {} symbols\n",
                i + 1,
                row[0],
                row[1]
            ));
        }
    }

    // 4. Hub functions: most-called
    out.push_str("\n=== Hub Functions (most callers) ===\n");
    let hub_rows = gq.raw_query(
        "MATCH ()-[r:CALLS]->(s:Symbol) RETURN s.name, s.file, count(r) AS calls ORDER BY calls DESC LIMIT 10",
    )?;
    if hub_rows.is_empty() {
        out.push_str("  (no call edges found)\n");
    } else {
        for (i, row) in hub_rows.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:30} {:40} {} callers\n",
                i + 1,
                row[0],
                row[1],
                row[2]
            ));
        }
    }

    // 5. Entry points: functions that call others but are not called themselves
    out.push_str("\n=== Entry Points (call others, never called) ===\n");
    let entry_rows = gq.raw_query(
        "MATCH (s:Symbol)-[:CALLS]->() WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN DISTINCT s.name, s.kind, s.file ORDER BY s.file, s.name LIMIT 20",
    )?;
    if entry_rows.is_empty() {
        out.push_str("  (none found)\n");
    } else {
        for row in &entry_rows {
            out.push_str(&format!("  {:>8} {:30} {}\n", row[1], row[0], row[2]));
        }
    }

    Ok(out)
}

fn tool_detect_changes(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let base = args.get("base").and_then(|b| b.as_str()).unwrap_or("HEAD");
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(3) as u32;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    build_detect_changes_report(prism.root(), &gq, base, depth)
}

/// Parse git diff output and map changed lines to symbols in the graph.
fn build_detect_changes_report(
    project_root: &std::path::Path,
    gq: &infigraph_core::graph::GraphQuery,
    base: &str,
    depth: u32,
) -> Result<String> {
    use std::collections::HashSet;

    // 1. Get changed files
    let name_output = std::process::Command::new("git")
        .args(["diff", "--name-only", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --name-only")?;

    if !name_output.status.success() {
        let stderr = String::from_utf8_lossy(&name_output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    let changed_files: Vec<String> = String::from_utf8_lossy(&name_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if changed_files.is_empty() {
        return Ok("No changes detected.".to_string());
    }

    // 2. Get unified diff with zero context to extract changed line ranges
    let diff_output = std::process::Command::new("git")
        .args(["diff", "--unified=0", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --unified=0")?;

    let diff_text = String::from_utf8_lossy(&diff_output.stdout);
    let hunks = parse_diff_hunks(&diff_text);

    // 3. For each changed file+range, find overlapping symbols
    let mut directly_changed: Vec<(String, String, String, u32, u32)> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (file, start, end) in &hunks {
        let symbols = gq.symbols_in_range(file, *start, *end)?;
        for s in symbols {
            if seen_ids.insert(s.id.clone()) {
                directly_changed.push((s.id, s.name, s.file, s.start_line, s.end_line));
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!("=== Change Detection (base: {}) ===\n\n", base));
    out.push_str(&format!("Changed files: {}\n", changed_files.len()));
    for f in &changed_files {
        out.push_str(&format!("  {}\n", f));
    }

    out.push_str(&format!(
        "\n=== Directly Changed Symbols ({}) ===\n",
        directly_changed.len()
    ));
    if directly_changed.is_empty() {
        out.push_str("  (no indexed symbols overlap with changed lines)\n");
    } else {
        for (_, name, file, start, end) in &directly_changed {
            out.push_str(&format!("  {:30} {} L{}-{}\n", name, file, start, end));
        }
    }

    // 4. Compute blast radius
    if !directly_changed.is_empty() && depth > 0 {
        let mut indirectly_affected: Vec<(String, String, String, String)> = Vec::new();
        let mut indirect_ids: HashSet<String> = HashSet::new();

        for (id, _, _, _, _) in &directly_changed {
            if let Ok(impacted) = gq.transitive_impact(id, depth) {
                for row in impacted {
                    if !seen_ids.contains(&row.id) && indirect_ids.insert(row.id.clone()) {
                        indirectly_affected.push((row.id, row.name, row.file, row.kind));
                    }
                }
            }
        }

        out.push_str(&format!(
            "\n=== Blast Radius (depth={}, {} indirectly affected) ===\n",
            depth,
            indirectly_affected.len()
        ));
        if indirectly_affected.is_empty() {
            out.push_str("  (no additional symbols affected)\n");
        } else {
            for (_, name, file, kind) in &indirectly_affected {
                out.push_str(&format!("  {:>8} {:30} {}\n", kind, name, file));
            }
        }
    }

    Ok(out)
}

/// Parse unified diff output (with --unified=0) to extract (file, start_line, end_line) hunks.
fn parse_diff_hunks(diff: &str) -> Vec<(String, u32, u32)> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.to_string();
            continue;
        }

        if line.starts_with("@@") && !current_file.is_empty() {
            if let Some(plus_part) = line.split('+').nth(1) {
                let range_part = plus_part.split(' ').next().unwrap_or("");
                let parts: Vec<&str> = range_part.split(',').collect();
                let start: u32 = parts[0].parse().unwrap_or(0);
                let count: u32 = if parts.len() > 1 {
                    parts[1].parse().unwrap_or(1)
                } else {
                    1
                };
                if start > 0 {
                    let end = if count == 0 { start } else { start + count - 1 };
                    hunks.push((current_file.clone(), start, end));
                }
            }
        }
    }

    hunks
}

// ─── New tools ───────────────────────────────────────────────────────────────

fn tool_list_projects(_args: &Value) -> Result<String> {
    let registry = Registry::load()?;

    if registry.repos.is_empty() {
        return Ok("No projects registered. Run index_project to index a codebase.".to_string());
    }

    let mut out = format!("Registered projects ({}):\n", registry.repos.len());
    let mut repos: Vec<_> = registry.repos.values().collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in &repos {
        out.push_str(&format!(
            "  {:30} {:>6} symbols  {}\n",
            entry.name,
            entry.symbol_count,
            entry.path.display()
        ));
    }
    Ok(out)
}

fn tool_delete_project(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let project_path = PathBuf::from(path);

    // Remove the .infigraph directory within the project
    let infigraph_dir = project_path.join(".infigraph");
    if infigraph_dir.exists() {
        std::fs::remove_dir_all(&infigraph_dir).context("failed to remove .infigraph directory")?;
    }

    // Unregister from the global registry
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
        Ok(format!(
            "Removed .infigraph directory from {}. (Project was not in the global registry.)",
            path
        ))
    } else {
        Ok(format!(
            "Removed .infigraph directory and unregistered '{}' from global registry.",
            to_remove.join(", ")
        ))
    }
}

fn tool_list_languages(_args: &Value) -> Result<String> {
    let registry = bundled_registry()?;
    let mut out = String::new();
    let mut count = 0;
    for pack in registry.languages() {
        out.push_str(&format!(
            "  {:20} {}\n",
            pack.name,
            pack.extensions.join(", ")
        ));
        count += 1;
    }
    Ok(format!("Supported languages ({}):\n{}", count, out))
}

fn tool_get_graph_schema(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let stats = prism.stats()?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let mut out = String::new();

    out.push_str("=== Node Types ===\n");
    out.push_str(&format!(
        "  Symbol  ({} nodes)  properties: id, name, kind, file, start_line, end_line, docstring\n",
        stats.symbols
    ));
    out.push_str(&format!(
        "  Module  ({} nodes)  properties: id, name, file, language\n",
        stats.modules
    ));

    out.push_str("\n=== Edge Types ===\n");
    out.push_str(&format!(
        "  CALLS     ({} edges)  Symbol -> Symbol\n",
        stats.calls
    ));
    out.push_str(&format!(
        "  INHERITS  ({} edges)  Symbol -> Symbol\n",
        stats.inherits
    ));
    out.push_str(&format!(
        "  CONTAINS  ({} edges)  Module -> Symbol, Symbol -> Symbol\n",
        stats.contains
    ));

    // Show symbol kinds present in the graph
    out.push_str("\n=== Symbol Kinds ===\n");
    let kind_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.kind, count(s) ORDER BY count(s) DESC")?;
    for row in &kind_rows {
        out.push_str(&format!("  {:>20}: {}\n", row[0], row[1]));
    }

    Ok(out)
}

fn tool_symbol_context(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    // Find the symbol
    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let mut out = String::new();
    out.push_str(&format!("=== Symbol: {} ===\n", symbol_id));
    out.push_str(&format!("  Name:       {}\n", detail.name));
    out.push_str(&format!("  Kind:       {}\n", detail.kind));
    out.push_str(&format!("  File:       {}\n", detail.file));
    out.push_str(&format!(
        "  Lines:      {}-{}\n",
        detail.start_line, detail.end_line
    ));

    // Docstring
    let doc_rows = gq.raw_query(&format!(
        "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.docstring",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = doc_rows.first() {
        if !row[0].is_empty() {
            out.push_str(&format!("  Docstring:  {}\n", row[0]));
        }
    }

    // Parent (containing scope)
    let parent_rows = gq.raw_query(&format!(
        "MATCH (parent)-[:CONTAINS]->(s:Symbol) WHERE s.id = '{}' RETURN parent.id, parent.name",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = parent_rows.first() {
        out.push_str(&format!("  Parent:     {} ({})\n", row[1], row[0]));
    }

    // Callers
    let callers = gq.callers_of(symbol_id)?;
    out.push_str(&format!("\n  Callers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("    {}\n", c));
        }
    }

    // Callees
    let callees = gq.callees_of(symbol_id)?;
    out.push_str(&format!("\n  Callees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("    {}\n", c));
        }
    }

    Ok(out)
}

fn tool_group_list(_args: &Value) -> Result<String> {
    let registry = Registry::load()?;

    if registry.groups.is_empty() {
        return Ok("No groups defined. Use group_create to create one.".to_string());
    }

    let mut out = format!("Groups ({}):\n", registry.groups.len());
    let mut groups: Vec<_> = registry.groups.values().collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    for group in &groups {
        out.push_str(&format!(
            "  {:30} {} repos, {} contracts\n",
            group.name,
            group.repos.len(),
            group.contracts.len()
        ));
        for repo_name in &group.repos {
            if let Some(entry) = registry.repos.get(repo_name) {
                out.push_str(&format!(
                    "    - {:25} ({} symbols)\n",
                    repo_name, entry.symbol_count
                ));
            } else {
                out.push_str(&format!("    - {:25} (not registered)\n", repo_name));
            }
        }
    }
    Ok(out)
}

fn tool_group_create(args: &Value) -> Result<String> {
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .context("missing 'name' argument")?;

    let mut registry = Registry::load()?;
    registry.create_group(name)?;

    Ok(format!("Group '{}' created.", name))
}

fn tool_group_add(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let repo_name = args
        .get("repo_name")
        .and_then(|r| r.as_str())
        .context("missing 'repo_name' argument")?;

    // If the repo isn't registered yet and a path is given, register it first
    let mut registry = Registry::load()?;
    if !registry.repos.contains_key(repo_name) {
        if let Some(path_str) = args.get("path").and_then(|p| p.as_str()) {
            let lang_registry = bundled_registry()?;
            let mut prism = Infigraph::open(&PathBuf::from(path_str), lang_registry)?;
            prism.init()?;
            registry.register_repo(repo_name, &PathBuf::from(path_str), &prism)?;
        }
    }

    registry.group_add(group_name, repo_name)?;

    Ok(format!(
        "Added repo '{}' to group '{}'.",
        repo_name, group_name
    ))
}

fn tool_group_query(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let cypher = args
        .get("cypher")
        .and_then(|c| c.as_str())
        .context("missing 'cypher' argument")?;

    let registry = Registry::load()?;
    let results = registry.group_query(group_name, cypher, bundled_registry)?;

    if results.is_empty() {
        return Ok(format!(
            "No results across repos in group '{}'.",
            group_name
        ));
    }

    let mut out = String::new();
    for (repo_name, rows) in &results {
        out.push_str(&format!("=== {} ({} rows) ===\n", repo_name, rows.len()));
        for row in rows {
            out.push_str(&format!("  {}\n", row.join(" | ")));
        }
    }
    Ok(out)
}

fn tool_group_sync(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let mut registry = Registry::load()?;
    let count = multi::sync_group_contracts(&mut registry, group_name, bundled_registry)?;

    Ok(format!(
        "Extracted {} contracts from group '{}'.",
        count, group_name
    ))
}

fn tool_group_contracts(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;

    if group.contracts.is_empty() {
        return Ok(format!(
            "No contracts in group '{}'. Run group_sync first.",
            group_name
        ));
    }

    let mut out = format!(
        "Contracts in group '{}' ({}):\n",
        group_name,
        group.contracts.len()
    );
    for c in &group.contracts {
        out.push_str(&format!(
            "  {:?} {} {} {}  (symbol: {}, file: {})\n",
            c.kind, c.service, c.method, c.path, c.symbol_id, c.file
        ));
    }
    Ok(out)
}

fn tool_group_deps(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let deps = infigraph_core::multi::detect_cross_service_deps(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )?;

    if deps.is_empty() {
        return Ok(format!(
            "No cross-service dependencies found in group '{}'. Run group_sync first.",
            group_name
        ));
    }

    let mut out = format!(
        "Cross-service dependencies in group '{}' ({}):\n",
        group_name,
        deps.len()
    );
    for d in &deps {
        out.push_str(&format!(
            "  {} ({}) → {} {} {} [{}]\n",
            d.caller_service,
            d.caller_symbol,
            d.target_service,
            d.target_method,
            d.target_path,
            d.caller_file
        ));
    }
    Ok(out)
}

fn tool_group_index(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let full = args.get("full").and_then(|f| f.as_bool()).unwrap_or(false);

    let mut registry = Registry::load()?;
    let results = infigraph_core::multi::index_group(
        &mut registry,
        group_name,
        full,
        infigraph_languages::bundled_registry,
    )?;

    let mut out = format!(
        "Indexed {} repos in group '{}':\n",
        results.len(),
        group_name
    );
    for (repo, indexed, total) in &results {
        out.push_str(&format!("  {}: {}/{} files\n", repo, indexed, total));
    }
    let group = registry.groups.get(group_name);
    if let Some(g) = group {
        for repo_name in &g.repos {
            if let Some(entry) = registry.repos.get(repo_name) {
                let p = entry.path.to_string_lossy();
                if let Some(msg) = auto_start_watch(&p) {
                    out.push_str(&format!("{}\n", msg));
                }
            }
        }
    }
    Ok(out)
}

fn tool_group_link(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let count = infigraph_core::multi::link_cross_service_calls(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )?;

    Ok(format!(
        "Linked {} cross-service CALLS_SERVICE edges in group '{}'.",
        count, group_name
    ))
}

fn tool_detect_clusters(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;

    let stats = infigraph_core::cluster::detect_clusters(&conn)?;
    Ok(format!("{}", stats))
}

fn tool_export_graph(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let format = args
        .get("format")
        .and_then(|f| f.as_str())
        .context("missing 'format' argument")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let mut buf = Vec::new();
    match format {
        "cypher" => infigraph_core::export::export_cypher(&gq, &mut buf)?,
        "graphml" => infigraph_core::export::export_graphml(&gq, &mut buf)?,
        "json" => infigraph_core::export::export_json(&gq, &mut buf)?,
        _ => anyhow::bail!(
            "unknown export format '{}'. Supported: cypher, graphml, json",
            format
        ),
    }

    String::from_utf8(buf).context("export produced invalid UTF-8")
}

fn tool_visualize(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let output_path = prism.root().join(".infigraph").join("graph.html");
    let path = infigraph_core::viz::generate_html(&gq, &output_path)?;
    Ok(format!("Graph visualization written to: {}", path))
}

fn tool_visualize_symbol(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as u32;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let safe_name: String = symbol_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let output_path = prism
        .root()
        .join(".infigraph")
        .join(format!("symbol-{safe_name}.html"));
    let path = infigraph_core::viz::generate_symbol_html(&gq, symbol_id, depth, &output_path)?;
    Ok(format!("Symbol subgraph visualization written to: {path}"))
}

fn tool_detect_routes(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let routes = infigraph_core::routes::detect_routes(&gq)?;
    Ok(infigraph_core::routes::format_routes(&routes))
}

fn tool_index_manifests(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let results = infigraph_core::manifest::index_manifests(prism.root(), store)?;
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
    Ok(out)
}

fn tool_get_dependencies(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let eco_filter = args.get("ecosystem").and_then(|v| v.as_str());

    let mut deps = infigraph_core::manifest::query_deps(store)?;
    if let Some(eco) = eco_filter {
        deps.retain(|d| d.ecosystem == eco);
    }

    if deps.is_empty() {
        return Ok("No dependencies found. Run index_manifests first.".to_string());
    }

    let mut out = format!("Dependencies ({}):\n\n", deps.len());
    let mut cur_eco = String::new();
    for d in &deps {
        if d.ecosystem != cur_eco {
            out.push_str(&format!("## {} \n", d.ecosystem));
            cur_eco = d.ecosystem.clone();
        }
        let dev_tag = if d.is_dev { " [dev]" } else { "" };
        out.push_str(&format!("  {}@{}{}\n", d.name, d.version, dev_tag));
    }
    Ok(out)
}

fn tool_find_all_references(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;

    let refs = gq.find_all_references(symbol_id)?;
    if refs.is_empty() {
        return Ok(format!("No references found for '{}'", symbol_id));
    }

    let mut out = format!("References to '{}' ({} total):\n\n", symbol_id, refs.len());
    for r in &refs {
        out.push_str(&format!("  {}:{} — in {}\n", r.file, r.line, r.caller_name));
    }
    Ok(out)
}

fn tool_get_api_surface(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut syms = gq.get_api_surface()?;
    if let Some(f) = file_filter {
        syms.retain(|s| s.file.contains(f));
    }

    if syms.is_empty() {
        return Ok("No public symbols found. Ensure project is indexed.".to_string());
    }

    let mut out = format!("API Surface ({} symbols):\n\n", syms.len());
    let mut cur_file = String::new();
    for s in &syms {
        if s.file != cur_file {
            out.push_str(&format!("## {}\n", s.file));
            cur_file = s.file.clone();
        }
        let doc = if s.docstring.is_empty() || s.docstring == "''" {
            String::new()
        } else {
            format!(
                " — {}",
                s.docstring
                    .trim_matches('\'')
                    .chars()
                    .take(80)
                    .collect::<String>()
            )
        };
        out.push_str(&format!(
            "  [{kind}] {name} (L{line}){doc}\n",
            kind = s.kind,
            name = s.name,
            line = s.line,
            doc = doc
        ));
    }
    Ok(out)
}

fn tool_get_file_deps(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .context("missing 'file'")?;

    let deps = gq.get_file_deps(file)?;
    let mut out = format!("File dependencies for '{}':\n\n", file);

    out.push_str(&format!("Imports ({}):\n", deps.imports.len()));
    for f in &deps.imports {
        out.push_str(&format!("  → {}\n", f));
    }
    if deps.imports.is_empty() {
        out.push_str("  (none)\n");
    }

    out.push_str(&format!("\nImported by ({}):\n", deps.imported_by.len()));
    for f in &deps.imported_by {
        out.push_str(&format!("  ← {}\n", f));
    }
    if deps.imported_by.is_empty() {
        out.push_str("  (none)\n");
    }

    Ok(out)
}

fn tool_get_type_hierarchy(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let hier = gq.get_type_hierarchy(symbol_id, depth)?;
    let mut out = format!("Type hierarchy for '{}':\n\n", hier.root_name);

    out.push_str(&format!("Ancestors ({}):\n", hier.ancestors.len()));
    for a in &hier.ancestors {
        out.push_str(&format!("  ↑ {} [{}] ({})\n", a.name, a.kind, a.file));
    }
    if hier.ancestors.is_empty() {
        out.push_str("  (none — root type)\n");
    }

    out.push_str(&format!("\nDescendants ({}):\n", hier.descendants.len()));
    for d in &hier.descendants {
        out.push_str(&format!("  ↓ {} [{}] ({})\n", d.name, d.kind, d.file));
    }
    if hier.descendants.is_empty() {
        out.push_str("  (none — leaf type)\n");
    }

    Ok(out)
}

fn tool_get_test_coverage(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut cov = gq.get_test_coverage()?;

    if let Some(f) = file_filter {
        cov.covered.retain(|s| s.file.contains(f));
        cov.uncovered.retain(|s| s.file.contains(f));
        let total = cov.covered.len() + cov.uncovered.len();
        cov.coverage_pct = (cov.covered.len() * 100).checked_div(total).unwrap_or(0);
        cov.covered_count = cov.covered.len();
        cov.uncovered_count = cov.uncovered.len();
    }

    let mut out = format!(
        "Test Coverage: {}% ({} covered, {} uncovered)\n\n",
        cov.coverage_pct, cov.covered_count, cov.uncovered_count
    );

    if !cov.uncovered.is_empty() {
        out.push_str("Uncovered symbols:\n");
        for s in cov.uncovered.iter().take(50) {
            out.push_str(&format!(
                "  ✗ {} [{}] — {}\n",
                s.symbol_name, s.kind, s.file
            ));
        }
        if cov.uncovered.len() > 50 {
            out.push_str(&format!("  ... and {} more\n", cov.uncovered.len() - 50));
        }
    }

    if !cov.covered.is_empty() {
        out.push_str(&format!("\nCovered symbols ({}):\n", cov.covered.len()));
        for s in cov.covered.iter().take(20) {
            out.push_str(&format!("  ✓ {} [{}]\n", s.symbol_name, s.kind));
        }
        if cov.covered.len() > 20 {
            out.push_str(&format!("  ... and {} more\n", cov.covered.len() - 20));
        }
    }

    Ok(out)
}

fn tool_scip_import(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let store = prism.store().context("not initialized")?;

    let index_rel = args
        .get("index")
        .and_then(|v| v.as_str())
        .unwrap_or("index.scip");
    let index_path = if std::path::Path::new(index_rel).is_absolute() {
        std::path::PathBuf::from(index_rel)
    } else {
        root.join(index_rel)
    };

    let stats = infigraph_core::scip::import_scip_index(&index_path, store)?;
    let mut out = format!(
        "SCIP import complete:\n  files processed: {}\n  symbols added: {}\n  symbols enriched: {}\n  relations added: {}\n  references added: {}",
        stats.files_processed,
        stats.symbols_added,
        stats.symbols_enriched,
        stats.relations_added,
        stats.references_added,
    );
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    if let Some(msg) = auto_start_watch(path) {
        out.push_str(&format!("\n{}", msg));
    }
    Ok(out)
}

fn tool_get_complexity(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let threshold = args.get("threshold").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let file_filter = args.get("file").and_then(|v| v.as_str());

    let base_q = if let Some(f) = file_filter {
        format!(
            "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') AND s.file CONTAINS '{}' RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC",
            f.replace('\'', "\\'")
        )
    } else {
        "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC".to_string()
    };

    let rows = gq.raw_query(&base_q)?;

    if rows.is_empty() {
        return Ok("No function/method symbols found. Run index_project first.".to_string());
    }

    let total: u32 = rows
        .iter()
        .filter_map(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()))
        .sum();
    let count = rows.len();
    let avg = if count > 0 {
        total as f64 / count as f64
    } else {
        0.0
    };

    let hotspots: Vec<_> = rows
        .iter()
        .filter(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) >= threshold)
        .collect();

    let mut out = format!(
        "Complexity Analysis: {} symbols, avg {:.1}, {} hotspots (>= {})\n\n",
        count,
        avg,
        hotspots.len(),
        threshold
    );

    if !hotspots.is_empty() {
        out.push_str(&format!("Hotspots (complexity >= {}):\n", threshold));
        for row in &hotspots {
            let name = row.first().map(|s| s.as_str()).unwrap_or("?");
            let file = row.get(1).map(|s| s.as_str()).unwrap_or("?");
            let line = row.get(2).map(|s| s.as_str()).unwrap_or("?");
            let cplx = row.get(3).map(|s| s.as_str()).unwrap_or("?");
            out.push_str(&format!("  [{cplx:>3}] {name}  ({file}:{line})\n"));
        }
        out.push('\n');
    }

    out.push_str("Top 20 by complexity:\n");
    for row in rows.iter().take(20) {
        let name = row.first().map(|s| s.as_str()).unwrap_or("?");
        let file = row.get(1).map(|s| s.as_str()).unwrap_or("?");
        let line = row.get(2).map(|s| s.as_str()).unwrap_or("?");
        let cplx = row.get(3).map(|s| s.as_str()).unwrap_or("?");
        let flag = if cplx.parse::<u32>().unwrap_or(0) >= threshold {
            " ⚠"
        } else {
            ""
        };
        out.push_str(&format!("  [{cplx:>3}] {name}  ({file}:{line}){flag}\n"));
    }

    Ok(out)
}

fn tool_detect_security_issues(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;

    let sev_filter = args
        .get("severity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase());
    let cat_filter = args
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let mut scan = infigraph_core::security::scan_project(&root)?;

    // Apply filters
    if let Some(ref sev) = sev_filter {
        scan.findings.retain(|f| f.severity.to_string() == *sev);
    }
    if let Some(ref cat) = cat_filter {
        scan.findings.retain(|f| {
            f.category.to_string().to_lowercase().replace(' ', "") == cat.replace(' ', "")
        });
    }

    Ok(infigraph_core::security::format_scan_results(&scan))
}

fn tool_semantic_diff(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let old_ref = args
        .get("old_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let new_ref = args
        .get("new_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;
    let registry = bundled_registry()?;
    let diff = infigraph_core::diff::semantic_diff(&root, old_ref, new_ref, &registry)?;
    Ok(infigraph_core::diff::format_diff(&diff))
}

fn tool_watch_project(args: &Value) -> Result<String> {
    init_watchers();

    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let debounce_ms = args
        .get("debounce_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(500);
    let auto_resolve = args
        .get("auto_resolve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&root, registry)?;
    prism.init()?;

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let watcher_id = format!(
        "watch-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let pending_reindex: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let pending_clone = Arc::clone(&pending_reindex);

    {
        let mut guard = get_watchers();
        if let Some(map) = guard.as_mut() {
            map.insert(
                watcher_id.clone(),
                WatcherEntry {
                    stop_tx,
                    path: root_str.clone(),
                    pending_reindex,
                },
            );
        }
    }

    let watcher_id_clone = watcher_id.clone();
    std::thread::spawn(move || {
        let id_short = watcher_id_clone[..12.min(watcher_id_clone.len())].to_string();
        let on_event = {
            let id_short = id_short.clone();
            move |evt: infigraph_core::watch::WatchEvent| {
                if evt.has_cross_file_calls {
                    let file = evt.path.to_string_lossy().replace('\\', "/");
                    eprintln!("[watch {id_short}] {evt}");
                    if !auto_resolve {
                        let mut pending = pending_clone.lock().unwrap();
                        if !pending.contains(&file) {
                            pending.push(file);
                        }
                        eprintln!("[watch {id_short}] ⚠ cross-file calls affected — call index_project to re-resolve (or use auto_resolve=true)");
                    }
                } else {
                    eprintln!("[watch {id_short}] {evt}");
                }
            }
        };
        if auto_resolve {
            // Use auto-resolve variant that runs full reindex on cross-file changes
            if let Err(e) = infigraph_core::watch::watch_project_auto_resolve(
                &prism,
                debounce_ms,
                stop_rx,
                &id_short,
                bundled_registry,
            ) {
                eprintln!("[watch] error: {e}");
            }
        } else if let Err(e) =
            infigraph_core::watch::watch_project(&prism, debounce_ms, stop_rx, on_event)
        {
            eprintln!("[watch] error: {e}");
        }
        let mut guard = WATCHERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(&watcher_id_clone);
        }
    });

    let auto_note = if auto_resolve {
        "\nauto_resolve: ON — full reindex runs automatically when cross-file calls are affected"
    } else {
        "\nauto_resolve: OFF — call index_project when notified of cross-file call changes, or use get_watch_status to check"
    };

    Ok(format!(
        "Watcher started.\nID: {watcher_id}\nPath: {root_str}\nDebounce: {debounce_ms}ms{auto_note}\nUse stop_watch to stop."
    ))
}

fn tool_stop_watch(args: &Value) -> Result<String> {
    let watcher_id = args
        .get("watcher_id")
        .and_then(|v| v.as_str())
        .context("missing 'watcher_id'")?;

    let mut guard = get_watchers();
    if let Some(map) = guard.as_mut() {
        if let Some(entry) = map.remove(watcher_id) {
            let _ = entry.stop_tx.send(());
            return Ok(format!("Watcher {watcher_id} stopped."));
        }
    }
    Ok(format!("No watcher found with ID: {watcher_id}"))
}

fn tool_get_watch_status(args: &Value) -> Result<String> {
    let watcher_id = args.get("watcher_id").and_then(|v| v.as_str());

    let guard = get_watchers();
    let map = match guard.as_ref() {
        Some(m) => m,
        None => return Ok("No watchers running.".to_string()),
    };

    if map.is_empty() {
        return Ok("No watchers running.".to_string());
    }

    if let Some(id) = watcher_id {
        match map.get(id) {
            None => return Ok(format!("No watcher found with ID: {id}")),
            Some(entry) => {
                let pending = entry.pending_reindex.lock().unwrap();
                let mut out = format!("Watcher: {id}\nPath: {}\n", entry.path);
                if pending.is_empty() {
                    out.push_str("Status: OK — no pending reindex needed\n");
                } else {
                    out.push_str(&format!(
                        "⚠ {} file(s) changed with cross-file calls — run index_project to re-resolve:\n",
                        pending.len()
                    ));
                    for f in pending.iter() {
                        out.push_str(&format!("  - {f}\n"));
                    }
                }
                return Ok(out);
            }
        }
    }

    // List all watchers
    let mut out = format!("{} watcher(s) running:\n", map.len());
    for (id, entry) in map.iter() {
        let pending_count = entry.pending_reindex.lock().unwrap().len();
        let warn = if pending_count > 0 {
            format!(" ⚠ {pending_count} pending reindex")
        } else {
            String::new()
        };
        out.push_str(&format!("  {id} — {}{warn}\n", entry.path));
    }
    Ok(out)
}

fn tool_detect_bridges(args: &Value) -> Result<String> {
    use infigraph_core::model::BridgeKind;

    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let kind_filter = args.get("kind").and_then(|v| v.as_str());

    let result = infigraph_core::bridges::detect_bridges(&std::path::PathBuf::from(path))?;

    let bridges: Vec<_> = match kind_filter {
        Some(k) => {
            let k_upper = k.to_uppercase();
            result
                .bridges
                .iter()
                .filter(|b| b.kind.as_str() == k_upper)
                .collect()
        }
        None => result.bridges.iter().collect(),
    };

    if bridges.is_empty() {
        let filter_note = kind_filter
            .map(|k| format!(" (filter: {k})"))
            .unwrap_or_default();
        return Ok(format!("No cross-language bridges detected{filter_note}."));
    }

    let ffi = result.ffi_count();
    let jni = result.jni_count();
    let grpc = result.grpc_count();
    let pinvoke = result.pinvoke_count();
    let cgo = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Cgo)
        .count();
    let ctypes = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Ctypes)
        .count();
    let wasm = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Wasm)
        .count();
    let com = result.com_count();

    let mut out = format!(
        "Cross-language bridges: {} total\n  FFI={} JNI={} CGO={} gRPC={} P/Invoke={} ctypes={} WASM={} COM={}\n\n",
        result.bridges.len(), ffi, jni, cgo, grpc, pinvoke, ctypes, wasm, com
    );

    // Group by file
    let mut by_file: std::collections::HashMap<&str, Vec<_>> = std::collections::HashMap::new();
    for b in &bridges {
        by_file.entry(&b.file).or_default().push(b);
    }
    let mut files: Vec<&str> = by_file.keys().copied().collect();
    files.sort_unstable();

    for file in files {
        let file_bridges = &by_file[file];
        out.push_str(&format!("{}:\n", file));
        let mut sorted = file_bridges.to_vec();
        sorted.sort_by_key(|b| b.line);
        for b in sorted {
            let target = b.target_language.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "  L{} [{}] {} -> {} | {}\n",
                b.line,
                b.kind.as_str(),
                b.foreign_symbol,
                target,
                b.detail
            ));
        }
    }

    Ok(out)
}

fn tool_semantic_search(args: &Value) -> Result<String> {
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

fn tool_get_doc_context(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found", symbol_id))?;

    let mut out = format!("=== {} {} ===\n", detail.kind, detail.name);
    out.push_str(&format!(
        "File:  {}:{}-{}\n",
        detail.file, detail.start_line, detail.end_line
    ));

    // Docstring
    let doc_rows = gq.raw_query(&format!(
        "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.docstring, s.complexity",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = doc_rows.first() {
        if !row[0].is_empty() {
            out.push_str(&format!("Doc:   {}\n", row[0]));
        }
        if !row[1].is_empty() && row[1] != "1" {
            out.push_str(&format!("Complexity: {}\n", row[1]));
        }
    }

    // Source snippet (read file directly)
    let file_path = prism.root().join(&detail.file);
    if let Ok(source) = std::fs::read_to_string(&file_path) {
        let lines: Vec<&str> = source.lines().collect();
        let start = (detail.start_line as usize).saturating_sub(1);
        let end = (detail.end_line as usize).min(lines.len());
        if start < end {
            out.push_str("\nSource:\n```\n");
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{:4}  {}\n", start + i + 1, line));
            }
            out.push_str("```\n");
        }
    }

    // Callers
    let callers = gq.callers_of(symbol_id)?;
    out.push_str(&format!("\nCallers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("  (none — possible entry point or dead code)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("  {}\n", c));
        }
    }

    // Callees
    let callees = gq.callees_of(symbol_id)?;
    out.push_str(&format!("\nCallees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("  {}\n", c));
        }
    }

    Ok(out)
}

fn tool_detect_clones(args: &Value) -> Result<String> {
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

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    // Fetch symbols to check
    let kind_filter = kinds
        .iter()
        .map(|k| format!("s.kind = '{}'", k))
        .collect::<Vec<_>>()
        .join(" OR ");
    let query = format!(
        "MATCH (s:Symbol) WHERE ({kind_filter}) RETURN s.id, s.name, s.kind, s.file, s.docstring"
    );
    let rows = gq.raw_query(&query)?;

    if rows.len() < 2 {
        return Ok("Not enough symbols to compare. Run index_project first.".to_string());
    }

    // Build embeddings
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

    let symbol_vecs: Vec<(String, String, String, Vec<f32>)> = rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            let emb = cached
                .get(&id)
                .cloned()
                .unwrap_or_else(|| embedder.embed(&text).unwrap_or_default());
            (id, row[1].clone(), row[3].clone(), emb)
        })
        .filter(|(_, _, _, emb)| !emb.is_empty())
        .collect();

    // Pairwise comparison
    let n = symbol_vecs.len();
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();

    for i in 0..n {
        for j in (i + 1)..n {
            // Skip same file (often fine to have similar helpers in same file)
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

    // Optionally write SIMILAR_TO edges
    if store_edges && !pairs.is_empty() {
        let write_conn = store.connection()?;
        for (score, i, j) in &pairs {
            let id_a = &symbol_vecs[*i].0;
            let id_b = &symbol_vecs[*j].0;
            let escape = |s: &str| s.replace('\'', "\\'");
            let _ = write_conn.query(&format!(
                "MATCH (a:Symbol), (b:Symbol) WHERE a.id = '{}' AND b.id = '{}' \
                 MERGE (a)-[r:SIMILAR_TO]->(b) SET r.score = {}",
                escape(id_a),
                escape(id_b),
                score
            ));
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

fn tool_refactor(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;

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

    let recs = infigraph_core::refactor::analyze(&conn, emb_ref, target, focus, limit)?;
    Ok(infigraph_core::refactor::format_recommendations(
        &recs, target,
    ))
}

fn tool_git_summary(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let n_commits = args.get("n_commits").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let author_filter = args.get("author").and_then(|v| v.as_str());
    let file_filter = args.get("file").and_then(|v| v.as_str());

    let root = prism.root().to_path_buf();
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    // Get recent commit hashes + metadata
    let n_commits_arg = format!("-{}", n_commits);
    let mut log_cmd_args: Vec<String> = vec![
        "log".to_string(),
        "--format=%H\x1f%an\x1f%ae\x1f%ai\x1f%s".to_string(),
        n_commits_arg,
    ];
    if let Some(author) = author_filter {
        log_cmd_args.push(format!("--author={}", author));
    }
    if let Some(file) = file_filter {
        log_cmd_args.push("--".to_string());
        log_cmd_args.push(file.to_string());
    }

    let log_out = std::process::Command::new("git")
        .args(&log_cmd_args)
        .current_dir(&root)
        .output()
        .context("failed to run git log")?;

    if !log_out.status.success() {
        let stderr = String::from_utf8_lossy(&log_out.stderr);
        anyhow::bail!("git log failed: {}", stderr.trim());
    }

    let log_text = String::from_utf8_lossy(&log_out.stdout);
    let commits: Vec<(&str, &str, &str, &str, &str)> = log_text
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(5, '\x1f').collect();
            if parts.len() == 5 {
                Some((parts[0], parts[1], parts[2], parts[3], parts[4]))
            } else {
                None
            }
        })
        .collect();

    if commits.is_empty() {
        return Ok("No commits found.".to_string());
    }

    let mut out = format!("Git Summary — last {} commits\n\n", commits.len());

    for (hash, author, _email, date, subject) in &commits {
        let short = &hash[..8.min(hash.len())];

        // Get files changed in this commit
        let parent_ref = format!("{}^", hash);
        let mut diff_cmd_args: Vec<String> = vec![
            "diff".to_string(),
            "--unified=0".to_string(),
            parent_ref,
            hash.to_string(),
        ];
        if let Some(file) = file_filter {
            diff_cmd_args.push("--".to_string());
            diff_cmd_args.push(file.to_string());
        }

        let diff_out = std::process::Command::new("git")
            .args(&diff_cmd_args)
            .current_dir(&root)
            .output();

        let diff_text_owned;
        let hunks = match diff_out {
            Ok(o) if o.status.success() => {
                diff_text_owned = String::from_utf8_lossy(&o.stdout).to_string();
                parse_diff_hunks(&diff_text_owned)
            }
            _ => vec![],
        };

        // Collect touched symbols
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (file, start, end) in &hunks {
            if let Ok(syms) = gq.symbols_in_range(file, *start, *end) {
                for s in syms {
                    touched.insert(format!(
                        "{} {} ({}:{})",
                        s.kind, s.name, s.file, s.start_line
                    ));
                }
            }
        }

        // Name-only list for files that had changes but no indexed symbols
        let parent_ref2 = format!("{}^", hash);
        let files_out = std::process::Command::new("git")
            .args(["diff", "--name-only", &parent_ref2, hash])
            .current_dir(&root)
            .output();
        let changed_files: Vec<String> = match files_out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            Err(_) => vec![],
        };

        // Date: just the date part (drop time zone)
        let date_short = date.get(..10).unwrap_or(date);

        out.push_str(&format!(
            "━━ {} {} — {} — {}\n",
            short, date_short, author, subject
        ));
        out.push_str(&format!("   Files changed: {}\n", changed_files.len()));
        for f in &changed_files {
            out.push_str(&format!("     {}\n", f));
        }
        if !touched.is_empty() {
            let mut sorted: Vec<_> = touched.iter().collect();
            sorted.sort();
            out.push_str(&format!("   Symbols touched ({}):\n", sorted.len()));
            for s in sorted {
                out.push_str(&format!("     + {}\n", s));
            }
        } else if !changed_files.is_empty() {
            out.push_str("   Symbols touched: none indexed in changed lines\n");
        }
        out.push('\n');
    }

    Ok(out)
}

fn tool_list_files(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query("MATCH (s:Symbol) RETURN DISTINCT s.file ORDER BY s.file")?;
    if rows.is_empty() {
        return Ok("No files indexed. Run index_project first.".to_string());
    }

    let glob = args.get("glob").and_then(|g| g.as_str()).unwrap_or("");

    let mut files: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.first().map(|s| s.as_str()))
        .filter(|f| glob.is_empty() || glob_matches(glob, f))
        .collect();
    files.dedup();

    Ok(files.join("\n"))
}

fn tool_generate_sequence_diagram(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id' argument")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);
    infigraph_core::sequence::generate_sequence_mermaid(&gq, symbol_id, depth)
}

fn save_analysis(path: &str, tool_name: &str, content: &str) -> Result<String> {
    let root = PathBuf::from(path);
    let dir = root.join(".infigraph").join("sessions").join("analysis");
    std::fs::create_dir_all(&dir)?;

    let date = session_date_id().replace("session_", "");
    let filename = format!("{tool_name}_{date}.md");
    let filepath = dir.join(&filename);
    std::fs::write(&filepath, content)?;

    let lines = content.lines().count();
    let summary: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
    Ok(format!(
        "Saved to {}\n({} lines, {} bytes)\n\n{}",
        filepath.display(),
        lines,
        content.len(),
        summary
    ))
}

fn log_activity(tool_name: &str, args: &Value) {
    if matches!(
        tool_name,
        "get_latest_session"
            | "save_session"
            | "search_sessions"
            | "purge_sessions"
            | "list_projects"
    ) {
        return;
    }
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
    if path.is_empty() {
        return;
    }
    let sessions_dir = PathBuf::from(path).join(".infigraph").join("sessions");
    if std::fs::create_dir_all(&sessions_dir).is_err() {
        return;
    }
    let date = session_date_id().replace("session_", "");
    let log_path = sessions_dir.join(format!("activity_{date}.jsonl"));
    let ts = session_epoch();
    let mut key_args = serde_json::Map::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "path" {
                continue;
            }
            if let Some(s) = v.as_str() {
                let truncated = if s.len() > 120 { &s[..120] } else { s };
                key_args.insert(k.clone(), json!(truncated));
            }
        }
    }
    let entry = json!({"ts": ts, "tool": tool_name, "args": key_args});
    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn session_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn session_date_id() -> String {
    let secs = session_epoch();
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < dy {
            break;
        }
        remaining -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let md = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 0usize;
    for (i, &d) in md.iter().enumerate() {
        if remaining < d {
            mo = i;
            break;
        }
        remaining -= d;
    }
    format!("session_{y:04}-{:02}-{:02}", mo + 1, remaining + 1)
}

fn tool_save_session(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args.get("path").and_then(|p| p.as_str()).context("missing 'path'")?;
    let summary = args.get("summary").and_then(|s| s.as_str()).context("missing 'summary'")?;
    let pending_tasks = args.get("pending_tasks").and_then(|s| s.as_str()).unwrap_or("");
    let decisions = args.get("decisions").and_then(|s| s.as_str()).unwrap_or("");
    let files_touched = args.get("files_touched").and_then(|s| s.as_str()).unwrap_or("");
    let constraints = args.get("constraints").and_then(|s| s.as_str()).unwrap_or("");
    let assumptions = args.get("assumptions").and_then(|s| s.as_str()).unwrap_or("");
    let blockers = args.get("blockers").and_then(|s| s.as_str()).unwrap_or("");
    let narrative = args.get("narrative").and_then(|s| s.as_str()).unwrap_or("");

    let now = session_epoch();
    let session_id = session_date_id();

    let new_files: Vec<&str> = files_touched.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

    let session = if let Some(existing) = store.load(&session_id)? {
        let merged_decisions = if decisions.is_empty() {
            existing.decisions.clone()
        } else if existing.decisions.is_empty() {
            decisions.to_string()
        } else {
            format!("{} | {}", existing.decisions, decisions)
        };

        let mut all_files: Vec<String> = existing.files_touched
            .split(", ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for f in &new_files {
            if !all_files.iter().any(|x| x == f) {
                all_files.push(f.to_string());
            }
        }

        SessionData {
            id: session_id.clone(),
            summary: summary.to_string(),
            pending_tasks: pending_tasks.to_string(),
            decisions: merged_decisions,
            files_touched: all_files.join(", "),
            constraints: constraints.to_string(),
            assumptions: assumptions.to_string(),
            blockers: blockers.to_string(),
            created_at: existing.created_at,
            updated_at: now,
        }
    } else {
        SessionData {
            id: session_id.clone(),
            summary: summary.to_string(),
            pending_tasks: pending_tasks.to_string(),
            decisions: decisions.to_string(),
            files_touched: new_files.join(", "),
            constraints: constraints.to_string(),
            assumptions: assumptions.to_string(),
            blockers: blockers.to_string(),
            created_at: now,
            updated_at: now,
        }
    };

    store.save(&session)?;

    let root = PathBuf::from(path);
    let sessions_dir = root.join(".infigraph").join("sessions");

    if !narrative.is_empty() {
        let md_path = sessions_dir.join(format!("{session_id}.md"));
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&md_path)?;
        let ts_secs = now % 86400;
        let hh = ts_secs / 3600;
        let mm = (ts_secs % 3600) / 60;
        writeln!(f, "\n## Save @ {hh:02}:{mm:02} UTC\n")?;
        writeln!(f, "{narrative}")?;
    }

    let emb_path = sessions_dir.join("embeddings.bin");
    let embed_text = format!("{summary} {pending_tasks} {decisions} {constraints} {assumptions} {narrative}");
    let embedder = embed::code_embedder();
    let vec = embedder.embed(&embed_text)?;
    let mut emb_store = embed::load_embeddings(&emb_path).unwrap_or_default();
    emb_store.retain(|(id, _)| id != &session_id);
    emb_store.push((session_id.clone(), vec));
    embed::save_embeddings(&emb_path, &emb_store)?;

    Ok(format!("Session saved: {session_id}"))
}

const CLUSTER_GAP_SECS: i64 = 72 * 3600;

fn detect_session_cluster(store: &SessionStore) -> Result<Vec<SessionData>> {
    let sorted = store.list_by_updated()?;
    if sorted.len() <= 1 {
        return Ok(sorted);
    }

    let mut cluster = vec![sorted[0].clone()];
    for session in &sorted[1..] {
        let prev_updated = cluster.last().unwrap().updated_at;
        if prev_updated - session.updated_at <= CLUSTER_GAP_SECS {
            cluster.push(session.clone());
        } else {
            break;
        }
    }
    Ok(cluster)
}

fn date_from_session_id(id: &str) -> &str {
    id.strip_prefix("session_").unwrap_or(id)
}

fn format_session_output(session: &SessionData, idx: usize, total: usize, path: &str) -> String {
    let mut out = String::new();

    if total == 1 {
        out.push_str("## Last Session Context\n\n");
    } else {
        out.push_str(&format!("## Session {} of {}\n\n", idx + 1, total));
    }
    out.push_str(&format!("**Session:** {}\n\n", session.id));
    if !session.summary.is_empty() { out.push_str(&format!("**Summary:** {}\n\n", session.summary)); }
    if !session.pending_tasks.is_empty() { out.push_str(&format!("**Pending Tasks:** {}\n\n", session.pending_tasks)); }
    if !session.decisions.is_empty() { out.push_str(&format!("**Decisions:** {}\n\n", session.decisions)); }
    if !session.files_touched.is_empty() { out.push_str(&format!("**Files Touched:** {}\n\n", session.files_touched)); }
    if !session.constraints.is_empty() { out.push_str(&format!("**Constraints (do not retry):** {}\n\n", session.constraints)); }
    if !session.assumptions.is_empty() { out.push_str(&format!("**Assumptions (do not break):** {}\n\n", session.assumptions)); }
    if !session.blockers.is_empty() { out.push_str(&format!("**Blockers (needs human):** {}\n\n", session.blockers)); }

    let narrative_path = PathBuf::from(path).join(".infigraph").join("sessions").join(format!("{}.md", session.id));
    if narrative_path.exists() {
        out.push_str(&format!("**Narrative log:** `{}` (read for full session context)\n\n", narrative_path.display()));
    }
    out
}

fn append_activity_log(out: &mut String, path: &str) {
    let today_date = session_date_id().replace("session_", "");
    let activity_path = PathBuf::from(path).join(".infigraph").join("sessions").join(format!("activity_{today_date}.jsonl"));
    if activity_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&activity_path) {
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let tail = if total > 20 { &lines[total-20..] } else { &lines[..] };
            if !tail.is_empty() {
                out.push_str(&format!("## Activity Log (today, last {} of {} calls)\n\n", tail.len(), total));
                for line in tail {
                    if let Ok(entry) = serde_json::from_str::<Value>(line) {
                        let tool = entry.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
                        let status = entry.get("status").and_then(|s| s.as_str()).unwrap_or("ok");
                        let marker = if status == "ok" { "" } else { " FAILED" };
                        let args_obj = entry.get("args").cloned().unwrap_or(json!({}));
                        let args_str = serde_json::to_string(&args_obj).unwrap_or_default();
                        let preview = if args_str.len() > 80 { &args_str[..80] } else { &args_str };
                        out.push_str(&format!("- `{tool}`{marker} {preview}\n"));
                    }
                }
                out.push('\n');
            }
        }
    }
}

fn append_old_session_hint(sessions_dir: &std::path::Path, out: &mut String) {
    if let Ok(entries) = std::fs::read_dir(sessions_dir) {
        let session_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                s.starts_with("session_") && s.ends_with(".json")
            })
            .collect();
        if session_files.len() > 30 {
            out.push_str(&format!(
                "\n> {} session files found. Consider running `purge_sessions` to clean up old sessions.\n",
                session_files.len()
            ));
        }
    }
}

fn tool_get_latest_session(args: &Value) -> Result<String> {
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let explicit_limit = args.get("limit").and_then(|v| v.as_u64());
    let store = open_session_store(args)?;

    let sessions = if explicit_limit.is_some() {
        let limit = explicit_limit.unwrap() as usize;
        store.list_recent(limit)?
    } else {
        detect_session_cluster(&store)?
    };

    if sessions.is_empty() {
        return Ok("No previous sessions found. This is a fresh start.".to_string());
    }

    let mut out = String::new();
    let total = sessions.len();

    if total > 1 {
        let newest_date = date_from_session_id(&sessions[0].id);
        let oldest_date = date_from_session_id(&sessions[total - 1].id);
        out.push_str(&format!(
            "## {} parallel sessions detected ({} — {})\n\n\
             **Ask the user which session to resume before proceeding.**\n\n",
            total, oldest_date, newest_date
        ));
    }

    for (idx, session) in sessions.iter().enumerate() {
        out.push_str(&format_session_output(session, idx, total, path));
        if idx < total - 1 {
            out.push_str("\n---\n\n");
        }
    }

    append_activity_log(&mut out, path);
    append_old_session_hint(store.sessions_dir(), &mut out);

    Ok(out)
}

fn tool_purge_sessions(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args.get("path").and_then(|p| p.as_str()).context("missing 'path'")?;
    let older_than_days = args.get("older_than_days").and_then(|v| v.as_u64()).unwrap_or(30);

    let now = session_epoch();
    let cutoff = now - (older_than_days as i64 * 86400);

    let all = store.list_all()?;
    let to_purge: Vec<&SessionData> = all.iter().filter(|s| s.created_at < cutoff).collect();

    if to_purge.is_empty() {
        return Ok(format!("No sessions older than {older_than_days} days found."));
    }

    let purged_ids: Vec<String> = to_purge.iter().map(|s| s.id.clone()).collect();

    for id in &purged_ids {
        store.delete(id)?;
    }

    let root = PathBuf::from(path);
    let emb_path = root.join(".infigraph").join("sessions").join("embeddings.bin");
    if emb_path.exists() {
        let mut emb_store = embed::load_embeddings(&emb_path).unwrap_or_default();
        let before = emb_store.len();
        emb_store.retain(|(id, _)| !purged_ids.contains(id));
        if emb_store.len() < before {
            embed::save_embeddings(&emb_path, &emb_store)?;
        }
    }

    let mut out = format!("Purged {} session(s) older than {} days:\n", to_purge.len(), older_than_days);
    for s in &to_purge {
        let preview = if s.summary.len() > 60 { &s.summary[..60] } else { &s.summary };
        out.push_str(&format!("- {}: {preview}\n", s.id));
    }
    Ok(out)
}

fn tool_search_sessions(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args.get("path").and_then(|p| p.as_str()).context("missing 'path'")?;
    let query = args.get("query").and_then(|s| s.as_str()).context("missing 'query'")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let root = PathBuf::from(path);
    let emb_path = root.join(".infigraph").join("sessions").join("embeddings.bin");

    if !emb_path.exists() {
        return Ok("No session embeddings found. Save at least one session with `save_session` first.".to_string());
    }

    let emb_store = embed::load_embeddings(&emb_path)?;
    if emb_store.is_empty() {
        return Ok("No session embeddings found.".to_string());
    }

    let embedder = embed::code_embedder();
    let query_vec = embedder.embed(query)?;
    if query_vec.is_empty() {
        return Ok("Failed to embed query.".to_string());
    }

    let mut scored: Vec<(f32, &str)> = emb_store.iter()
        .map(|(id, vec)| (embed::cosine_similarity(&query_vec, vec), id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let mut out = format!("## Session Search: \"{query}\"\n\n");

    for (score, session_id) in &scored {
        if let Some(session) = store.load(session_id)? {
            out.push_str(&format!("### {} (relevance: {:.3})\n\n", session.id, score));
            if !session.summary.is_empty() {
                out.push_str(&format!("**Summary:** {}\n\n", session.summary));
            }
            if !session.pending_tasks.is_empty() {
                out.push_str(&format!("**Pending Tasks:** {}\n\n", session.pending_tasks));
            }
            if !session.decisions.is_empty() {
                out.push_str(&format!("**Decisions:** {}\n\n", session.decisions));
            }
            if !session.files_touched.is_empty() {
                out.push_str(&format!("**Files Touched:** {}\n\n", session.files_touched));
            }
            let narrative_path = root.join(".infigraph").join("sessions").join(format!("{session_id}.md"));
            if narrative_path.exists() {
                out.push_str(&format!("**Narrative log:** `{}` (read for full context)\n\n", narrative_path.display()));
            }
            out.push_str("---\n\n");
        }
    }

    Ok(out)
}

fn glob_matches(glob: &str, path: &str) -> bool {
    // Simple glob: * matches any sequence, ? matches one char
    let gi = glob.chars().peekable();
    let pi = path.chars().peekable();
    glob_match_inner(&gi.collect::<Vec<_>>(), &pi.collect::<Vec<_>>())
}

fn glob_match_inner(glob: &[char], path: &[char]) -> bool {
    match (glob.first(), path.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            // ** matches path separators too; * stops at /
            let greedy = glob.first() == Some(&'*') && glob.get(1) == Some(&'*');
            if greedy {
                // try consuming 0..=n chars including /
                for i in 0..=path.len() {
                    if glob_match_inner(&glob[2..], &path[i..]) {
                        return true;
                    }
                }
                false
            } else {
                for i in 0..=path.len() {
                    if path.get(i) == Some(&'/') && i > 0 {
                        break;
                    }
                    if glob_match_inner(&glob[1..], &path[i..]) {
                        return true;
                    }
                }
                false
            }
        }
        (Some('?'), Some(_)) => glob_match_inner(&glob[1..], &path[1..]),
        (Some(g), Some(p)) if g.eq_ignore_ascii_case(p) => glob_match_inner(&glob[1..], &path[1..]),
        _ => false,
    }
}
