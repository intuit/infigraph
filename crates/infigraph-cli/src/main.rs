mod agent;
mod analysis_commands;
mod commands;
mod config_targets;
mod graph_commands;
mod group_commands;
mod hooks;
mod index;
mod info_commands;
mod install;
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
        /// Confluence base URL (e.g. https://wiki.cloud.intuit.com)
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
}

#[derive(Subcommand)]
pub(crate) enum GroupAction {
    /// Create a new repository group
    Create { name: String },
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
    /// Extract and sync contracts across repos in a group
    Sync { group: String },
    /// Show contracts discovered in a group
    Contracts { group: String },
    /// Detect cross-service HTTP dependencies within a group
    Deps { group: String },
    /// Link cross-service dependencies as CALLS_SERVICE edges in caller graphs
    Link { group: String },
    /// Run a Cypher query across all repos in a group
    Query { group: String, cypher: String },
    /// Watch all repos in a group for changes, auto-reindex and rebuild combined graph
    Watch {
        group: String,
        /// Debounce interval in milliseconds
        #[arg(short, long, default_value = "500")]
        debounce: u64,
    },
}

fn main() -> Result<()> {
    // ANTLR parsers recurse deeply; Rayon's default 2MB stack overflows.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));

    match cli.command {
        Commands::Init { group, quick, yes } => cmd_init(&root, group.as_deref(), quick, yes),
        Commands::Index { full, no_embed } => cmd_index(&root, full, no_embed),
        Commands::Stats => cmd_stats(&root),
        Commands::Languages => cmd_languages(Some(&root)),
        Commands::Symbols { file } => cmd_symbols(&root, &file),
        Commands::Query { cypher } => cmd_query(&root, &cypher),
        Commands::Search {
            query,
            limit,
            alpha,
        } => cmd_search(&root, &query, limit, alpha),
        Commands::DeadCode => cmd_dead_code(&root),
        Commands::Impact { symbol, depth } => cmd_impact(&root, &symbol, depth),
        Commands::Install => cmd_install(),
        Commands::Uninstall => cmd_uninstall(),
        Commands::Bench { n } => {
            let registry = bundled_registry()?;
            let mut prism = Infigraph::open(&root, registry)?;
            prism.init()?;
            let store = prism.store().context("not initialized")?;
            store.test_parquet_quality()?;
            store.benchmark_bulk_write(n)
        }
        Commands::BenchParquet => {
            let registry = bundled_registry()?;
            let mut prism = Infigraph::open(&root, registry)?;
            prism.init()?;
            let store = prism.store().context("not initialized")?;
            store.benchmark_parquet_vs_csv()
        }
        Commands::Update => cmd_update(),
        Commands::Group { action } => cmd_group(&root, action),
        Commands::Repos => cmd_repos(),
        Commands::SearchCode {
            pattern,
            file_pattern,
            limit,
        } => cmd_search_code(&root, &pattern, file_pattern.as_deref(), limit),
        Commands::Snippet { symbol_id } => cmd_snippet(&root, &symbol_id),
        Commands::Architecture => cmd_architecture(&root),
        Commands::DetectChanges { base, depth } => cmd_detect_changes(&root, &base, depth),
        Commands::Cluster => cmd_cluster(&root),
        Commands::Export { format, output } => cmd_export(&root, &format, output),
        Commands::Visualize => cmd_visualize(&root),
        Commands::VisualizeSymbol { symbol_id, depth } => {
            cmd_visualize_symbol(&root, &symbol_id, depth)
        }
        Commands::Routes => cmd_routes(&root),
        Commands::ScipImport { index } => cmd_scip_import(&root, &index),
        Commands::Watch { debounce } => cmd_watch(&root, debounce),
        Commands::IndexDocs => cmd_index_docs(&root),
        Commands::ReindexDocs => cmd_reindex_docs(&root),
        Commands::CleanDocs => cmd_clean_docs(&root),
        Commands::SearchDocs { query, limit } => cmd_search_docs(&root, &query, limit),
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
            &root,
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
        Commands::IndexManifests => cmd_index_manifests(&root),
        Commands::Dependencies { ecosystem } => cmd_dependencies(&root, ecosystem.as_deref()),
        Commands::FindRefs { symbol } => cmd_find_refs(&root, &symbol),
        Commands::ApiSurface { file } => cmd_api_surface(&root, file.as_deref()),
        Commands::FileDeps { file } => cmd_file_deps(&root, &file),
        Commands::TypeHierarchy { symbol, depth } => cmd_type_hierarchy(&root, &symbol, depth),
        Commands::TestCoverage { file } => cmd_test_coverage(&root, file.as_deref()),
        Commands::Security { severity, category } => {
            cmd_security(&root, severity.as_deref(), category.as_deref())
        }
        Commands::Complexity { threshold, file } => {
            cmd_complexity(&root, threshold, file.as_deref())
        }
        Commands::SemanticDiff { old, new } => cmd_semantic_diff(&root, &old, &new),
        Commands::Sequence { symbol_id, depth } => cmd_sequence(&root, &symbol_id, depth),
        Commands::Refactor {
            target,
            focus,
            limit,
        } => cmd_refactor(&root, target.as_deref(), &focus, limit),
        Commands::Check {
            config,
            json,
            checks,
        } => {
            let any_failed =
                cmd_check(&root, config.as_deref(), json, checks.as_deref())?;
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
            &root,
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
        } => cmd_vulns(&root, severity.as_deref(), ecosystem.as_deref(), json),
        Commands::Forget => cmd_forget(&root),
        Commands::BridgesPromote => cmd_bridges_promote(&root),
        Commands::DetectPatterns { pattern, json } => {
            cmd_detect_patterns(&root, pattern.as_deref(), json)
        }
    }
}
