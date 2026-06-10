use std::io::{self, BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

mod tools;
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
        tool_def("search", "PRIMARY: Unified search — finds symbols by name, meaning, or text pattern in one call. Runs keyword-hybrid (BM25+vector) AND semantic-hybrid AND regex grep together, merges and deduplicates results. Auto-escalates internally when results are weak — no need to retry with different tools. Use this INSTEAD OF grep/ripgrep/find for ALL search. Set scope='docs' for document-only search.",
            p(true,false,false,json!({"query":{"type":"string","description":"Search query (symbol name, natural language, or text pattern)"},"limit":{"type":"integer","default":20},"kind":{"type":"string","description":"Optional: filter by symbol kind (Function, Method, Class, etc.)"},"file_pattern":{"type":"string","description":"Optional: glob to restrict text search (e.g. '*.py')"},"scope":{"type":"string","enum":["code","docs","all"],"default":"all","description":"Search scope: code (symbols only), docs (documents only), all (both)"},"regex":{"type":"boolean","default":false,"description":"If true, treat query as a raw regex pattern for grep (not escaped)"}})), &["path","query"]),
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

    tools::helpers::log_activity(tool_name, &args);

    let result = match tool_name {
        "index_project" => tools::index::tool_index_project(&args),
        "search" => tools::search::tool_search(&args),
        "search_symbols" => tools::search::tool_search_symbols(&args),
        "query_graph" => tools::graph::tool_query_graph(&args),
        "get_symbols_in_file" => tools::graph::tool_get_symbols_in_file(&args),
        "get_stats" => tools::graph::tool_get_stats(&args),
        "detect_dead_code" => tools::analysis::call_graph::tool_detect_dead_code(&args),
        "trace_callers" => tools::analysis::call_graph::tool_trace_callers(&args),
        "trace_callees" => tools::analysis::call_graph::tool_trace_callees(&args),
        "transitive_impact" => tools::analysis::call_graph::tool_transitive_impact(&args),
        "search_code" => tools::search::tool_search_code(&args),
        "get_code_snippet" => tools::graph::tool_get_code_snippet(&args),
        "get_architecture" => tools::analysis::call_graph::tool_get_architecture(&args),
        "detect_changes" => tools::analysis::git::tool_detect_changes(&args),
        "list_projects" => tools::graph::tool_list_projects(&args),
        "delete_project" => tools::graph::tool_delete_project(&args),
        "list_languages" => tools::graph::tool_list_languages(&args),
        "get_graph_schema" => tools::graph::tool_get_graph_schema(&args),
        "symbol_context" => tools::graph::tool_symbol_context(&args),
        "group_list" => tools::groups::tool_group_list(&args),
        "group_create" => tools::groups::tool_group_create(&args),
        "group_add" => tools::groups::tool_group_add(&args),
        "group_query" => tools::groups::tool_group_query(&args),
        "group_sync" => tools::groups::tool_group_sync(&args),
        "group_contracts" => tools::groups::tool_group_contracts(&args),
        "group_deps" => tools::groups::tool_group_deps(&args),
        "group_index" => tools::groups::tool_group_index(&args),
        "group_link" => tools::groups::tool_group_link(&args),
        "detect_clusters" => tools::analysis::call_graph::tool_detect_clusters(&args),
        "export_graph" => tools::analysis::diagrams::tool_export_graph(&args),
        "visualize" => tools::analysis::diagrams::tool_visualize(&args),
        "visualize_symbol" => tools::analysis::diagrams::tool_visualize_symbol(&args),
        "detect_routes" => tools::graph::tool_detect_routes(&args),
        "scip_import" => tools::index::tool_scip_import(&args),
        "index_manifests" => tools::docs::tool_index_manifests(&args),
        "get_dependencies" => tools::index::tool_get_dependencies(&args),
        "find_all_references" => tools::graph::tool_find_all_references(&args),
        "get_api_surface" => tools::graph::tool_get_api_surface(&args),
        "get_file_deps" => tools::graph::tool_get_file_deps(&args),
        "get_type_hierarchy" => tools::graph::tool_get_type_hierarchy(&args),
        "get_test_coverage" => tools::graph::tool_get_test_coverage(&args),
        "get_complexity" | "analyze_complexity" => tools::graph::tool_get_complexity(&args),
        "detect_security_issues" => tools::analysis::security::tool_detect_security_issues(&args),
        "semantic_diff" => tools::analysis::git::tool_semantic_diff(&args),
        "watch_project" => tools::watch::tool_watch_project(&args),
        "stop_watch" => tools::watch::tool_stop_watch(&args),
        "get_watch_status" => tools::watch::tool_get_watch_status(&args),
        "detect_bridges" => tools::analysis::security::tool_detect_bridges(&args),
        "semantic_search" => tools::search::tool_semantic_search(&args),
        "get_doc_context" => tools::graph::tool_get_doc_context(&args),
        "detect_clones" => tools::analysis::clones::tool_detect_clones(&args),
        "refactor" => tools::analysis::clones::tool_refactor(&args),
        "git_summary" => tools::analysis::git::tool_git_summary(&args),
        "list_files" => tools::graph::tool_list_files(&args),
        "generate_sequence_diagram" => tools::graph::tool_generate_sequence_diagram(&args),
        "save_session" => tools::session::tool_save_session(&args),
        "get_latest_session" => tools::session::tool_get_latest_session(&args),
        "purge_sessions" => tools::session::tool_purge_sessions(&args),
        "search_sessions" => tools::session::tool_search_sessions(&args),
        "review" => tools::docs::tool_review(&args),
        "index_docs" => tools::docs::tool_index_docs(&args),
        "search_docs" => tools::docs::tool_search_docs(&args),
        "clean_docs" => tools::docs::tool_clean_docs(&args),
        "reindex_docs" => tools::docs::tool_reindex_docs(&args),
        "index_confluence" => tools::docs::tool_index_confluence(&args),
        "index_confluence_pages" => tools::docs::tool_index_confluence_pages(&args),
        "watch_docs" => tools::docs::tool_watch_docs(&args),
        "stop_watch_docs" => tools::docs::tool_stop_watch_docs(&args),
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
