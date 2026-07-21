mod agent;
mod analysis_commands;
mod commands;
mod config_targets;
mod git_commands;
mod graph_commands;
mod group_commands;
mod hooks;
mod index;
mod info_commands;
mod install;
mod pipeline_commands;
mod scip_download;
mod search_commands;
mod viz_commands;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use infigraph_core::lang::LanguageRegistry;
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use agent::cmd_init;
use commands::*;
use group_commands::{cmd_group, cmd_repos};
use index::cmd_index;
use install::{cmd_install, cmd_uninstall, cmd_update};

/// Build a language registry with bundled languages + grammar plugins.
/// Grammar plugins are loaded from `~/.infigraph/grammars/` and `<project>/grammars/`.
fn full_registry(project_root: Option<&Path>) -> Result<LanguageRegistry> {
    let mut registry = bundled_registry()?;
    let project_grammars = project_root.map(|r| r.join("grammars"));
    if let Err(e) = infigraph_grammar_plugin::register_grammar_plugins(
        &mut registry,
        project_grammars.as_deref(),
        project_root,
    ) {
        eprintln!("[infigraph] Warning: failed to load grammar plugins: {e}");
    }
    Ok(registry)
}

#[derive(Parser)]
#[command(
    name = "infigraph",
    version,
    about = "AST-powered code analysis and impact review"
)]
struct Cli {
    /// Project root directory (defaults to current directory)
    #[arg(short, long)]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize infigraph in the current project
    Init {
        /// Associate with a repo group (writes multi-repo instructions for agents)
        #[arg(long)]
        group: Option<String>,
        /// Skip the interactive wizard (just write agent instructions)
        #[arg(long)]
        quick: bool,
        /// Auto-accept all prompts (non-interactive)
        #[arg(long, short)]
        yes: bool,
    },

    /// Parse all files and build the code graph
    Index {
        /// Clean .infigraph and rebuild from scratch
        #[arg(long)]
        full: bool,
        /// Skip embedding generation (faster, disables semantic search)
        #[arg(long)]
        no_embed: bool,
    },

    /// Show graph statistics
    Stats,

    /// List available languages
    Languages,

    /// Show symbols extracted from a file
    Symbols {
        /// File to inspect
        file: String,
    },

    /// Compact annotated file skeleton (signatures + complexity + fan-in)
    Skeleton {
        /// File to inspect
        file: String,
    },

    /// Ingest structured data using plug-n-play TOML schemas
    Ingest {
        /// Schema ID (omit to list available schemas)
        #[arg(long)]
        schema: Option<String>,
        /// Path to JSON or YAML data file
        #[arg(long)]
        data_file: Option<String>,
        /// Directory containing JSON/YAML data files
        #[arg(long)]
        source: Option<String>,
    },

    /// Run a raw Cypher query against the graph
    Query {
        /// Cypher query string
        cypher: String,
    },

    /// BM25 text search over indexed symbols
    Search {
        /// Search query
        query: String,

        /// Max results to return
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,

        /// Balance between BM25 (0.0) and vector (1.0)
        #[arg(short, long, default_value = "0.3")]
        alpha: f32,
    },

    /// Show all direct callers of a symbol
    Callers {
        /// Symbol ID (e.g. "auth.py::authenticate")
        symbol: String,
    },

    /// Show all symbols called by a given symbol
    Callees {
        /// Symbol ID
        symbol: String,
    },

    /// Detect potentially dead code (functions/methods with no callers)
    DeadCode,

    /// Show transitive impact of changing a symbol
    Impact {
        /// Symbol ID (e.g., "auth.py::authenticate")
        symbol: String,

        /// Max traversal depth
        #[arg(short, long, default_value = "5")]
        depth: u32,
    },

    /// Install infigraph MCP server config for AI coding agents
    Install,

    /// Uninstall infigraph MCP server config from AI coding agents
    Uninstall,

    /// Benchmark bulk write strategies (dev use)
    #[command(hide = true)]
    Bench {
        #[arg(long, default_value = "134000")]
        n: usize,
    },

    /// Benchmark Parquet vs UNWIND with real data (dev use)
    #[command(hide = true)]
    BenchParquet,

    /// Update infigraph — downloads latest binary and re-registers MCP configs
    Update,

    /// Manage repository groups for multi-repo/microservice analysis
    Group {
        #[command(subcommand)]
        action: GroupAction,
    },

    /// List all registered repositories
    Repos,

    /// Grep-like text search across project files
    SearchCode {
        /// Regex pattern to search for
        pattern: String,

        /// Optional glob filter for file paths (e.g., "*.rs", "**/*.py")
        #[arg(short = 'f', long)]
        file_pattern: Option<String>,

        /// Max results to return
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
    },

    /// Retrieve source code for a symbol by its ID
    Snippet {
        /// Symbol ID (e.g., "auth.py::authenticate")
        symbol_id: String,
    },

    /// Show codebase architecture overview (language breakdown, hotspots, hubs, entry points)
    Architecture,

    /// Detect symbols affected by uncommitted or recent git changes
    DetectChanges {
        /// Git ref to diff against (default: HEAD)
        #[arg(short, long, default_value = "HEAD")]
        base: String,

        /// Max traversal depth for blast radius
        #[arg(short, long, default_value = "3")]
        depth: u32,
    },

    /// Detect functional modules via Louvain community detection on the call graph
    Cluster,

    /// Export the code graph in various formats
    Export {
        /// Output format: cypher, graphml, or json
        format: String,

        /// Write to file instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Generate an interactive HTML graph visualization using vis.js
    #[command(alias = "viz")]
    Visualize,

    /// Generate a focused subgraph visualization centered on one symbol
    #[command(alias = "viz-sym")]
    VisualizeSymbol {
        /// Symbol ID (e.g. "src/auth.py::authenticate")
        symbol_id: String,
        /// Hop depth from the symbol
        #[arg(short, long, default_value = "2")]
        depth: u32,
    },

    /// Detect HTTP routes/endpoints from indexed code (Flask, Express, Spring, etc.)
    Routes,

    /// Import a SCIP index.scip file to enrich the graph with compiler-grade symbols
    ScipImport {
        /// Path to the index.scip file
        #[arg(short = 'i', long, default_value = "index.scip")]
        index: PathBuf,
    },

    /// Watch project for file changes and auto-reindex
    Watch {
        /// Debounce interval in milliseconds
        #[arg(short, long, default_value = "500")]
        debounce: u64,
    },

    /// Stop the background auto-watcher
    WatchStop,

    /// Check if a background watcher is running
    WatchStatus,

    /// Index documents (Markdown, PDF, DOCX, TXT, RST, HTML, etc.) into a document graph
    IndexDocs,

    /// Force full document reindex from scratch
    ReindexDocs,

    /// Delete document index and embeddings
    CleanDocs,

    /// Search indexed documents by meaning or keywords
    SearchDocs {
        /// Search query (natural language or keywords)
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },

    /// Index Confluence pages into the document graph (fetch, parse, chunk, embed)
    IndexConfluence {
        /// Confluence base URL (e.g. https://confluence.example.com)
        #[arg(long)]
        base_url: String,
        /// Confluence space key to index
        #[arg(long)]
        space: String,
        /// Specific page IDs to index (comma-separated). If omitted, indexes entire space.
        #[arg(long, value_delimiter = ',')]
        page_ids: Option<Vec<String>>,
        /// Personal Access Token for authentication
        #[arg(long, env = "CONFLUENCE_PAT")]
        pat: Option<String>,
        /// Email for basic auth (used with --api-token)
        #[arg(long, env = "CONFLUENCE_EMAIL")]
        email: Option<String>,
        /// API token for basic auth (used with --email)
        #[arg(long, env = "CONFLUENCE_API_TOKEN")]
        api_token: Option<String>,
        /// Follow links found in pages and crawl linked pages
        #[arg(long)]
        follow_links: bool,
        /// Max depth when following links (default: 1)
        #[arg(long, default_value = "1")]
        follow_depth: usize,
        /// Max total pages to crawl (default: 100)
        #[arg(long, default_value = "100")]
        max_pages: usize,
    },

    /// Parse package manifests and index dependencies into the graph
    IndexManifests,

    /// List all external dependencies discovered from manifests
    #[command(alias = "deps")]
    Dependencies {
        /// Filter by ecosystem (npm, cargo, pip, maven, gem, nuget, go, composer, pub)
        #[arg(short, long)]
        ecosystem: Option<String>,
    },

    /// Find every reference location for a symbol (for safe rename/refactor)
    #[command(alias = "refs")]
    FindRefs {
        /// Symbol ID (e.g. "auth.py::authenticate")
        symbol: String,
    },

    /// Show the public API surface: all public symbols and HTTP routes
    #[command(alias = "api")]
    ApiSurface {
        /// Optional file filter
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Show file-level import dependencies (what this file imports and what imports it)
    FileDeps {
        /// Relative file path (e.g. "src/auth.py")
        file: String,
    },

    /// Show full type inheritance hierarchy for a class or interface
    #[command(alias = "hierarchy")]
    TypeHierarchy {
        /// Symbol ID of the class or interface
        symbol: String,
        /// Max hierarchy depth
        #[arg(short, long, default_value = "5")]
        depth: u32,
    },

    /// Show test coverage: which symbols have tests and which don't
    #[command(alias = "coverage")]
    TestCoverage {
        /// Optional file filter
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Scan for security vulnerabilities (SQL injection, hardcoded secrets, eval, pickle, weak crypto, etc.)
    #[command(alias = "sec")]
    Security {
        /// Filter by severity: CRITICAL, HIGH, MEDIUM, LOW
        #[arg(short, long)]
        severity: Option<String>,
        /// Filter by category: SqlInjection, HardcodedSecret, WeakCrypto, etc.
        #[arg(short, long)]
        category: Option<String>,
    },

    /// Show cyclomatic complexity for all functions/methods
    #[command(alias = "cx")]
    Complexity {
        /// Flag symbols at or above this threshold (default: 10)
        #[arg(short, long, default_value = "10")]
        threshold: u32,
        /// Optional file filter
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Detect near-duplicate functions via vector similarity
    Clones {
        /// Similarity threshold (0.0-1.0, default 0.92)
        #[arg(short, long, default_value = "0.92")]
        threshold: f64,
        /// Max clone pairs to return
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Detect cross-cutting concerns from annotations/decorators
    #[command(alias = "xcut")]
    Concerns {
        /// Filter: Authorization, Validation, Caching, Transaction, etc.
        #[arg(short, long)]
        kind: Option<String>,
    },

    /// Detect config-driven conditional resolution (@Profile, process.env, etc.)
    #[command(alias = "config")]
    ConfigBindings {
        /// Filter: Profile, Qualifier, Environment, etc.
        #[arg(short, long)]
        kind: Option<String>,
        /// Filter by profile name
        #[arg(short, long)]
        profile: Option<String>,
    },

    /// Symbol-level diff between two git refs (added/removed/signature-changed/moved symbols)
    #[command(alias = "sdiff")]
    SemanticDiff {
        /// Old git ref
        #[arg(long, default_value = "HEAD~1")]
        old: String,
        /// New git ref
        #[arg(long, default_value = "HEAD")]
        new: String,
    },

    /// Generate a Mermaid sequence diagram from the call graph rooted at a symbol
    #[command(alias = "seq")]
    Sequence {
        /// Symbol ID (e.g. "src/main.rs::main")
        symbol_id: String,
        /// Max call depth to traverse
        #[arg(short, long, default_value = "3")]
        depth: u32,
    },

    /// Analyze code for refactoring opportunities
    Refactor {
        /// File path or symbol name to analyze (default: whole project)
        #[arg(short, long)]
        target: Option<String>,
        /// Focus area: all, complexity, duplication, coupling, size
        #[arg(short, long, default_value = "all")]
        focus: String,
        /// Max recommendations
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },

    /// Detect reflection/dynamic invocation sites
    Reflection {
        /// Filter: ClassForName, ServiceLoader, Getattr, etc.
        #[arg(short, long)]
        mechanism: Option<String>,
    },

    /// Taint analysis: trace data from sources to sinks
    Taint {
        /// Filter: SqlInjection, CommandInjection, XssRisk, PathTraversal, etc.
        #[arg(short, long)]
        category: Option<String>,
        /// Include sanitized flows
        #[arg(long)]
        show_sanitized: bool,
        /// Run inter-procedural analysis (follows call chains)
        #[arg(long)]
        inter: bool,
        /// Max call chain depth for inter-procedural (default: 5)
        #[arg(short, long, default_value = "5")]
        depth: u32,
    },

    /// Detect dynamic URL construction in HTTP clients
    DynamicUrls,

    /// Multi-layer path traversal detection
    PathTraversal {
        /// Max call chain depth (default: 5)
        #[arg(short, long, default_value = "5")]
        depth: u32,
    },

    /// Run CI checks (security, complexity, dead-code) against configurable thresholds
    #[command(alias = "ci")]
    Check {
        /// Path to check config TOML (default: .infigraph/check.toml)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Output results as JSON
        #[arg(long)]
        json: bool,
        /// Comma-separated list of checks to run: security,complexity,dead-code (default: all)
        #[arg(long = "check")]
        checks: Option<String>,
    },

    /// PR review: changed symbols + blast radius + affected tests + API changes + security
    #[command(alias = "pr")]
    Review {
        /// Git ref to diff against (default: HEAD~1)
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        /// Max blast-radius results per symbol
        #[arg(long, default_value = "1000")]
        limit: usize,
        /// Output as JSON instead of Markdown
        #[arg(long)]
        json: bool,
        /// Enable LLM-augmented review via Claude API (requires ANTHROPIC_API_KEY)
        #[arg(long)]
        llm: bool,
        /// Print the LLM prompt without calling the API
        #[arg(long)]
        dry_run: bool,
        /// PR context/intent (e.g. "bug fix for auth timeout", "refactor payment module")
        #[arg(long)]
        context: Option<String>,
        /// Cross-repo review: use a group name for blast radius across repos
        #[arg(long)]
        group: Option<String>,
    },

    /// Scan dependencies for known vulnerabilities via the OSV database
    #[command(alias = "vuln")]
    Vulns {
        /// Minimum severity to show: CRITICAL, HIGH, MEDIUM, LOW (default: all)
        #[arg(short, long)]
        severity: Option<String>,
        /// Filter by ecosystem (npm, cargo, pip, maven, gem, nuget, go, composer, pub)
        #[arg(short, long)]
        ecosystem: Option<String>,
        /// Output results as JSON
        #[arg(long)]
        json: bool,
    },

    /// Clear all learned resolution patterns (from SCIP corrections)
    Forget,

    /// Detect cross-language boundaries (FFI, JNI, gRPC, etc.)
    #[command(alias = "ffi")]
    Bridges {
        /// Filter by kind: FFI, JNI, CGO, GRPC, P_INVOKE, CTYPES, WASM, COM
        #[arg(short, long)]
        kind: Option<String>,
    },

    /// Promote BRIDGE_TO edges to CALLS edges where both endpoints are resolved symbols
    #[command(alias = "promote-bridges")]
    BridgesPromote,

    /// Detect design patterns (Factory, Singleton, Observer, Strategy, Decorator)
    #[command(alias = "patterns")]
    DetectPatterns {
        /// Filter by pattern type (factory, singleton, observer, strategy, decorator)
        #[arg(short, long)]
        pattern: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Capture quality baseline or compare against stored baseline
    #[command(alias = "bq")]
    BenchQuality {
        /// Save current metrics as the new baseline
        #[arg(long)]
        save: bool,
    },

    /// Background SCIP enrichment (spawned by index)
    #[command(hide = true)]
    ScipEnrich {
        /// Comma-separated detected languages
        languages: String,
    },

    /// Remove cached SCIP runtime binaries (Node.js, JRE, .NET SDK, Dart SDK, PHP)
    CleanRuntimes,

    /// Symbol-level commit history (which functions were added/removed/modified per commit)
    #[command(alias = "git")]
    GitSummary {
        /// Number of recent commits to analyze
        #[arg(short, long, default_value = "10")]
        commits: usize,
        /// Filter by commit author
        #[arg(short, long)]
        author: Option<String>,
        /// Filter by file path
        #[arg(short, long)]
        file: Option<String>,
    },

    /// List indexed files (optionally filtered by glob pattern)
    Files {
        /// Glob pattern (e.g. "*.rs", "src/**/*.py")
        #[arg(short, long)]
        glob: Option<String>,
    },

    /// Generate focused test context for a file (symbols, callers, coverage)
    TestContext {
        /// File to generate test context for
        #[arg(short, long)]
        file: Option<String>,
        /// Max symbols to include
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Delete the project's .infigraph data and deregister from repo list
    Delete,

    /// Pipeline analysis (plugins, deps, impact, compliance, query)
    Pipeline {
        #[command(subcommand)]
        action: PipelineAction,
    },

    /// LM2 adaptive context: ranked code + sessions + skeleton in one query
    #[command(alias = "mc")]
    MemoryContext {
        /// Natural language query
        query: String,
        /// Anchor file (boosts nearby symbols)
        #[arg(short, long)]
        file: Option<String>,
        /// Depth tier: auto, L1, L2, L3
        #[arg(short, long, default_value = "auto")]
        depth: String,
        /// Sources to include: code,sessions,skeleton
        #[arg(short, long, default_value = "code,sessions,skeleton")]
        sources: String,
        /// Max code results
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
    },

    /// Consolidate similar sessions + build symbol clusters + purge expired
    #[command(alias = "consolidate")]
    ConsolidateMemory {
        /// Cosine similarity threshold for merging (0.0-1.0)
        #[arg(short, long, default_value = "0.7")]
        threshold: f64,
    },

    /// Delete sessions older than N days
    PurgeSessions {
        /// Delete sessions older than this many days
        #[arg(short, long, default_value = "30")]
        days: u32,
    },
}

#[derive(Subcommand)]
pub(crate) enum GroupAction {
    /// Create a new repository group
    Create {
        name: String,
        /// Organization scope (prevents group name collisions). Defaults to INFIGRAPH_ORG env var.
        #[arg(long)]
        org: Option<String>,
    },
    /// Add a repository to a group
    Add {
        group: String,
        /// Name to register this repo as
        repo: String,
    },
    /// Remove a repository from a group
    Remove { group: String, repo: String },
    /// List all groups and their repos
    List,
    /// Index (or reindex) all repos in a group
    Index {
        group: String,
        /// Clean .infigraph and rebuild from scratch
        #[arg(long)]
        full: bool,
    },
    /// Build (or rebuild) the combined graph for a group
    Combined { group: String },
    /// Build (or rebuild) the physical combined document store for a group
    CombinedDocs { group: String },
    /// Extract and sync contracts across repos in a group
    Sync { group: String },
    /// Show contracts discovered in a group
    Contracts { group: String },
    /// Detect cross-service HTTP dependencies within a group
    Deps { group: String },
    /// Link cross-service dependencies as CALLS_SERVICE edges in caller graphs
    Link { group: String },
    /// Run a Cypher query across all repos in a group
    Query {
        group: String,
        cypher: String,
        /// Query the combined graph instead of individual repos
        #[arg(long)]
        combined: bool,
    },
    /// Full rebuild: index → sync → link → combined (all steps in one command)
    Build {
        group: String,
        /// Force full reindex (wipe .infigraph/ per repo)
        #[arg(long)]
        full: bool,
    },
    /// Hybrid BM25+vector search across the combined graph
    Search {
        group: String,
        /// Search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "20")]
        limit: usize,
        /// BM25/vector blend (0.0 = pure BM25, 1.0 = pure vector)
        #[arg(short, long, default_value = "0.3")]
        alpha: f32,
        /// Deep mode: enrich results with cross-repo graph context for LLM reasoning
        #[arg(long)]
        deep: bool,
    },
    /// Hybrid BM25+vector search across the combined document store
    SearchDocs {
        group: String,
        /// Document search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// BM25/vector blend (0.0 = pure BM25, 1.0 = pure vector)
        #[arg(short, long, default_value = "0.5")]
        alpha: f32,
    },
    /// Watch all repos in a group for changes, auto-reindex and rebuild combined graph
    Watch {
        group: String,
        /// Debounce interval in milliseconds
        #[arg(short, long, default_value = "500")]
        debounce: u64,
    },
}

#[derive(Subcommand)]
pub(crate) enum PipelineAction {
    /// List discovered pipeline plugins
    Plugins,
    /// Show pipeline dependency graph
    Deps,
    /// Impact analysis: what pipelines are affected by changing a table
    Impact {
        /// Table name to analyze
        table: String,
        /// Max traversal depth
        #[arg(short, long, default_value = "5")]
        depth: u32,
    },
    /// Check pipeline compliance against a plugin's schema
    Compliance {
        /// Scope to check (e.g. "production")
        scope: String,
        /// Plugin ID to check against
        plugin_id: String,
    },
    /// Query pipeline metadata by plugin field value
    Query {
        /// Plugin ID
        plugin_id: String,
        /// Field to match
        field: String,
        /// Value to match
        value: String,
    },
}

fn main() -> Result<()> {
    // ANTLR parsers recurse deeply; Rayon's default 2MB stack overflows.
    // Windows default main-thread stack is 1MB — also too small.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global();

    let update_handle = install::check_for_update_background();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));

    let should_auto_watch = !matches!(
        cli.command,
        Commands::Watch { .. }
            | Commands::WatchStop
            | Commands::WatchStatus
            | Commands::ScipEnrich { .. }
            | Commands::Delete
            | Commands::Update
            | Commands::Install
            | Commands::Uninstall
            | Commands::Init { .. }
            | Commands::Languages
            | Commands::Repos
            | Commands::CleanRuntimes
    );

    let result = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            if should_auto_watch {
                index::ensure_watcher_running(&root);
            }
            run(cli.command, &root)
        })
        .expect("failed to spawn main worker thread")
        .join()
        .expect("main worker thread panicked");

    install::print_update_hint(update_handle);

    result
}

fn run(command: Commands, root: &Path) -> Result<()> {
    match command {
        Commands::Init { group, quick, yes } => cmd_init(root, group.as_deref(), quick, yes),
        Commands::Index { full, no_embed } => cmd_index(root, full, no_embed),
        Commands::Stats => cmd_stats(root),
        Commands::Languages => cmd_languages(Some(root)),
        Commands::Symbols { file } => cmd_symbols(root, &file),
        Commands::Skeleton { file } => cmd_skeleton(root, &file),
        Commands::Ingest {
            schema,
            data_file,
            source,
        } => cmd_ingest(
            root,
            schema.as_deref(),
            data_file.as_deref(),
            source.as_deref(),
        ),
        Commands::Query { cypher } => cmd_query(root, &cypher),
        Commands::Search {
            query,
            limit,
            alpha,
        } => cmd_search(root, &query, limit, alpha),
        Commands::Callers { symbol } => cmd_callers(root, &symbol),
        Commands::Callees { symbol } => cmd_callees(root, &symbol),
        Commands::DeadCode => cmd_dead_code(root),
        Commands::Impact { symbol, depth } => cmd_impact(root, &symbol, depth),
        Commands::Install => cmd_install(),
        Commands::Uninstall => cmd_uninstall(),
        Commands::Bench { n } => {
            let registry = bundled_registry()?;
            let mut prism = Infigraph::open(root, registry)?;
            prism.init()?;
            let store = prism.store().context("not initialized")?;
            store.test_parquet_quality()?;
            store.benchmark_bulk_write(n)
        }
        Commands::BenchParquet => {
            let registry = bundled_registry()?;
            let mut prism = Infigraph::open(root, registry)?;
            prism.init()?;
            let store = prism.store().context("not initialized")?;
            store.benchmark_parquet_vs_csv()
        }
        Commands::Update => cmd_update(),
        Commands::Group { action } => cmd_group(root, action),
        Commands::Repos => cmd_repos(),
        Commands::SearchCode {
            pattern,
            file_pattern,
            limit,
        } => cmd_search_code(root, &pattern, file_pattern.as_deref(), limit),
        Commands::Snippet { symbol_id } => cmd_snippet(root, &symbol_id),
        Commands::Architecture => cmd_architecture(root),
        Commands::DetectChanges { base, depth } => cmd_detect_changes(root, &base, depth),
        Commands::Cluster => cmd_cluster(root),
        Commands::Export { format, output } => cmd_export(root, &format, output),
        Commands::Visualize => cmd_visualize(root),
        Commands::VisualizeSymbol { symbol_id, depth } => {
            cmd_visualize_symbol(root, &symbol_id, depth)
        }
        Commands::Routes => cmd_routes(root),
        Commands::ScipImport { index } => cmd_scip_import(root, &index),
        Commands::Watch { debounce } => cmd_watch(root, debounce),
        Commands::WatchStop => cmd_watch_stop(root),
        Commands::WatchStatus => cmd_watch_status(root),
        Commands::IndexDocs => cmd_index_docs(root),
        Commands::ReindexDocs => cmd_reindex_docs(root),
        Commands::CleanDocs => cmd_clean_docs(root),
        Commands::SearchDocs { query, limit } => cmd_search_docs(root, &query, limit),
        Commands::IndexConfluence {
            base_url,
            space,
            page_ids,
            pat,
            email,
            api_token,
            follow_links,
            follow_depth,
            max_pages,
        } => cmd_index_confluence(
            root,
            &base_url,
            &space,
            page_ids,
            pat,
            email,
            api_token,
            follow_links,
            follow_depth,
            max_pages,
        ),
        Commands::IndexManifests => cmd_index_manifests(root),
        Commands::Dependencies { ecosystem } => cmd_dependencies(root, ecosystem.as_deref()),
        Commands::FindRefs { symbol } => cmd_find_refs(root, &symbol),
        Commands::ApiSurface { file } => cmd_api_surface(root, file.as_deref()),
        Commands::FileDeps { file } => cmd_file_deps(root, &file),
        Commands::TypeHierarchy { symbol, depth } => cmd_type_hierarchy(root, &symbol, depth),
        Commands::TestCoverage { file } => cmd_test_coverage(root, file.as_deref()),
        Commands::Security { severity, category } => {
            cmd_security(root, severity.as_deref(), category.as_deref())
        }
        Commands::Complexity { threshold, file } => {
            cmd_complexity(root, threshold, file.as_deref())
        }
        Commands::Clones { threshold, limit } => cmd_clones(root, threshold, limit),
        Commands::Concerns { kind } => cmd_concerns(root, kind.as_deref()),
        Commands::ConfigBindings { kind, profile } => {
            cmd_config_bindings(root, kind.as_deref(), profile.as_deref())
        }
        Commands::SemanticDiff { old, new } => cmd_semantic_diff(root, &old, &new),
        Commands::Sequence { symbol_id, depth } => cmd_sequence(root, &symbol_id, depth),
        Commands::Refactor {
            target,
            focus,
            limit,
        } => cmd_refactor(root, target.as_deref(), &focus, limit),
        Commands::Reflection { mechanism } => cmd_reflection(root, mechanism.as_deref()),
        Commands::Taint {
            category,
            show_sanitized,
            inter,
            depth,
        } => cmd_taint(root, category.as_deref(), show_sanitized, inter, depth),
        Commands::DynamicUrls => cmd_dynamic_urls(root),
        Commands::PathTraversal { depth } => cmd_path_traversal(root, depth),
        Commands::Check {
            config,
            json,
            checks,
        } => {
            let any_failed = cmd_check(root, config.as_deref(), json, checks.as_deref())?;
            if any_failed {
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::Review {
            base,
            limit,
            json,
            llm,
            dry_run,
            context,
            group,
        } => cmd_review(
            root,
            &base,
            limit,
            json,
            llm,
            dry_run,
            context.as_deref(),
            group.as_deref(),
        ),
        Commands::Vulns {
            severity,
            ecosystem,
            json,
        } => cmd_vulns(root, severity.as_deref(), ecosystem.as_deref(), json),
        Commands::Forget => cmd_forget(root),
        Commands::Bridges { kind } => cmd_bridges(root, kind.as_deref()),
        Commands::BridgesPromote => cmd_bridges_promote(root),
        Commands::DetectPatterns { pattern, json } => {
            cmd_detect_patterns(root, pattern.as_deref(), json)
        }
        Commands::ScipEnrich { languages } => {
            let detected: std::collections::HashSet<String> = languages
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            index::cmd_scip_enrich(root, &detected);
            Ok(())
        }
        Commands::CleanRuntimes => {
            scip_download::clean_runtimes();
            println!("All cached SCIP runtimes removed.");
            Ok(())
        }
        Commands::BenchQuality { save } => {
            let registry = bundled_registry()?;
            let mut prism = infigraph_core::Infigraph::open(root, registry)?;
            prism.init()?;
            let backend = prism
                .backend()
                .context("graph not initialized -- run 'infigraph index' first")?;

            let current = infigraph_core::bench::QualityMetrics::capture(root, backend)?;

            if save {
                infigraph_core::bench::save_baseline(root, &current)?;
                println!("Baseline saved to .infigraph/quality_baseline.json");
                println!("{}", current.format());
            } else {
                match infigraph_core::bench::load_baseline(root) {
                    Some(baseline) => {
                        let results = infigraph_core::bench::compare(&baseline.metrics, &current);
                        print!("{}", infigraph_core::bench::format_comparison(&results));
                        let regressions = results.iter().filter(|r| r.regression).count();
                        if regressions > 0 {
                            println!("\n  {} regression(s) detected!", regressions);
                            std::process::exit(1);
                        } else {
                            println!("\n  No regressions detected.");
                        }
                    }
                    None => {
                        println!("No baseline found. Run with --save to create one:");
                        println!("  infigraph bench-quality --save");
                        println!("{}", current.format());
                    }
                }
            }
            Ok(())
        }
        Commands::GitSummary {
            commits,
            author,
            file,
        } => cmd_git_summary(root, commits, author.as_deref(), file.as_deref()),
        Commands::Files { glob } => cmd_list_files(root, glob.as_deref()),
        Commands::TestContext { file, limit } => {
            cmd_generate_test_context(root, file.as_deref(), limit)
        }
        Commands::Delete => cmd_delete_project(root),
        Commands::MemoryContext {
            query,
            file,
            depth,
            sources,
            limit,
        } => cmd_memory_context(root, &query, file.as_deref(), &depth, &sources, limit),
        Commands::ConsolidateMemory { threshold } => cmd_consolidate_memory(root, threshold),
        Commands::PurgeSessions { days } => cmd_purge_sessions(root, days),
        Commands::Pipeline { action } => match action {
            PipelineAction::Plugins => cmd_pipeline_plugins(root),
            PipelineAction::Deps => cmd_pipeline_deps(root),
            PipelineAction::Impact { table, depth } => cmd_pipeline_impact(root, &table, depth),
            PipelineAction::Compliance { scope, plugin_id } => {
                cmd_pipeline_compliance(root, &scope, &plugin_id)
            }
            PipelineAction::Query {
                plugin_id,
                field,
                value,
            } => cmd_pipeline_query(root, &plugin_id, &field, &value),
        },
    }
}
