# Infigraph — Architecture & Technical Design

## Table of Contents

1. [What Problem It Solves](#1-what-problem-it-solves)
2. [How It Works — End to End](#2-how-it-works--end-to-end)
3. [What Gets Persisted and Where](#3-what-gets-persisted-and-where)
4. [Graph Schema](#4-graph-schema)
5. [Codebase Layout](#5-codebase-layout)
6. [Indexing Patterns](#6-indexing-patterns)
7. [Search — How Hybrid Works](#7-search--how-hybrid-works)
8. [Incremental Indexing](#8-incremental-indexing)
9. [Cross-File Call Resolution](#9-cross-file-call-resolution)
10. [Design Decisions and Trade-offs](#10-design-decisions-and-trade-offs)
11. [Known Limitations](#11-known-limitations)
12. [Measuring Impact](#12-measuring-impact)

---

## 1. What Problem It Solves

AI coding agents (Claude Code, Cursor, Copilot, etc.) are structurally blind to your codebase. When an agent needs to answer "who calls this function?" or "what breaks if I change this class?", it has two options — read files (expensive in tokens, slow, often incomplete) or guess (unreliable).

Infigraph solves this by building a **persistent knowledge graph** of your codebase — all symbols, all call edges, all import relationships — before the agent ever runs. Queries that would require reading dozens of files instead resolve as sub-millisecond graph traversals.

The practical effect: AI agents answer structural questions precisely without consuming context-window space on raw file contents. The README claims 60–80% token reduction on symbol-heavy tasks; the actual number depends on how file-heavy the current workflow is.

---

## 2. How It Works — End to End

```
Source Files (any of 62 languages)
        |
        | SHA-256 hash check (skip unchanged files)
        v
   AST Parsing
   ├── tree-sitter (59 languages) — entities.scm + relations.scm queries
   └── ANTLR4 (3 languages)      — .g4 grammars + Rust extraction listeners
        |
        v
   FileExtraction { symbols: Vec<Symbol>, relations: Vec<Relation> }
        |
        |-- Cross-file call resolution pass (name-based + import-scope aware)
        |
        v
   KuzuDB Graph Store (.infigraph/graph/)
   ├── Node tables: Symbol, Module, File, Folder, Cluster, Dependency
   └── Edge tables: CALLS, IMPORTS, CONTAINS, INHERITS, TESTED_BY, ...
        |
        v
   Embedding pass (new/changed symbols only)
   ├── Model2Vec — potion-base-8M, 256-dim (primary)
   └── Trigram hash fallback (if model not found)
        |
        v
   embeddings.bin (.infigraph/embeddings.bin)
        |
        v
   MCP Server (infigraph-mcp) — 59 tools exposed to AI agents
   └── Web UI (localhost:9749) — graph explorer, route map, search
```

Everything runs locally. No LLM calls, no cloud APIs, no network required during indexing or querying.

---

## 3. What Gets Persisted and Where

Two distinct storage locations:

### Per-project (inside each repo)

```
your-project/
└── .infigraph/
    ├── graph/                  KuzuDB columnar graph database
    │   ├── catalog.kz          schema and table metadata
    │   ├── data/               column files (one per property per table)
    │   └── wal/                write-ahead log for crash recovery
    ├── sessions/               Session context database (separate KuzuDB instance)
    │   └── db/                 Stores session summaries, pending tasks, decisions, touched files
    ├── embeddings.bin          256-dim float32 vectors, one per symbol
    ├── hnsw.bin                (optional) HNSW index for approximate nearest neighbor search
    └── graph.html              (optional) last generated visualization
```

### Global (shared across all projects)

```
~/.infigraph/
├── models/
│   └── potion-base-8M/         ML model files (copied from release archive)
│       ├── model.safetensors   weight tensors (~15MB)
│       └── tokenizer.json      vocabulary
└── registry.json               index of all known projects and groups
                                 { "repos": { "my-app": { "path": "/work/my-app" } },
                                   "groups": { "platform": { "repos": [...] } } }
```

Each project has its own fully independent graph database. Running `infigraph index` in `/work/service-a` builds `/work/service-a/.infigraph/` — it does not affect any other project. The only shared state is the model (weights don't change) and registry.json (a lookup table, not data).

The `.infigraph/` directory is automatically excluded from indexing, grep search, and file walking.

### embeddings.bin binary format

The vector file uses a simple custom binary format (length-prefixed, little-endian):

```
[count: u32]
for each symbol:
  [id_length: u32][id_bytes: utf8]
  [dim: u32][f32 * dim]
```

This keeps vector loading fast (sequential read, no parsing overhead) and keeps vectors out of KuzuDB where columnar storage would add overhead for the cosine similarity workload.

In **remote mode** (`--features remote`), embeddings are stored in Postgres + pgvector (`embeddings` table with `vector(256)` column). The search path materializes vectors into memory via `PostgresMetaStore::all_embeddings("symbol")` and uses brute-force cosine scoring instead of HNSW.

---

## 4. Graph Schema

The full KuzuDB schema (from `crates/infigraph-core/src/graph/schema.rs`):

### Node Tables

| Table | Key Properties |
|-------|---------------|
| `Symbol` | id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, embedding |
| `Module` | id, name, file, language, content_hash, summary |
| `File` | id, name, path, language, symbol_count |
| `Folder` | id, name, path |
| `Cluster` | id, name, description |
| `Dependency` | id, name, version, ecosystem, is_dev |

**Symbol kinds** (language-agnostic): Function, Method, Class, Struct, Interface, Trait, Enum, Module, Variable, Constant, Test, Section, Route

Symbol `id` format: `"relative/path/to/file.py::symbol_name"` or `"file.py::ClassName::method_name"` for methods.

### Edge Tables

| Edge | Direction | Properties |
|------|-----------|------------|
| `CALLS` | Symbol → Symbol | — |
| `IMPORTS` | Module → Module | — |
| `CONTAINS` | Module → Symbol | — |
| `INHERITS` | Symbol → Symbol | — |
| `TESTED_BY` | Symbol → Symbol | — |
| `READS` | Symbol → Symbol | — |
| `WRITES` | Symbol → Symbol | — |
| `MEMBER_OF` | Symbol → Cluster | — |
| `SIMILAR_TO` | Symbol → Symbol | score: FLOAT |
| `BRIDGE_TO` | Symbol → Symbol | bridge_kind, detail |
| `CALLS_SERVICE` | Symbol → Symbol | method, path, target_service |
| `DEPENDS_ON` | Module → Dependency | is_dev: BOOLEAN |
| `DEFINES` | File → Symbol | — |
| `CONTAINS_FILE` | Folder → File | — |
| `CONTAINS_FOLDER` | Folder → Folder | — |

All Cypher queries are supported: `MATCH`, `WHERE`, `WITH`, `OPTIONAL MATCH`, variable-length paths (`-[:CALLS*1..5]->`), mutations, aggregations.

---

## 5. Codebase Layout

```
infigraph/
├── crates/
│   ├── infigraph-core/          Core library — all analysis logic
│   │   └── src/
│   │       ├── model/            Symbol, Relation, FileExtraction types
│   │       ├── lang/             LanguagePack trait, LanguageRegistry
│   │       ├── extract/          AST → Symbol/Relation extraction
│   │       │   ├── entities.rs   Processes tree-sitter entity captures
│   │       │   └── relations.rs  Processes tree-sitter relation captures
│   │       ├── graph/            KuzuDB store, schema DDL, query helpers
│   │       ├── search/           BM25 index + hybrid search + grep
│   │       ├── embed/            EmbedProvider trait, Model2Vec, trigram fallback
│   │       ├── resolve/          Cross-file call resolution pass
│   │       ├── cluster/          Louvain community detection
│   │       ├── multi/            Multi-repo registry, groups, cross-service deps
│   │       ├── routes/           HTTP route/endpoint detection (22 frameworks)
│   │       ├── scip/             SCIP index import (compiler-grade enrichment)
│   │       ├── viz/              HTML graph visualization (vis.js)
│   │       ├── export/           Cypher, GraphML, JSON export
│   │       ├── diff/             Git diff → affected symbols
│   │       ├── bridges/          Cross-language FFI/gRPC/JNI bridge detection
│   │       ├── security/         Sensitive file detection (secrets, keys, etc.)
│   │       ├── watch/            File system watcher for live reindex (auto-starts after indexing)
│   │       ├── refactor/         Refactoring analysis — complexity, coupling, clones, dead code
│   │       ├── sequence.rs       Mermaid sequence diagram generation from call graph
│   │       ├── session/          Session context persistence (save/restore across AI sessions)
│   │       └── manifest/         MCP manifest / agent config reading
│   │
│   ├── infigraph-languages/     59 tree-sitter language packs
│   │   └── languages/<lang>/
│   │       ├── entities.scm      Tree-sitter queries: symbols to extract
│   │       └── relations.scm     Tree-sitter queries: edges to extract
│   │
│   ├── infigraph-grammar-plugin/  Runtime ANTLR grammar plugin system (JVM bridge)
│   │
│   ├── infigraph-cli/           40 CLI commands (infigraph binary)
│   ├── infigraph-mcp/           59-tool MCP server + web UI (infigraph-mcp binary)
│   └── lsp-to-scip/              Generic LSP → SCIP bridge (lsp-to-scip binary)
│
├── models/
│   └── potion-base-8M/           Bundled Model2Vec weights (shipped in release archive)
├── tests/
│   └── fixtures/microservices/   Realistic test repos (Python, TypeScript, Rust)
├── install.sh                    One-line installer (Unix)
├── install.ps1                   One-line installer (Windows)
└── release.sh                    Local release builder
```

---

## 6. Indexing Patterns

### Single repository (most common)

```bash
cd /your/project
infigraph index
```

All supported file types across all directories are indexed into one graph. With 62 supported languages, a monorepo with TypeScript frontend, Python backend, and HCL infrastructure config is indexed in a single pass — each component into the same graph. Cross-language call edges are not created (see Limitations), but all symbols, routes, and structural relationships within each language are fully connected.

### Multi-component monorepo

Same as above. There is no special configuration needed. Run `infigraph index` from the repo root and all components are indexed into one unified graph. Queries like "find all HTTP routes across all services" or "dead code across all components" work project-wide.

### Multi-repo / microservices

For architectures where services live in separate repositories:

```bash
infigraph group create platform
infigraph group add platform /path/to/service-a
infigraph group add platform /path/to/service-b
infigraph group sync platform        # detect HTTP contracts between services
infigraph group deps platform        # map cross-service URL call dependencies
infigraph group query platform "MATCH (s:Symbol) WHERE s.kind = 'Route' RETURN s.name, s.file"
```

Each repo still has its own `.infigraph/` database. The group is a logical overlay in `~/.infigraph/registry.json` that enables cross-repo Cypher queries and HTTP contract detection. `group sync` scans URL string literals in each service and matches them against the route definitions of other services in the group.

---

## 7. Search — How Hybrid Works

Every `search_symbols` query runs two engines in parallel and combines their scores:

### BM25 (lexical)

A custom BM25 implementation built from all symbol texts (name + docstring). Parameters tuned for code: K1=1.2, B=0.75. Tokenization splits on non-alphanumeric characters (preserving underscores) and lowercases. Both BM25 and vector scores are independently normalized to [0, 1] before combining.

Best for: exact or near-exact name matches, API lookups, known symbol names.

### Model2Vec (semantic)

Each symbol's text (kind + name + language + docstring) is embedded into a 256-dimensional float32 vector using `potion-base-8M`, a distilled sentence transformer that runs as pure Rust inference with no ONNX runtime or GPU. Vectors are precomputed at index time and loaded from `embeddings.bin` on first search.

Best for: conceptual queries ("authentication logic", "payment handling"), synonyms, partial description matches.

### Combining scores

```
final_score = (1.0 - alpha) * bm25_score + alpha * vector_score
```

`alpha` defaults to 0.5. Setting `alpha=0.0` gives pure BM25 (fast, exact); `alpha=1.0` gives pure vector (semantic). The default balance works well for most code search queries.

### Trigram fallback

If the Model2Vec model files are not found, the embedder automatically falls back to character trigram hashing (no ML, no model files required, pure Rust). Quality is noticeably lower for semantic queries but the system remains fully functional.

---

## 8. Incremental Indexing

Every indexed file has a SHA-256 fingerprint stored in the `Module` node (`content_hash` property). The fingerprint covers both halves of staleness: the file's bytes **and** the language pack that parsed it (its `entities.scm`/`relations.scm` sources, the grammar's structural identity, its custom edge definitions, and `EXTRACTOR_SCHEMA_VERSION`). Upgrading an extractor therefore re-extracts the files that extractor owns without a `--full` reindex, and without disturbing other languages. On subsequent `infigraph index` runs:

1. All files are fingerprinted (in parallel via rayon) via `LanguagePack::content_fingerprint`
2. Files whose fingerprint matches the stored one are skipped entirely — no re-parsing, no graph updates
3. Changed and new files are re-parsed and their nodes/edges are deleted and reinserted
4. The cross-file call resolution pass only re-resolves calls from changed files, but reads the full symbol table from the graph (so cross-file edges from unchanged files are preserved)

For large changes (>100 files changed), the write path uses KuzuDB's `COPY FROM CSV` bulk loader for throughput. For small changes (<100 files), it uses per-file transactions which have lower overhead for tiny batches.

Embedding updates are also incremental: only symbols in changed files get new embeddings. Symbols in unchanged files keep their cached vectors.

---

## 9. Cross-File Call Resolution

AST extraction is file-local: when a call to `authenticate()` appears in `main.py`, the extractor creates a `CALLS` edge to `main.py::authenticate`. But the real definition is in `auth.py`.

A post-indexing resolution pass fixes this:

1. Builds a global symbol table: `name → [(id, file, kind)]` from the full graph
2. For each `CALLS` edge where the target doesn't exist in the same file, looks up the target name globally
3. If exactly one cross-file match exists → creates the resolved edge
4. If multiple candidates exist → filters by import scope (uses the `IMPORTS` edges to find which files are actually imported by the caller)
5. SQL CTEs (function-kind, `.sql` files) are explicitly excluded from cross-file resolution (CTE names are query-scoped, not global)

Unresolved calls (to builtins, external libraries, dynamic dispatch targets) are silently dropped — they don't create dangling edges.

Resolution statistics are reported after every index run: `total cross-file calls / resolved / unresolved`.

---

## 10. Design Decisions and Trade-offs

### Rust for the core engine

The index-and-query loop runs on every agent tool call. Python or Node would add per-invocation interpreter startup overhead and memory pressure. Rust gives native performance, a single statically-linked binary with no runtime dependencies, and safe concurrency (rayon for parallel file parsing).

### KuzuDB (lbug) over SQLite or Neo4j

- **SQLite**: No native graph traversal. Variable-length path queries (blast radius, transitive callers) would require recursive CTEs or application-level loops — both slow and complex.
- **Neo4j**: Requires a running server process, JVM, significant RAM, and separate installation. The goal is zero-config local use.
- **KuzuDB** (via the `lbug` maintained fork): Embedded, columnar, Cypher-native. Runs in-process, zero configuration, supports full Cypher including variable-length paths. The columnar layout means property scans (e.g. "all symbols of kind Route") are fast because only the kind column is read. The trade-off is that KuzuDB is less mature than SQLite and the lbug fork adds a build-time cmake dependency.

### tree-sitter as the primary parser

tree-sitter provides:
- Grammar-based AST for 59 languages with a single Rust API
- Error-tolerant parsing (produces partial trees for files with syntax errors)
- Pattern-matching query language (`.scm` files) for extracting symbols without hand-writing traversal code
- Active community maintaining language grammars

The trade-off: tree-sitter is a concrete syntax tree, not a semantic one. It has no type information, no import resolution, no scope awareness. That is why the cross-file resolution pass is necessary, and why compiler-grade SCIP import is provided for languages where precision matters.

### ANTLR4 as the fallback for custom DSLs

For languages with no tree-sitter grammar, ANTLR4 generates a full parser from a `.g4` grammar. The generated Rust code is checked in (no Java needed at runtime or build time — only for grammar regeneration). The trade-off is that writing an ANTLR extraction listener is more work than writing `.scm` queries.

### Model2Vec instead of OpenAI/Cohere embeddings

Embedding via API would require network access, API keys, and proxy configuration — none of which can be assumed in an enterprise development environment. Model2Vec (`potion-base-8M`) is a distilled sentence transformer that runs as pure Rust inference (~15MB model, ~30ms per batch). Quality is lower than GPT-text-embedding-3 but more than sufficient for code symbol search. The model ships bundled in the release archive.

### embeddings.bin separate from KuzuDB

Similarity search uses dot-product scoring (vectors are L2-normalized at embedding time, so dot product ≡ cosine similarity) with rayon-parallelized brute-force scan. A process-lifetime cache keyed by file mtime eliminates repeated disk loads — the first query reads the full file, subsequent queries in the same MCP session hit memory. KuzuDB's columnar format would store vectors in a FLOAT[] column, but loading them through the Kuzu query interface adds serialization overhead that makes the operation significantly slower for the full-scan workload. The flat binary file is bulk-read in one call and cached for the process lifetime.

### HNSW Approximate Nearest Neighbor Index

For large codebases (>100K symbols), brute-force scan can become a bottleneck. Infigraph builds an optional HNSW (Hierarchical Navigable Small World) index at `.infigraph/hnsw.bin` for approximate nearest neighbor search. The HNSW index is built after embedding computation and provides sub-linear query time for similarity lookups — ~2ms for 500K symbols vs ~50ms brute-force. The index is rebuilt incrementally when embeddings change. For smaller projects, brute-force remains the default as the overhead of maintaining the HNSW structure is not justified.

### Auto-Watch After Indexing

The MCP server automatically starts a file watcher after any indexing operation (`index_project`, `scip_import`, `group_index`). This keeps the graph in sync with file changes without requiring a manual `watch_project` call. The watcher uses OS-level filesystem events (fsevents on macOS, inotify on Linux) with 500ms debounce and auto-reindexes only changed files. A duplicate-path guard prevents multiple watchers on the same project.

### Session Continuity

Session context (summary, pending tasks, decisions, touched files) is persisted to a separate KuzuDB instance at `.infigraph/sessions/db`. This keeps session data isolated from the code graph — sessions can be purged without affecting the index. Each session stores TOUCHED edges linking to files the agent worked on, enabling semantic resume: `get_latest_session` returns the prior session's state so the agent can pick up where it left off. Sessions are auto-purged after 30 days by default.

### LM2 Memory System

Session continuity is extended by a 5-phase memory pipeline (LM2): output gate (filters noise), tiered retrieval (L1 current file, L2 related files, L3 semantic archive), confidence decay (older memories lose weight), auto-injection (relevant session context injected into `symbol_context`/`get_doc_context` output), and consolidation (merges related sessions to reduce redundancy and boost confidence). Tools: `memory_context` gathers code + session + skeleton context in one call with automatic depth selection; `consolidate_memory` merges overlapping sessions.

### Search Performance Caches

Two disk caches accelerate repeat searches. A BM25 binary cache at `.infigraph/bm25_cache.bin` persists the inverted index across CLI/MCP sessions — eliminates rebuild on every search call. A binary HNSW sidecar at `.infigraph/hnsw.bin` stores the approximate nearest neighbor graph in a compact binary format. Together they reduce MCP repeat search latency by 16x (3.68s→223ms) and CLI search by 2x (4.3s→2.08s).

---

## 11. Known Limitations

| Limitation | Detail |
|------------|--------|
| No cross-language call edges | A TypeScript frontend calling a Python backend via HTTP is detected by `group deps` (URL matching), but there is no direct CALLS edge between the TypeScript caller and the Python handler. |
| No dynamic dispatch resolution | Virtual function calls, duck typing, interface dispatch — the graph has structural edges from the AST, not runtime call graph edges. |
| Similarity search fallback is brute-force | HNSW index is built when available; falls back to rayon-parallelized dot-product scan (~19ms for 129K symbols). Brute-force scales linearly but remains fast for most projects. |
| No type inference | Type information comes only from SCIP import (if available). AST-only indexing does not resolve generic types, inferred types, or union types. |
| Windows cross-compilation unsupported | KuzuDB/lbug requires C++20 `<format>` (GCC 13+). Cross-compiling for Windows from macOS fails due to available Docker images shipping older GCC. Windows must be built natively. |
| Generated code excluded from fmt | ANTLR-generated Rust parsers in `src/generated/` have `#![rustfmt::skip]` and are not checked by `cargo fmt`. |

---

## 12. Measuring Impact

Concrete metrics to assess before/after adopting Infigraph in an AI agent workflow:

| Metric | How to measure | Typical direction |
|--------|---------------|-------------------|
| Tokens per agent session | Export Claude Code usage before and after enabling Infigraph on a representative task set | Down 40–80% on symbol-heavy tasks |
| Tool calls to answer a structural question | Count `Read` / `Glob` / `Grep` calls vs. single `search_symbols` or `trace_callers` call | Down from N file reads to 1 graph query |
| Incremental index time | `time infigraph index` on second run (only changed files) vs. full build | Seconds vs. minutes |
| Cross-file call resolution rate | Reported after every `infigraph index`: "X resolved, Y unresolved" | Unresolved = builtins/externals, not bugs |
| Agent correctness on "who calls X?" | Manual spot-check: compare agent answer via grep vs. via `trace_callers` | Graph answer is exact; grep misses dynamic callers |

The token reduction claim is most pronounced when an agent would otherwise read multiple full source files to answer a structural question. For tasks that are already file-local (e.g., "fix this bug in this function"), the benefit is smaller.
