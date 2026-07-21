pub mod compress;
pub mod recovery;
pub mod session_context;
pub mod tools;
pub mod web;

use serde_json::{json, Value};

pub use tools::watch::auto_start_watch;

/// Maps MCP tool names to their CLI subcommand names.
/// Used by parity tests to verify every MCP tool has a CLI equivalent.
pub const MCP_TO_CLI_MAP: &[(&str, &str)] = &[
    ("index_project", "index"),
    ("search", "search"),
    ("query_graph", "query"),
    ("get_symbols_in_file", "symbols"),
    ("get_stats", "stats"),
    ("detect_dead_code", "dead-code"),
    ("trace_callers", "callers"),
    ("trace_callees", "callees"),
    ("transitive_impact", "impact"),
    ("search_code", "search-code"),
    ("get_code_snippet", "snippet"),
    ("get_architecture", "architecture"),
    ("detect_changes", "detect-changes"),
    ("list_projects", "repos"),
    ("delete_project", "delete"),
    ("list_languages", "languages"),
    ("detect_clusters", "cluster"),
    ("export_graph", "export"),
    ("visualize", "visualize"),
    ("visualize_symbol", "visualize-symbol"),
    ("detect_routes", "routes"),
    ("scip_import", "scip-import"),
    ("index_manifests", "index-manifests"),
    ("get_dependencies", "dependencies"),
    ("find_all_references", "find-refs"),
    ("get_api_surface", "api-surface"),
    ("get_file_deps", "file-deps"),
    ("get_type_hierarchy", "type-hierarchy"),
    ("get_test_coverage", "test-coverage"),
    ("get_complexity", "complexity"),
    ("get_skeleton", "skeleton"),
    ("detect_security_issues", "security"),
    ("detect_cross_cutting", "concerns"),
    ("detect_config_bindings", "config-bindings"),
    ("detect_reflection", "reflection"),
    ("detect_taint_flows", "taint"),
    ("detect_interprocedural_taint", "taint"), // --inter flag
    ("detect_dynamic_urls", "dynamic-urls"),
    ("detect_path_traversal", "path-traversal"),
    ("ingest_structured", "ingest"),
    ("semantic_diff", "semantic-diff"),
    ("watch_project", "watch"),
    ("detect_bridges", "bridges"),
    ("detect_clones", "clones"),
    ("refactor", "refactor"),
    ("git_summary", "git-summary"),
    ("list_files", "files"),
    ("generate_test_context", "test-context"),
    ("generate_sequence_diagram", "sequence"),
    ("review", "review"),
    ("index_docs", "index-docs"),
    ("search_docs", "search-docs"),
    ("clean_docs", "clean-docs"),
    ("reindex_docs", "reindex-docs"),
    ("index_confluence", "index-confluence"),
    ("pipeline_plugins", "pipeline plugins"),
    ("pipeline_deps", "pipeline deps"),
    ("pipeline_impact", "pipeline impact"),
    ("pipeline_compliance", "pipeline compliance"),
    ("pipeline_query", "pipeline query"),
    // group_* mapped to "group <action>"
    ("group_list", "group list"),
    ("group_create", "group create"),
    ("group_add", "group add"),
    ("group_query", "group query"),
    ("group_sync", "group sync"),
    ("group_contracts", "group contracts"),
    ("group_deps", "group deps"),
    ("group_index", "group index"),
    ("group_link", "group link"),
    ("group_search", "group search"),
    ("group_search_docs", "group search-docs"),
    ("group_build", "group build"),
    ("stop_watch", "watch-stop"),
    ("get_watch_status", "watch-status"),
    ("memory_context", "memory-context"),
    ("consolidate_memory", "consolidate-memory"),
    ("purge_sessions", "purge-sessions"),
];

/// MCP tools that are intentionally MCP-only (no CLI equivalent needed).
/// These are either agent-optimized variants of existing CLI commands,
/// or features that only make sense in an AI agent context.
pub const MCP_ONLY_TOOLS: &[&str] = &[
    "search_symbols",   // CLI has `search` which covers this
    "semantic_search",  // CLI has `search` which covers this
    "symbol_context",   // agent-optimized read — CLI uses `snippet`
    "get_doc_context",  // agent-optimized read — CLI uses `snippet`
    "get_graph_schema", // low value as CLI — use `stats` or `query`
    // stop_watch / get_watch_status now have CLI equivalents: watch-stop, watch-status
    "watch_docs",             // watch runs interactively in CLI
    "stop_watch_docs",        // watch runs interactively in CLI
    "save_session",           // agent session management only
    "get_latest_session",     // agent session management only
    "search_sessions",        // agent session management only
    "index_confluence_pages", // programmatic — CLI has `index-confluence`
    "group_link_docs",        // cross-repo doc linking — runs as part of group_build
    "compress",               // agent-only — compress arbitrary text
];

pub const MCP_TOOL_NAMES: &[&str] = &[
    "index_project",
    "search",
    "search_symbols",
    "query_graph",
    "get_symbols_in_file",
    "get_stats",
    "detect_dead_code",
    "trace_callers",
    "trace_callees",
    "transitive_impact",
    "search_code",
    "get_code_snippet",
    "get_architecture",
    "detect_changes",
    "list_projects",
    "delete_project",
    "list_languages",
    "get_graph_schema",
    "symbol_context",
    "group_list",
    "group_create",
    "group_add",
    "group_query",
    "group_sync",
    "group_contracts",
    "group_deps",
    "group_index",
    "group_link",
    "group_link_docs",
    "group_search",
    "group_search_docs",
    "group_build",
    "detect_clusters",
    "export_graph",
    "visualize",
    "visualize_symbol",
    "detect_routes",
    "scip_import",
    "index_manifests",
    "get_dependencies",
    "find_all_references",
    "get_api_surface",
    "get_file_deps",
    "get_type_hierarchy",
    "get_test_coverage",
    "get_complexity",
    "get_skeleton",
    "detect_security_issues",
    "detect_cross_cutting",
    "detect_config_bindings",
    "detect_reflection",
    "detect_taint_flows",
    "detect_interprocedural_taint",
    "detect_dynamic_urls",
    "detect_path_traversal",
    "ingest_structured",
    "semantic_diff",
    "watch_project",
    "stop_watch",
    "get_watch_status",
    "detect_bridges",
    "semantic_search",
    "get_doc_context",
    "detect_clones",
    "refactor",
    "git_summary",
    "list_files",
    "generate_test_context",
    "generate_sequence_diagram",
    "save_session",
    "get_latest_session",
    "purge_sessions",
    "search_sessions",
    "review",
    "index_docs",
    "search_docs",
    "clean_docs",
    "reindex_docs",
    "index_confluence",
    "index_confluence_pages",
    "watch_docs",
    "stop_watch_docs",
    "pipeline_plugins",
    "pipeline_deps",
    "pipeline_impact",
    "pipeline_compliance",
    "pipeline_query",
    "memory_context",
    "consolidate_memory",
    "compress",
];

pub fn allowed_tools_from_names() -> Vec<String> {
    MCP_TOOL_NAMES
        .iter()
        .map(|name| format!("mcp__infigraph__{name}"))
        .collect()
}

fn tool_compress(args: &Value) -> Result<String, anyhow::Error> {
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'text' parameter"))?;
    let content_type = compress::classify_content(text);
    let compressed = compress::compress_generic(text);
    let raw_tokens = ((text.split_whitespace().count() as f64) * 1.4).ceil() as usize;
    let comp_tokens = ((compressed.split_whitespace().count() as f64) * 1.4).ceil() as usize;
    let ratio = if raw_tokens > 0 {
        ((comp_tokens as f64 / raw_tokens as f64) * 100.0).round()
    } else {
        100.0
    };
    Ok(format!(
        "Detected: {:?} | {raw_tokens} → {comp_tokens} tokens ({ratio}%)\n\n{compressed}",
        content_type
    ))
}

pub fn dispatch_tool(tool_name: &str, args: &Value) -> Result<String, anyhow::Error> {
    match tool_name {
        "index_project" => tools::index::tool_index_project(args),
        "search" => tools::search::tool_search(args),
        "search_symbols" => tools::search::tool_search_symbols(args),
        "query_graph" => tools::graph::tool_query_graph(args),
        "get_symbols_in_file" => tools::graph::tool_get_symbols_in_file(args),
        "get_stats" => tools::graph::tool_get_stats(args),
        "detect_dead_code" => tools::analysis::call_graph::tool_detect_dead_code(args),
        "trace_callers" => tools::analysis::call_graph::tool_trace_callers(args),
        "trace_callees" => tools::analysis::call_graph::tool_trace_callees(args),
        "transitive_impact" => tools::analysis::call_graph::tool_transitive_impact(args),
        "search_code" => tools::search::tool_search_code(args),
        "get_code_snippet" => tools::graph::tool_get_code_snippet(args),
        "get_architecture" => tools::analysis::call_graph::tool_get_architecture(args),
        "detect_changes" => tools::analysis::git::tool_detect_changes(args),
        "list_projects" => tools::graph::tool_list_projects(args),
        "delete_project" => tools::graph::tool_delete_project(args),
        "list_languages" => tools::graph::tool_list_languages(args),
        "get_graph_schema" => tools::graph::tool_get_graph_schema(args),
        "symbol_context" => tools::graph::tool_symbol_context(args),
        "group_list" => tools::groups::tool_group_list(args),
        "group_create" => tools::groups::tool_group_create(args),
        "group_add" => tools::groups::tool_group_add(args),
        "group_query" => tools::groups::tool_group_query(args),
        "group_sync" => tools::groups::tool_group_sync(args),
        "group_contracts" => tools::groups::tool_group_contracts(args),
        "group_deps" => tools::groups::tool_group_deps(args),
        "group_index" => tools::groups::tool_group_index(args),
        "group_link" => tools::groups::tool_group_link(args),
        "group_link_docs" => tools::groups::tool_group_link_docs(args),
        "group_search" => tools::groups::tool_group_search(args),
        "group_search_docs" => tools::groups::tool_group_search_docs(args),
        "group_build" => tools::groups::tool_group_build(args),
        "detect_clusters" => tools::analysis::call_graph::tool_detect_clusters(args),
        "export_graph" => tools::analysis::diagrams::tool_export_graph(args),
        "visualize" => tools::analysis::diagrams::tool_visualize(args),
        "visualize_symbol" => tools::analysis::diagrams::tool_visualize_symbol(args),
        "detect_routes" => tools::graph::tool_detect_routes(args),
        "scip_import" => tools::index::tool_scip_import(args),
        "index_manifests" => tools::docs::tool_index_manifests(args),
        "get_dependencies" => tools::index::tool_get_dependencies(args),
        "find_all_references" => tools::graph::tool_find_all_references(args),
        "get_api_surface" => tools::graph::tool_get_api_surface(args),
        "get_file_deps" => tools::graph::tool_get_file_deps(args),
        "get_type_hierarchy" => tools::graph::tool_get_type_hierarchy(args),
        "get_test_coverage" => tools::graph::tool_get_test_coverage(args),
        "get_complexity" => tools::graph::tool_get_complexity(args),
        "get_skeleton" => tools::graph::tool_get_skeleton(args),
        "detect_security_issues" => tools::analysis::security::tool_detect_security_issues(args),
        "detect_cross_cutting" => tools::analysis::concerns::tool_detect_cross_cutting(args),
        "detect_config_bindings" => tools::analysis::config::tool_detect_config_bindings(args),
        "detect_reflection" => tools::analysis::reflection::tool_detect_reflection(args),
        "detect_taint_flows" => tools::analysis::taint::tool_detect_taint_flows(args),
        "detect_interprocedural_taint" => {
            tools::analysis::taint::tool_detect_interprocedural_taint(args)
        }
        "detect_dynamic_urls" => tools::analysis::taint::tool_detect_dynamic_urls(args),
        "detect_path_traversal" => tools::analysis::taint::tool_detect_path_traversal(args),
        "ingest_structured" => tools::analysis::structured::tool_ingest_structured(args),
        "semantic_diff" => tools::analysis::git::tool_semantic_diff(args),
        "watch_project" => tools::watch::tool_watch_project(args),
        "stop_watch" => tools::watch::tool_stop_watch(args),
        "get_watch_status" => tools::watch::tool_get_watch_status(args),
        "detect_bridges" => tools::analysis::security::tool_detect_bridges(args),
        "semantic_search" => tools::search::tool_semantic_search(args),
        "get_doc_context" => tools::graph::tool_get_doc_context(args),
        "detect_clones" => tools::analysis::clones::tool_detect_clones(args),
        "refactor" => tools::analysis::clones::tool_refactor(args),
        "git_summary" => tools::analysis::git::tool_git_summary(args),
        "list_files" => tools::graph::tool_list_files(args),
        "generate_test_context" => tools::graph::tool_generate_test_context(args),
        "generate_sequence_diagram" => tools::graph::tool_generate_sequence_diagram(args),
        "save_session" => tools::session::tool_save_session(args),
        "get_latest_session" => tools::session::tool_get_latest_session(args),
        "purge_sessions" => tools::session::tool_purge_sessions(args),
        "search_sessions" => tools::session::tool_search_sessions(args),
        "review" => tools::docs::tool_review(args),
        "index_docs" => tools::docs::tool_index_docs(args),
        "search_docs" => tools::docs::tool_search_docs(args),
        "clean_docs" => tools::docs::tool_clean_docs(args),
        "reindex_docs" => tools::docs::tool_reindex_docs(args),
        "index_confluence" => tools::docs::tool_index_confluence(args),
        "index_confluence_pages" => tools::docs::tool_index_confluence_pages(args),
        "watch_docs" => tools::docs::tool_watch_docs(args),
        "stop_watch_docs" => tools::docs::tool_stop_watch_docs(args),
        "pipeline_plugins" => tools::pipelines::tool_pipeline_plugins(args),
        "pipeline_deps" => tools::pipelines::tool_pipeline_deps(args),
        "pipeline_impact" => tools::pipelines::tool_pipeline_impact(args),
        "pipeline_compliance" => tools::pipelines::tool_pipeline_compliance(args),
        "pipeline_query" => tools::pipelines::tool_pipeline_query(args),
        "memory_context" => tools::memory_context::tool_memory_context(args),
        "consolidate_memory" => tools::session::tool_consolidate_memory(args),
        "compress" => tool_compress(args),
        "get_compression_stats" => Ok(session_context::get_compression_stats()),
        _ => Err(anyhow::anyhow!("Unknown tool: {tool_name}")),
    }
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
pub fn build_tools_list() -> Vec<Value> {
    vec![
        tool_def("index_project", "REQUIRED FIRST STEP: Parse all source files and build the code knowledge graph. Must run before any other infigraph tool. Auto-indexes 60+ languages.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("search", "PRIMARY: Unified search — finds symbols by name, meaning, or text pattern in one call. Runs keyword-hybrid (BM25+vector) AND semantic-hybrid AND regex grep together, merges and deduplicates results. Auto-escalates internally when results are weak — no need to retry with different tools. Use this INSTEAD OF grep/ripgrep/find for ALL search. Set scope='docs' for document-only search.",
            p(true,false,false,json!({"query":{"type":"string","description":"Search query (symbol name, natural language, or text pattern)"},"limit":{"type":"integer","default":20},"kind":{"type":"string","description":"Optional: filter by symbol kind (Function, Method, Class, etc.)"},"file_pattern":{"type":"string","description":"Optional: glob to restrict text search (e.g. '*.py')"},"scope":{"type":"string","enum":["code","docs","all"],"default":"all","description":"Search scope: code (symbols only), docs (documents only), all (both)"},"regex":{"type":"boolean","default":false,"description":"If true, treat query as a raw regex pattern for grep (not escaped)"},"detail":{"type":"boolean","default":false,"description":"If true, return full source snippets and doc excerpts. Default (false) returns compact one-line-per-result format."}})), &["path","query"]),
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
        tool_def("get_architecture", "PRIMARY: Codebase architecture overview. Use FIRST when onboarding to a new project. Default returns compact summary (top-5 per section); set detail=true for full listing including all entry points.",
            p(true,false,false,json!({"detail":{"type":"boolean","default":false,"description":"If true, return full listing. Default (false) returns top-5 per section."}})), &["path"]),
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
        tool_def("group_create", "Create a new repo group for organizing related repos (e.g. microservices). Use 'org' to scope group names per-team (prevents collisions on shared Postgres).",
            json!({"name":{"type":"string","description":"Group name"},"org":{"type":"string","description":"Organization scope (defaults to INFIGRAPH_ORG env var). Groups stored as org/name."}}), &["name"]),
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
        tool_def("group_link_docs", "Rebuild the physical combined document store for a group from existing per-repo document indexes, including cross-repo LINKS_TO edges and merged embeddings.",
            json!({"group_name":{"type":"string"}}), &["group_name"]),
        tool_def("group_search", "PRIMARY: Hybrid BM25+vector search across the combined graph of a group. Searches all symbols across all repos in one call. Requires group_build first. Use deep=true when initial results seem incomplete — enriches results with cross-repo graph edges (who calls what across services) for LLM reasoning.",
            json!({"group_name":{"type":"string"},"query":{"type":"string","description":"Search query"},"limit":{"type":"integer","default":20},"alpha":{"type":"number","default":0.3,"description":"BM25/vector blend (0=pure BM25, 1=pure vector)"},"deep":{"type":"boolean","default":false,"description":"Deep mode: enrich results with cross-repo call graph context. Use when tracing cross-service chains or when initial search misses related code in other repos."}}), &["group_name","query"]),
        tool_def("group_search_docs", "PRIMARY: Hybrid BM25+vector search across the physical combined document store for a group. Searches document chunks from every repository. Requires group_build first.",
            json!({"group_name":{"type":"string"},"query":{"type":"string","description":"Document search query"},"limit":{"type":"integer","default":10},"alpha":{"type":"number","default":0.5,"description":"BM25/vector blend (0=pure BM25, 1=pure vector)"}}), &["group_name","query"]),
        tool_def("group_build", "PRIMARY: Full group rebuild in one command. Builds both the combined code graph and physical combined document store with merged embeddings. After build, use group_search for code or group_search_docs for documents.",
            json!({"group_name":{"type":"string"},"full":{"type":"boolean","default":false,"description":"Clean and rebuild from scratch"}}), &["group_name"]),
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
        tool_def("find_all_references", "PRIMARY: Find every location where a symbol is referenced. Use INSTEAD OF grep for rename/refactor safety. Default groups by file; set detail=true for per-line calling context.",
            p(true,true,false,json!({"detail":{"type":"boolean","default":false,"description":"If true, return full per-reference context. Default (false) groups by file."}})), &["path","symbol_id"]),
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
        tool_def("get_skeleton", "Compact annotated file skeleton. Shows one line per symbol: line number, signature, and annotations (complexity, statement count, fan-in). Class/struct members indented. Use INSTEAD OF reading whole files for structural overview.",
            p(true,false,false,json!({"file":{"type":"string","description":"File path (relative to project root)"}})), &["path"]),
        tool_def("detect_security_issues", "PRIMARY: Security vulnerability scan. Use INSTEAD OF manual grep for security patterns. Detects SQL injection, hardcoded secrets, eval/exec, path traversal, SSRF, XXE, weak crypto, command injection, XSS, open redirect. Returns file, line, severity, fix.",
            p(true,false,false,json!({"severity":{"type":"string","description":"Filter: CRITICAL, HIGH, MEDIUM, LOW (default: all)"},"category":{"type":"string","description":"Filter by category e.g. SqlInjection, HardcodedSecret, WeakCrypto"}})), &["path"]),
        tool_def("detect_cross_cutting", "PRIMARY: Detect cross-cutting concerns from annotations/decorators. Finds authorization (@PreAuthorize, @login_required, [Authorize]), validation, caching, transactions, rate limiting, audit logging, feature flags, CORS, async, retry patterns across Java, Python, TypeScript, C#, Ruby, Go, Rust.",
            p(true,false,false,json!({"kind":{"type":"string","description":"Filter by concern kind: Authorization, Validation, Caching, Transaction, RateLimiting, AuditLogging, FeatureFlag, Cors, Async, Retry (default: all)"}})), &["path"]),
        tool_def("detect_config_bindings", "PRIMARY: Detect config-driven conditional resolution. Finds @Profile, @ConditionalOnProperty, @Qualifier (Spring), settings.DEBUG (Django), IsDevelopment() (.NET), Rails.env, #[cfg(feature)] (Rust), //go:build (Go), process.env (Node.js). Also discovers config files (application.yml, appsettings.json, .env, etc.).",
            p(true,false,false,json!({"kind":{"type":"string","description":"Filter by binding kind: Profile, Qualifier, Environment, DjangoSetting, RailsEnv, BuildTag, FeatureGate, EnvConfig (default: all)"},"profile":{"type":"string","description":"Filter by profile name (e.g. 'production', 'default')"}})), &["path"]),
        tool_def("detect_reflection", "PRIMARY: Detect reflection/dynamic invocation sites. Finds Class.forName (Java), ServiceLoader.load (Java), getattr/importlib (Python), dynamic import/require (JS/TS), Activator.CreateInstance (C#), .send (Ruby), reflect (Go). Resolves targets via config files and graph symbols. Emits RESOLVES_TO edges.",
            p(true,false,false,json!({"mechanism":{"type":"string","description":"Filter by mechanism: ClassForName, ServiceLoader, JavaReflection, Getattr, ImportModule, DynamicRequire, DynamicImport, CSharpReflection, RubySend, GoPlugin (default: all)"}})), &["path"]),
        tool_def("detect_taint_flows", "PRIMARY: Intra-procedural taint analysis. Traces data from user-controlled sources (HTTP params, body, headers, file reads, env vars) to dangerous sinks (SQL, commands, HTML, file access, redirects, deserialization). Tracks variable assignments, detects sanitizers. Emits TAINT_FLOW edges.",
            p(true,false,false,json!({"category":{"type":"string","description":"Filter by sink category: SqlInjection, CommandInjection, XssRisk, PathTraversal, OpenRedirect, InsecureDeserialization, LdapInjection, XPathInjection (default: all)"},"show_sanitized":{"type":"boolean","default":false,"description":"Include sanitized (suppressed) flows in output"}})), &["path"]),
        tool_def("detect_interprocedural_taint", "Inter-procedural taint analysis. Traces taint across function call boundaries via CALLS graph edges. Finds source functions (HTTP input) that reach sink functions (SQL, commands, etc.) through call chains up to max_depth.",
            p(true,false,false,json!({"max_depth":{"type":"integer","default":5,"description":"Max call chain depth (default: 5)"},"category":{"type":"string","description":"Filter by sink category (default: all)"}})), &["path"]),
        tool_def("detect_dynamic_urls", "Detect dynamic URL construction in HTTP client calls. Finds fetch, axios, requests, HttpClient, etc. with string concatenation or template literals. Matches against known routes. Emits CALLS_SERVICE edges for matched URLs.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("detect_path_traversal", "Multi-layer path traversal detection. Combines intra and inter-procedural taint analysis focused on file path operations. Checks for sanitizers (realpath, canonicalize, secure_filename) across call chains.",
            p(true,false,false,json!({"max_depth":{"type":"integer","default":5,"description":"Max call chain depth for inter-procedural analysis (default: 5)"}})), &["path"]),
        tool_def("ingest_structured", "Ingest structured data (YAML/JSON) into the graph using plug-n-play TOML schemas. Schemas define node tables, columns, edges. Discovers schemas from .infigraph/structured-schemas/ and ~/.infigraph/structured-schemas/. Call without schema_id to list available schemas.",
            p(true,false,false,json!({"schema_id":{"type":"string","description":"Schema ID to use for ingestion"},"data_file":{"type":"string","description":"Path to .json or .yaml/.yml data file"},"data":{"type":"array","description":"Inline JSON array of records to ingest"}})), &["path"]),
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
        tool_def("get_doc_context", "PRIMARY: Full documentation context for a symbol — signature, docstring, source, callers, callees, file. One call replaces get_code_snippet + trace_callers + trace_callees. Use BEFORE modifying any function. Default returns compact summary (no source); set detail=true for full source.",
            p(true,true,false,json!({"detail":{"type":"boolean","default":false,"description":"If true, return full source code. Default (false) returns signature + callers/callees only."}})), &["path","symbol_id"]),
        tool_def("detect_clones", "PRIMARY: Find near-duplicate functions using vector similarity. Use to identify copy-paste code and refactoring opportunities. Stores SIMILAR_TO edges for later querying.",
            p(true,false,false,json!({"threshold":{"type":"number","default":0.92,"description":"Similarity threshold 0.0-1.0 (default: 0.92). Lower = more results but more false positives."},"limit":{"type":"integer","default":20,"description":"Max clone pairs to return"},"kinds":{"type":"string","default":"Function,Method","description":"Comma-separated symbol kinds to check (default: Function,Method)"},"store_edges":{"type":"boolean","default":true,"description":"Write SIMILAR_TO edges to graph for later querying"}})), &["path"]),
        tool_def("refactor", "PRIMARY: Analyze code for refactoring opportunities — file size, complexity hotspots, coupling (fan-in/fan-out), near-duplicate functions, dead code. Returns ranked recommendations with impact/effort scores. Use instead of manually running detect_clones + get_complexity + detect_dead_code separately.",
            p(true,false,false,json!({"target":{"type":"string","description":"File path or symbol name to analyze (default: whole project)"},"focus":{"type":"string","enum":["all","complexity","duplication","coupling","size"],"default":"all","description":"Focus area: all, complexity, duplication, coupling, size"},"limit":{"type":"integer","default":10,"description":"Max recommendations to return"}})), &["path"]),
        tool_def("git_summary", "PRIMARY: Symbol-level commit history. Use INSTEAD OF git log for understanding recent changes. Shows which functions were added/removed/modified per commit, not just file names.",
            p(true,false,false,json!({"n_commits":{"type":"integer","default":10,"description":"Number of recent commits to summarize (default: 10)"},"author":{"type":"string","description":"Optional: filter by author name/email"},"file":{"type":"string","description":"Optional: filter to a specific file path"}})), &["path"]),
        tool_def("list_files", "PRIMARY: List all source files in project. Use INSTEAD OF find/ls/glob for file discovery. Supports glob patterns (e.g. '*.rs', 'src/**').",
            p(true,false,false,json!({"glob":{"type":"string","description":"Optional glob pattern to filter files (e.g. '*.rs', 'src/**')"}})), &["path"]),
        tool_def("generate_test_context", "PRIMARY: Generate prioritized test generation context. Finds untested symbols, ranks by complexity and callers, includes example test as style reference, control-flow branches, source code, and framework-specific templates with conventions and scaffolds per test type (unit/integration/functional/e2e). Use to guide LLM test generation.",
            p(true,false,false,json!({"file":{"type":"string","description":"Optional: filter to symbols in files matching this substring"},"limit":{"type":"integer","default":10,"description":"Max number of target symbols to return (default: 10)"},"test_type":{"type":"string","description":"Optional: filter templates to a specific test type (unit, integration, functional, e2e). Omit to get all applicable templates."}})), &["path"]),
        tool_def("generate_sequence_diagram", "PRIMARY: Generate Mermaid sequence diagram from call graph. Use to visualize control flow through a function. Participants = files, messages = calls.",
            p(true,true,false,json!({"depth":{"type":"integer","default":3,"description":"Max call depth to traverse (default: 3)"}})), &["path","symbol_id"]),
        tool_def("save_session", "Save session context to a dedicated session DB for cross-session continuity. Stores Session node + semantic embedding. Multiple calls per day merge: summary/pending_tasks/constraints/assumptions/blockers overwrite, decisions append, files_touched union. Use `narrative` for full session story — written to .infigraph/sessions/session_YYYY-MM-DD.md and embedded for semantic search. Use `name` to save a named session that can be recalled later by identity.",
            p(true,false,false,json!({
                "summary":{"type":"string","description":"Brief summary of what was accomplished this session"},
                "name":{"type":"string","description":"Optional name/label for this session (e.g. 'perf-optimization', 'auth-refactor'). Named sessions are stored separately from daily auto-saves and can be recalled by name via get_latest_session."},
                "pending_tasks":{"type":"string","description":"Tasks remaining / next steps"},
                "decisions":{"type":"string","description":"Structured decisions: 'Goal: X. Decision: Y. Why: Z. Invalidates-if: W.' Use | to separate multiple decisions"},
                "files_touched":{"type":"string","description":"Comma-separated list of files modified"},
                "constraints":{"type":"string","description":"What was tried and failed: 'Tried: X. Failed because: Y. Do not retry unless: Z.'"},
                "assumptions":{"type":"string","description":"What current approach depends on: 'Assumes: X. If X changes: Y.'"},
                "blockers":{"type":"string","description":"Stuck items needing human input or external dependency"},
                "narrative":{"type":"string","description":"Full session story: what was explored, found, reasoned, decided, and why. Raw chronological dump. Appended to .infigraph/sessions/session_YYYY-MM-DD.md with timestamp. Use for rich context recovery in future sessions."}
            })), &["path","summary"]),
        tool_def("get_latest_session", "Retrieve recent session context from graph DB. Call at START of every new session to resume where you left off. Default: all sessions updated within 72h of the newest save (compact index). Use name to recall a named session in full. Use detail=true for full fields when multiple sessions match. Use limit for raw recent-N history.",
            p(true,false,false,json!({"limit":{"type":"integer","description":"Return N most recent sessions by update time (skips 72h window clustering)"},"name":{"type":"string","description":"Recall a named session by its label (e.g. 'perf-optimization'). Returns full detail for that session."},"detail":{"type":"boolean","default":false,"description":"When multiple sessions match, return full fields for each instead of compact index"}})), &["path"]),
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
        // Pipeline plugin tools
        tool_def("pipeline_plugins", "List loaded pipeline plugins and their configuration.",
            p(true,false,false,json!({})), &["path"]),
        tool_def("pipeline_deps", "List pipeline dependency edges (which pipelines feed into which).",
            p(true,false,false,json!({})), &["path"]),
        tool_def("pipeline_impact", "Transitive impact analysis: what pipelines are affected if a table/dataset changes.",
            p(true,false,false,json!({"table_name":{"type":"string","description":"Table or dataset name to analyze impact for"},"max_depth":{"type":"integer","default":3,"description":"Max traversal depth for transitive impact"}})), &["path","table_name"]),
        tool_def("pipeline_compliance", "Query pipelines by compliance scope (e.g. 'IRS 7216', 'PII', 'SOX').",
            p(true,false,false,json!({"scope":{"type":"string","description":"Compliance scope to search for"},"plugin_id":{"type":"string","description":"Plugin ID to query (default: 'intuit')"}})), &["path","scope"]),
        tool_def("pipeline_query", "Query a plugin-specific pipeline table by field value. Generic escape hatch for plugin-specific queries.",
            p(true,false,false,json!({"plugin_id":{"type":"string","description":"Pipeline plugin ID (e.g. 'intuit', 'dbt')"},"field":{"type":"string","description":"Column name to search"},"value":{"type":"string","description":"Value to match (case-insensitive contains)"}})), &["path","plugin_id","field","value"]),
        tool_def("memory_context", "LM2 output gate: Adaptive context assembly in one call. Searches code symbols (BM25+vector), sessions (semantic), and file skeletons. Ranks by relevance with L1/L2/L3 hierarchical depth. L1=anchor file symbols, L2=+callers/callees/deps, L3=full hybrid search. Auto-selects depth from query complexity. Replaces manual search+symbol_context+search_sessions chains.",
            p(true,false,false,json!({"query":{"type":"string","description":"What context is needed (natural language)"},"file":{"type":"string","description":"Optional anchor file — boosts symbols in/near this file, includes its skeleton"},"depth":{"type":"string","enum":["auto","L1","L2","L3"],"default":"auto","description":"Retrieval depth: L1=anchor file only, L2=+callers/callees/deps, L3=full hybrid search, auto=heuristic selection"},"limit":{"type":"integer","default":10,"description":"Max code results to return (default 10)"},"sources":{"type":"string","default":"code,sessions,skeleton","description":"Comma-separated source filter: code, sessions, skeleton"}})), &["path","query"]),
        tool_def("consolidate_memory", "LM2 memory update: Merges similar sessions into consolidated summaries. Groups by embedding similarity, creates merged session with combined decisions/constraints/assumptions. Source sessions preserved with reduced confidence. Run when session count grows large.",
            p(true,false,false,json!({"threshold":{"type":"number","default":0.7,"description":"Similarity threshold for grouping sessions (0.0-1.0, default 0.7)"}})), &["path"]),
        tool_def("compress", "Compress arbitrary text (JSON, logs, build output, stack traces, tables). Auto-detects content type and applies type-specific compression. Returns detected type, token savings, and compressed text.",
            json!({"text": {"type": "string", "description": "Text to compress"}}), &["text"]),
        tool_def("get_compression_stats", "Show compression metrics for the current session: compression level, token budget usage, dedup entries, call count.",
            json!({}), &[]),
    ]
}

pub fn mcp_log(level: &str, msg: &str) {
    use std::io::Write;
    let path = mcp_log_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {level}: {msg}");
    }
}

fn mcp_log_file_path() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".infigraph")
        .join("mcp.log")
}

pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    ((words as f64) * 1.4).ceil() as usize
}

pub fn handle_initialize(id: &Value, is_primary: bool) -> Value {
    mcp_log("INFO", &format!("initialize called (primary={is_primary})"));
    if is_primary {
        std::thread::spawn(|| {
            mcp_log("DEBUG", "init_watchers start");
            tools::watch::init_watchers();
            mcp_log("DEBUG", "init_doc_watchers start");
            tools::docs::init_doc_watchers();

            let registry = match infigraph_core::multi::Registry::load() {
                Ok(r) => {
                    mcp_log(
                        "DEBUG",
                        &format!("registry loaded: {} repos", r.repos.len()),
                    );
                    r
                }
                Err(e) => {
                    mcp_log("ERROR", &format!("registry load failed: {e}"));
                    return;
                }
            };

            for entry in registry.repos.values() {
                if !entry.path.join(".infigraph").exists() {
                    continue;
                }
                let path = entry.path.to_string_lossy().to_string();
                tools::watch::auto_start_watch(&path);
                tools::docs::auto_start_doc_watch(&path);
            }
        });
    } else {
        mcp_log("INFO", "Skipping watchers — not primary instance");
    }

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

pub fn handle_tools_list(id: &Value) -> Value {
    let tools = build_tools_list();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools
        }
    })
}

pub fn handle_tools_call(id: &Value, request: &Value) -> Value {
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    tools::helpers::log_activity(tool_name, &args);

    let metrics_enabled = std::env::var("INFIGRAPH_METRICS").is_ok_and(|v| v == "1");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch_tool(tool_name, &args)
    }));

    match result {
        Ok(Ok(content)) => {
            let detail_requested = args
                .get("detail")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            session_context::record_tool_call(tool_name, detail_requested);
            session_context::record_focus(tool_name, &args);
            let compressed = compress::compress_pipeline_safe(&content, tool_name, &args);
            let comp_tokens = estimate_tokens(&compressed);
            let level_used = session_context::get_compression_level();
            session_context::track_tokens(comp_tokens);
            if metrics_enabled {
                let raw_tokens = estimate_tokens(&content);
                let ratio = if raw_tokens > 0 {
                    comp_tokens as f64 / raw_tokens as f64
                } else {
                    1.0
                };
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let args_summary = args
                    .get("query")
                    .or_else(|| args.get("symbol"))
                    .or_else(|| args.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let detail = args
                    .get("detail")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let line = json!({
                    "tool": tool_name,
                    "timestamp": ts,
                    "raw_tokens": raw_tokens,
                    "compressed_tokens": comp_tokens,
                    "compression_ratio": (ratio * 100.0).round() / 100.0,
                    "compression_level": format!("{:?}", level_used),
                    "detail_requested": detail,
                    "args_summary": args_summary,
                });
                if let Some(dir) = args
                    .get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| std::path::PathBuf::from(p).join(".infigraph"))
                    .or_else(|| {
                        std::env::var("HOME")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".infigraph"))
                    })
                {
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join("compression_metrics.jsonl");
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                    {
                        use std::io::Write;
                        let _ = writeln!(f, "{}", line);
                    }
                }
            }
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": compressed }]
                }
            })
        }
        Ok(Err(e)) => {
            mcp_log("ERROR", &format!("tool={tool_name} err={e}"));
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("Error: {e}") }],
                    "isError": true
                }
            })
        }
        Err(panic_info) => {
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            mcp_log("ERROR", &format!("tool={tool_name} PANIC: {msg}"));
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("Error: panic in {tool_name}: {msg}") }],
                    "isError": true
                }
            })
        }
    }
}
