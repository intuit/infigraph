# Code Parsing — How It Works

This document describes how Infigraph parses source code, extracts symbols and relationships, builds a graph, and enables search. The core logic lives in `crates/infigraph-core/` with language definitions in `crates/infigraph-languages/`.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Entry Points](#entry-points)
3. [Language Detection and Registry](#language-detection-and-registry)
4. [File Discovery](#file-discovery)
5. [Tree-Sitter Parsing](#tree-sitter-parsing)
6. [Grammar Plugin System (ANTLR)](#grammar-plugin-system-antlr)
7. [Symbol Model](#symbol-model)
8. [Edge Extraction](#edge-extraction)
9. [Graph Storage (Kùzu)](#graph-storage-kùzu)
10. [Incremental Indexing](#incremental-indexing)
11. [Embeddings](#embeddings)
12. [Search](#search)
13. [Watch Mode](#watch-mode)
14. [Route and Contract Extraction](#route-and-contract-extraction)
15. [Multi-Repo Groups](#multi-repo-groups)

---

## Architecture Overview

```
File Discovery ──► Language Detection ──► Tree-Sitter Parse ──► AST
                                                                 │
                                                    ┌────────────┴────────────┐
                                               entities.scm              relations.scm
                                               (symbols)                 (edges)
                                                    │                         │
                                                    ▼                         ▼
                                              SymbolKind::*             CALLS, IMPORTS,
                                              Function, Class,         INHERITS, READS,
                                              Method, Struct...        WRITES, TESTED_BY
                                                    │                         │
                                                    └─────────┬───────────────┘
                                                              ▼
                                                    Kùzu Graph Database
                                                    (.infigraph/graph.kuzu)
                                                              │
                                              ┌───────────────┼───────────────┐
                                         Embeddings        BM25 Index     HNSW Index
                                     (potion-base-8M)   (.bm25_cache)   (.usearch)
                                              │               │               │
                                              └───────────────┴───────────────┘
                                                              │
                                                     Hybrid Search
                                              (BM25 + vector + regex grep)
```

The pipeline runs in this order:

1. Walk the directory tree, detect language per file
2. Hash each file (SHA-256), skip unchanged files (incremental)
3. Parse source via tree-sitter (or ANTLR grammar plugin for unsupported languages)
4. Run `entities.scm` query to extract symbols (functions, classes, methods, etc.)
5. Run `relations.scm` query to extract edges (calls, imports, inherits, etc.)
6. Extract statements, routes, and additional analysis edges
7. Bulk-load symbols and edges into Kùzu via Parquet COPY
8. Generate embeddings for new/changed symbols
9. Prune stale symbols from deleted files

---

## Entry Points

### Infigraph struct

`Infigraph` is the top-level driver (analogous to `DocIndex` for documents). Key methods:

| Method | What it does |
|--------|-------------|
| `open(root, lang_registry)` | Opens or creates `.infigraph/` dir, sets up graph DB path |
| `init()` | Initializes Kùzu schema, opens the graph store |
| `index()` | Main incremental indexing routine |
| `reindex()` | Full wipe and rebuild |

### MCP Tools

| Tool | Purpose |
|------|---------|
| `index_project` | Index a project (prefers CLI subprocess, falls back to in-process) |
| `search` | Unified hybrid search (BM25 + vector + grep) |
| `get_doc_context` | Source + callers + callees for a symbol |
| `find_all_references` | All usages of a symbol across the codebase |
| `trace_callers` / `trace_callees` | Call chain traversal |
| `transitive_impact` | Blast radius analysis |
| `watch_project` | Start background file watcher |

### CLI

The `infigraph` binary (`crates/infigraph-cli/`) exposes `index`, `reindex`, `search`, `watch`, and analysis subcommands.

---

## Language Detection and Registry

### LanguageRegistry

`LanguageRegistry` (`crates/infigraph-core/src/lang/registry.rs:9-16`) maps file extensions to language packs.

### LanguagePack

Each language is defined as a `LanguagePack` (`crates/infigraph-core/src/lang/mod.rs:42-47`):

```rust
struct LanguagePack {
    name: &'static str,
    extensions: &'static [&'static str],
    backend: ParserBackend,
    custom_edges: Option<...>,
}
```

### ParserBackend

`ParserBackend` enum (`crates/infigraph-core/src/lang/mod.rs:32-39`):

```rust
enum ParserBackend {
    TreeSitter {
        grammar: Language,          // tree-sitter grammar
        entity_query: Query,       // entities.scm compiled
        relation_query: Query,     // relations.scm compiled
    },
    Custom(Box<dyn CustomExtractor>),
}
```

### Bundled languages

`bundled_registry()` (`crates/infigraph-languages/src/lib.rs:182-425`) registers all 62 supported languages. Each language has:

- A `tree-sitter-<lang>` crate providing the grammar
- `entities.scm` — tree-sitter query for symbol extraction
- `relations.scm` — tree-sitter query for relationship extraction
- `lang.toml` — language metadata

All query files live under `crates/infigraph-languages/languages/<lang>/`.

### Supported languages (62)

Rust, Python, JavaScript, TypeScript, Java, Go, C, C++, C#, Ruby, PHP, Swift, Kotlin, Scala, Dart, Elixir, Haskell, Lua, R, Julia, Perl, Bash, PowerShell, Zig, Nim, OCaml, Erlang, Clojure, F#, Groovy, MATLAB, Fortran, COBOL, Ada, Pascal, Verilog, SystemVerilog, VHDL, Assembly, Makefile, CMake, Dockerfile, HCL/Terraform, Nix, YAML, TOML, JSON, XML, HTML, CSS, SCSS, SQL, GraphQL, Protobuf, Thrift, Svelte, Vue, JSX, TSX, Markdown, LaTeX, Typst.

---

## File Discovery

File discovery walks the project directory, applying:

### Ignored directories

Same as document indexing: `.git`, `node_modules`, `__pycache__`, `.venv`, `target`, `build`, `dist`, plus directories starting with `.`

### File selection

A file is indexed if:
1. Its extension matches a registered `LanguagePack` in the registry
2. It's not a binary file
3. It's under configurable size limits

### Parallel processing

Files are processed in parallel using `rayon`'s `par_iter` for extraction and hashing.

---

## Tree-Sitter Parsing

### extract_file (`crates/infigraph-core/src/extract/mod.rs:18-72`)

The core parsing function for a single file:

1. Set the tree-sitter `Language` on a thread-local parser (`TS_PARSER`)
2. Parse source code into an AST tree
3. Run `extract_entities` — executes `entities.scm` query against the AST
4. Run `extract_relations` — executes `relations.scm` query against the AST
5. Run `extract_statements_for_symbols` — extracts statement-level detail
6. Generate route → handler CALLS edges via `generate_route_handler_edges`
7. Compute the SHA-256 `content_hash` fingerprint over the file bytes plus the language pack's extractor fingerprint (`LanguagePack::content_fingerprint`)

### entities.scm — Symbol extraction

Each language's `entities.scm` is a tree-sitter query that captures symbol definitions. Example pattern:

```scheme
;; Capture function definitions
(function_definition
  name: (identifier) @name) @definition.function

;; Capture class definitions
(class_definition
  name: (identifier) @name) @definition.class
```

Capture names like `@definition.function`, `@definition.class`, `@definition.method` map to `SymbolKind` variants. The extraction layer reads start/end positions, visibility modifiers, parameters, return types, and docstrings from the AST context.

### relations.scm — Edge extraction

Each language's `relations.scm` captures relationships:

```scheme
;; Capture function calls
(call_expression
  function: (identifier) @reference.call)

;; Capture imports
(import_statement
  module_name: (dotted_name) @reference.import)

;; Capture inheritance
(class_definition
  superclasses: (argument_list
    (identifier) @reference.inherits))
```

Capture names like `@reference.call`, `@reference.import`, `@reference.inherits` map to edge types (CALLS, IMPORTS, INHERITS).

---

## Grammar Plugin System (ANTLR)

For languages without tree-sitter grammars, Infigraph supports ANTLR-based grammar plugins.

### Plugin structure

Each plugin provides:
- `plugin.toml` — metadata (language name, extensions, grammar files)
- `.g4` files — ANTLR grammar definitions (externally owned, never modified)

### Execution model

```
Rust host ──stdin/stdout JSON──► JVM subprocess (GrammarDriver.java)
                                      │
                                 ANTLR Parser
                                      │
                                 Symbol/Edge JSON
```

- **Rust host:** `crates/infigraph-grammar-plugin/src/driver.rs:150-173` — `parse()` method sends source code to the JVM process and reads back extracted symbols/edges as JSON
- **JVM driver:** `driver/src/main/java/com/infigraph/driver/GrammarDriver.java` — loads ANTLR grammars, parses source, walks the parse tree, and emits structured JSON
- **Plugin registration:** `register_grammar_plugins()` (`crates/infigraph-grammar-plugin/src/lib.rs:64-144`) scans for `plugin.toml` files and registers each as a `Custom` backend in the language registry

### Difference from tree-sitter path

| Aspect | Tree-Sitter | Grammar Plugin |
|--------|------------|----------------|
| Grammar source | Rust crate (`tree-sitter-<lang>`) | `.g4` files + JVM |
| Query language | `.scm` (S-expression) | Java visitor pattern |
| Performance | In-process, fast | Subprocess, slower |
| Setup | Zero (bundled) | Requires JVM |

---

## Symbol Model

### SymbolKind

`SymbolKind` enum (`crates/infigraph-core/src/model/mod.rs:6-21`):

```rust
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Trait,
    Enum,
    Module,
    Variable,
    Constant,
    Test,
    Section,
    Route,
    Field,
}
```

### Symbol node fields (Kùzu)

The `Symbol` node table stores:

| Field | Type | Description |
|-------|------|-------------|
| `id` | STRING (PK) | Unique symbol identifier (typically `file::name` or qualified path) |
| `name` | STRING | Symbol name |
| `kind` | STRING | One of the SymbolKind variants |
| `file` | STRING | Source file path (relative to project root) |
| `start_line` | INT64 | First line of the symbol definition |
| `end_line` | INT64 | Last line of the symbol definition |
| `signature_hash` | STRING | Hash of the symbol's signature (for change detection) |
| `language` | STRING | Programming language |
| `visibility` | STRING | `public`, `private`, `protected`, etc. |
| `parent` | STRING | Parent symbol ID (for nested definitions) |
| `docstring` | STRING | Documentation string / comment |
| `complexity` | INT64 | Cyclomatic complexity |
| `parameters` | STRING | Parameter list (serialized) |
| `return_type` | STRING | Return type annotation |
| `category` | STRING | Semantic category |

---

## Edge Extraction

### Relationship types

All edge types are defined in `CREATE_SCHEMA` (`crates/infigraph-core/src/graph/schema.rs:16-106`):

| Edge | From → To | Properties | How extracted |
|------|-----------|-----------|--------------|
| `CALLS` | Symbol → Symbol | — | `relations.scm` `@reference.call` captures |
| `IMPORTS` | Module → Module | — | `relations.scm` `@reference.import` captures |
| `CONTAINS` | Module → Symbol | — | File-level containment |
| `INHERITS` | Symbol → Symbol | — | `relations.scm` `@reference.inherits` captures |
| `TESTED_BY` | Symbol → Symbol | — | Test function → tested function mapping |
| `READS` | Symbol → Symbol | — | Variable read access |
| `WRITES` | Symbol → Symbol | — | Variable write access |
| `DEFINES` | File → Symbol | — | File-level symbol ownership |
| `CALLS_SERVICE` | Symbol → Symbol | `method`, `path`, `target_service` | Cross-service call detection |
| `SIMILAR_TO` | Symbol → Symbol | `score` | Embedding similarity (clone detection) |
| `BRIDGE_TO` | Symbol → Symbol | `bridge_kind`, `detail` | Bridge/adapter pattern detection |
| `HAS_STATEMENT` | Symbol → Statement | — | Statement-level detail extraction |
| `HAS_CONCERN` | Symbol → Concern | — | Cross-cutting concern detection |
| `HAS_CONFIG` | Symbol → ConfigBinding | — | Configuration binding detection |
| `RESOLVES_TO` | Symbol → Symbol | `mechanism`, `config_source` | Dynamic resolution (DI, reflection) |
| `TAINT_FLOW` | Symbol → Symbol | `source_kind`, `sink_kind`, `path` | Taint analysis flow |
| `DEPENDS_ON` | Module → Dependency | `is_dev` | Package dependency |
| `MEMBER_OF` | Symbol → Cluster | — | Code cluster membership |
| `CONTAINS_FILE` | Folder → File | — | Directory structure |
| `CONTAINS_FOLDER` | Folder → Folder | — | Directory nesting |

### Other node tables

| Node | Fields | Purpose |
|------|--------|---------|
| `Module` | `id, name, file, content_hash, language` | File-level module (one per source file) |
| `File` | `id, path, language, size` | Physical file |
| `Folder` | `id, path` | Directory |
| `Dependency` | `id, name, version, ecosystem` | External package dependency |
| `Cluster` | `id, label` | Code cluster (from clustering analysis) |
| `Statement` | `id, kind, text, line` | Statement-level nodes |
| `Concern` | `id, name, kind` | Cross-cutting concerns |
| `ConfigBinding` | `id, key, value, source` | Configuration bindings |

### Edge resolution

Edges from `relations.scm` use **name-based resolution**: the query captures a called/imported/inherited name, and the extraction layer resolves it to a symbol ID by searching the current file's symbols and imported modules. Cross-file resolution uses the Module → IMPORTS → Module edges to follow import chains.

---

## Statement-Level Extraction

Beyond symbols and edges, Infigraph extracts **control-flow statements** from every function, method, and test. This powers complexity analysis, test context generation, and fine-grained understanding of function internals.

### How it works

`extract_statements_for_symbols` (`crates/infigraph-core/src/extract/mod.rs:74-99`):

1. Filters symbols to `Function`, `Method`, and `Test` kinds
2. Walks the AST via `collect_fn_nodes` (`extract/mod.rs:101-123`) — matches AST nodes to symbols by line range
3. For each matched function node, calls `extract_statements` (`crates/infigraph-core/src/analysis/mod.rs:16-34`) to extract control-flow nodes

### Statement types

Extracted statement kinds include:

| Kind | Example |
|------|---------|
| `If` | `if condition { ... }` |
| `ElseIf` | `else if condition { ... }` |
| `Else` | `else { ... }` |
| `For` | `for item in collection { ... }` |
| `While` | `while condition { ... }` |
| `DoWhile` | `do { ... } while (condition)` |
| `Loop` | `loop { ... }` (Rust) |
| `Match` | `match value { ... }` / `switch` |
| `Case` | Individual match/switch arms |
| `Try` | `try { ... }` |
| `Catch` | `catch (e) { ... }` / `except` |
| `Ternary` | `condition ? a : b` |
| `Guard` | Early return / guard clause |

### Storage

Statements are stored as `Statement` nodes in Kùzu, linked to their parent symbol via `HAS_STATEMENT` edges. Each Statement has: `id`, `kind`, `text`, `line`.

### Usage

- **Complexity analysis:** statement count and nesting depth contribute to cyclomatic complexity
- **Test context generation:** `generate_test_context` uses statements to understand what a function does internally
- **Code understanding:** `get_doc_context` includes statement breakdown for richer context

---

## Test Detection and TESTED_BY Edges

Infigraph automatically detects test functions and links them to the code they test via `TESTED_BY` edges.

### Test detection

During entity extraction, functions matching test patterns are assigned `SymbolKind::Test`:

| Language | Pattern |
|----------|---------|
| Rust | `#[test]`, `#[rstest]`, functions in `mod tests` |
| Python | `def test_*`, functions in `test_*.py` |
| JavaScript/TypeScript | `it()`, `test()`, `describe()` blocks |
| Java | `@Test` annotation |
| Go | `func Test*(t *testing.T)` |
| And more per language... |

### TESTED_BY edge derivation

`store.derive_tested_by_edges()` (called post-indexing):

1. Finds all symbols with `kind = 'Test'`
2. Traces each test's `CALLS` edges to find which non-test functions it invokes
3. Creates `TESTED_BY` edges from the tested function → the test function
4. Idempotent — safe to call multiple times (deletes and recreates)

### Test coverage analysis

The `get_test_coverage` MCP tool uses TESTED_BY edges to report:
- **Untested symbols** — functions/methods with no TESTED_BY edges, ranked by caller count (more callers = higher priority to test)
- **Test-to-code ratio** — percentage of functions covered by at least one test
- **Source code** of untested functions for immediate context

---

## Graph Storage (Kùzu)

### Database location

`.infigraph/graph.kuzu` — a Kùzu embedded graph database directory.

### Schema initialization

`CREATE_SCHEMA` (`crates/infigraph-core/src/graph/schema.rs:16-106`) — all tables created with `IF NOT EXISTS` for idempotency.

### Bulk loading

Like the document store, code indexing uses Parquet COPY for bulk loads:

1. Export symbols/edges as Arrow RecordBatches
2. Write to temporary Parquet files
3. `COPY <table> FROM '<parquet_path>'` into Kùzu

Old data for changed files is deleted via `DETACH DELETE` before re-inserting.

### Query layer

`crates/infigraph-core/src/graph/queries.rs` — provides typed query methods:
- `get_callers(symbol_id)`, `get_callees(symbol_id)` — direct call relationships
- `trace_callers(symbol_id, depth)`, `trace_callees(symbol_id, depth)` — transitive traversal
- `find_references(symbol_id)` — all usages across the graph
- `get_symbols_in_file(file)` — all symbols defined in a file
- `transitive_impact(symbol_id)` — downstream blast radius

### File hash storage

Content fingerprints are stored on `Module` nodes via the `content_hash` column. Retrieved by `get_file_hashes()` (`graph/cozo_store.rs:1405-1412`, `graph/store.rs:163-175`) for incremental indexing.

---

## Incremental Indexing

### How it works

1. Walk the directory tree, collect all source files
2. Fingerprint each file with `LanguagePack::content_fingerprint` — SHA-256 over the file's bytes **and** the pack's extractor fingerprint
3. Load previously stored fingerprints via `get_file_hashes()`
4. **Skip** files whose fingerprint hasn't changed
5. Re-parse and re-extract only changed/new files

### What invalidates a file's fingerprint

Because the fingerprint folds in the language pack, a file becomes stale either when the user edits it or when Infigraph's own extraction for that language changes:

| Change | Detected | How |
|--------|----------|-----|
| File edited | exact | file bytes |
| `entities.scm` / `relations.scm` edited | exact | query sources hashed into the pack fingerprint |
| Custom edge definitions changed | exact | edge name/capture pairs |
| Grammar crate upgraded | heuristic | ABI version plus node-kind/field/parse-state counts — tree-sitter exposes no grammar version, so an upgrade that leaves all four counts unchanged is missed |
| Runtime grammar-plugin (ANTLR) grammar, extractor source, or `plugin.toml` changed | exact | `plugin_fingerprint` hashes the plugin directory's inputs into the pack via `LanguagePack::with_fingerprint_part` |
| Rust-side extraction logic changed (symbol-ID scheme, new derived edge, span convention) | **manual** | bump `EXTRACTOR_SCHEMA_VERSION` in `lang/mod.rs` |
| Grammar-plugin built-in extractor changed inside the driver jar | **not detected** | the jar's contents are opaque; only the extractor's name is covered |

The fingerprint is deliberately *per pack*, not global: a Java query fix re-extracts Java files and leaves Python and TypeScript cached. Mixing in the Infigraph crate version instead would make every release a full reindex.

Two call sites produce a `content_hash` — the skip gate in `Infigraph::index_via_backend` and `extract_file` — and they must use the same formula, hence the single `content_fingerprint` function. If they ever diverge, nothing is skipped and every index silently becomes a full reindex; `test_incremental_index_skips_unchanged` (`tests/index_perf.rs`, `#[ignore]`d, run with `--ignored`) is the guard.
6. Delete old symbols/edges for changed files via `DETACH DELETE`
7. Insert new symbols/edges via Parquet COPY
8. Delete symbols for files that no longer exist on disk (stale pruning)

### Change detection tool

`detect_changes` MCP tool / CLI command uses the diff module (`crates/infigraph-core/src/diff/compute.rs`) to compare the current file state against the indexed graph, reporting which files have changed, been added, or been removed.

---

## Embeddings

### Model

**potion-base-8M** (Model2Vec) — 256-dimensional, bundled locally at `models/potion-base-8M/`. No network calls or API keys required.

### EmbedProvider trait (`crates/infigraph-core/src/embed/mod.rs:55-65`)

```rust
pub trait EmbedProvider: Send + Sync {
    fn dimension(&self) -> usize;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn embed(&self, text: &str) -> Result<Vec<f32>> { /* default: batch of 1 */ }
}
```

### Implementations

| Implementation | Dimensions | Usage |
|---------------|-----------|-------|
| `Model2VecEmbedder` | 256 | Primary (used when model files are available) |
| `TrigramEmbedder` | configurable | Fallback (character trigram hashing, no model files needed) |

### Singleton instances

Two separate embedder singletons:
- `CODE_EMBEDDER` / `code_embedder()` — for code symbols
- `DOC_EMBEDDER` / `doc_embedder()` — for document chunks

Both initialized via `init_embedder` / `best_embedder` (`embed/mod.rs:311-340`).

### Storage

**Local mode:**
- Embeddings: `.infigraph/embeddings.bin` (code), `.infigraph/docs_embeddings.bin` (docs)
- HNSW index: `.infigraph/hnsw_index.usearch` + `.meta`

**Remote mode** (`--features remote`, `INFIGRAPH_BACKEND=neo4j`):
- Embeddings: Postgres + pgvector (`embeddings` table, `kind` column separates `symbol` vs `doc_chunk`)
- HNSW index: not used — brute-force scoring via materialized vectors from `all_embeddings(kind)`

### HNSW threshold

HNSW index is built only when the symbol/chunk count exceeds a threshold (100K+ for code, 200K+ for docs) or when an HNSW index already exists. Below the threshold, brute-force linear scan is used (fast enough for smaller codebases). Remote mode always uses brute-force.

---

## Search

### Unified search tool

The `search` MCP tool runs **three strategies in one call**:

1. **BM25** — keyword/term frequency matching
2. **Vector** — semantic similarity via embeddings
3. **Regex grep** — exact text pattern matching

Results are merged, deduplicated, and ranked.

### BM25Index (`crates/infigraph-core/src/search/mod.rs:29-35`)

```rust
struct BM25Index {
    docs: Vec<(String, String)>,                    // symbol_id → text
    inverted: HashMap<String, Vec<(usize, f32)>>,   // term → (doc_index, tf)
    avg_doc_len: f32,
}
```

Standard Okapi BM25 scoring with K1 and B parameters. Score computation in `compute_raw_scores()` (`search/mod.rs:200-274`).

### Persistent cache

BM25 index is persisted as `.infigraph/bm25_cache.bin` to avoid rebuilding across CLI/MCP sessions.

### Hybrid scoring

Combined score: `score = (1 - alpha) * bm25_normalized + alpha * vector_normalized`

Default `alpha = 0.3` for code search (0.5 for doc search). Scores are normalized by their respective maximum values before combining.

---

## Watch Mode

### watch_project (`crates/infigraph-core/src/watch/mod.rs:59-78`)

Uses the `notify` crate's `RecommendedWatcher` in recursive mode.

### Behavior

- Monitors the project directory for file changes
- Debounces changes (configurable `debounce_ms`)
- On change: triggers incremental reindex for affected files
- Emits `WatchEvent` via callback for MCP-side tracking
- Auto-started after `index_project`, `scip_import`, or `group_index`

### Watcher management (MCP)

- Tracked in `WatcherEntry` / `get_watchers` / `init_watchers` (`crates/infigraph-mcp/src/tools/watch.rs`)
- `get_watch_status` surfaces pending files needing full reindex (e.g., cross-file call-edge changes)
- `stop_watch` signals the watcher to stop via channel

### Limitations

Single-file changes can be incrementally re-parsed, but changes affecting cross-file edges (e.g., renaming a function called from other files) may require a broader reindex. The watch status tool reports when this is the case.

---

## Route and Contract Extraction

### Route detection (heuristic)

`detect_routes()` (`crates/infigraph-core/src/routes/mod.rs:51-75`):

1. Query all `Function`/`Method` symbols from the graph
2. For each, call `detect_route_from_symbol` — heuristic matching on name, decorator, and docstring patterns
3. Per-language detectors in `routes/{python,go,java,js_ts,rust,ruby,php,csharp,elixir,generic}.rs`
4. Framework sniffers (`detect_python_framework`, `detect_java_framework`, `detect_rust_framework` in `routes/helpers.rs:93-195`) identify which web framework is in use

Detected patterns include:
- Python: Flask `@app.route`, Django `urlpatterns`, FastAPI decorators
- Java: Spring `@RequestMapping`, JAX-RS `@Path`
- Go: `http.HandleFunc`, Gin/Echo/Chi router calls
- Rust: Actix `#[get]`/`#[post]`, Axum router
- JavaScript/TypeScript: Express `.get()`/`.post()`, Next.js file-based routes
- And more per language

### Contract extraction (structured)

The newer, more rigorous mechanism for multi-repo use:

`sync_group_contracts → extract_contracts` (`crates/infigraph-core/src/multi/mod.rs`):

- Extracts one structured route fact per real endpoint at index time
- Consumed uniformly by the cross-service linker
- Distinct from the heuristic `detect_routes` — produces `Contract` structs with `kind`, `service`, `method`, `path`, `symbol_id`, `file`

### Cross-service call detection

`crates/infigraph-core/src/multi/cross_service.rs`:

- `scan_source_for_urls` — finds URL patterns in source code
- `extract_api_paths` — extracts API path patterns
- `link_cross_service_calls` (line 491-700) — matches caller URLs against contracts from other repos, creates `CALLS_SERVICE` edges

---

## Multi-Repo Groups

### Concept

Groups allow indexing and querying across multiple repositories as a unified graph.

### Workflow

```
group_create("my-group")
  → group_add("my-group", "repo-a", path)
  → group_add("my-group", "repo-b", path)
  → group_build("my-group")
      Step 1: Index all repos
      Step 2: Sync contracts
      Step 3: Link cross-service calls (CALLS_SERVICE edges)
      Step 4: Build combined graph
      Step 5: Index docs + cross-repo doc linking
```

### Combined graph

`build_combined_graph()` (`crates/infigraph-core/src/multi/combined.rs:22-245`):

1. Creates a fresh combined Kùzu DB at a group-specific path
2. For each repo: exports Symbol, Module, File nodes and all edge tables to Parquet
3. Prefixes every ID with `[{repo_name}]::` to avoid collisions across repos
4. `COPY FROM` all Parquet files into the combined store
5. Runs `resolve_cross_repo()` to create cross-repo edges using contracts

### Registry

`Registry` stores repo entries (name, path, symbol count) and group definitions (repos, contracts). Persisted to disk and loaded by all group operations.

### Group search

`group_search` (`combined.rs`) runs hybrid BM25+vector search across the combined graph. `group_query` executes arbitrary Cypher against it.
