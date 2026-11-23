# Document Indexing — How It Works

This document describes how Infigraph discovers, extracts, chunks, links, and searches documents. All code lives in the `crates/infigraph-docs/` crate.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Entry Points](#entry-points)
3. [File Discovery](#file-discovery)
4. [Document Extraction](#document-extraction)
5. [Chunking](#chunking)
6. [Graph Storage (DocStore)](#graph-storage-docstore)
7. [Link Extraction](#link-extraction)
8. [BFS Crawling](#bfs-crawling)
9. [Combined Group Document Store](#combined-group-document-store)
10. [Manifest Integration](#manifest-integration)
11. [Incremental Indexing](#incremental-indexing)
12. [Embeddings](#embeddings)
13. [Search](#search)
14. [Watch Mode](#watch-mode)

---

## Architecture Overview

```
File Discovery ──► Extraction ──► Chunking ──► DocStore (Kùzu)
                                                  │
                                           Link Extraction
                                           ┌──────┴──────┐
                                      Intra-repo      Cross-repo
                                      LINKS_TO        LINKS_TO
                                                  │
                                           BFS Crawling
                                      (follow links outside
                                       doc root, within git repo)
                                                  │
                                           Embeddings + BM25
                                                  │
                                           Hybrid Search
```

The pipeline runs in this order:

1. Walk the directory tree, collect document files
2. Hash each file (SHA-256), skip unchanged files (incremental)
3. Extract text from each format (Markdown, PDF, DOCX, etc.)
4. Chunk extracted text into heading-bounded or fixed-size segments
5. Bulk-load documents and chunks into Kùzu via Parquet COPY
6. Extract links from each document, create LINKS_TO edges
7. BFS-crawl outgoing links to discover docs outside the doc root but inside the git repo
8. Generate embeddings for new/changed chunks
9. Prune stale documents no longer on disk

---

## Entry Points

### DocIndex

`DocIndex` (`lib.rs:22-26`) is the top-level driver:

```rust
struct DocIndex {
    root: PathBuf,
    db_path: PathBuf,
    store: Option<DocStore>,
}
```

| Method | What it does |
|--------|-------------|
| `open(root)` | Creates `.infigraph/` dir, sets `db_path = .infigraph/docs.kuzu` |
| `init()` | Opens `DocStore::open(&db_path)`, initializes schema |
| `index()` | Main incremental indexing routine |
| `reindex()` | `clean()` + `index()` — full rebuild |
| `clean()` | Drops store, deletes `docs.kuzu`, `.wal`, `.lock`, embeddings, HNSW index |

### MCP Tools

All MCP tools live in `crates/infigraph-mcp/src/tools/docs.rs`:

| Tool | Function | Behavior |
|------|----------|----------|
| `index_docs` | `tool_index_docs` | Prefers shelling out to `infigraph index-docs` CLI; falls back to in-process `DocIndex`. Starts doc watcher afterward. |
| `reindex_docs` | `tool_reindex_docs` | Same CLI-vs-inprocess pattern, calls `idx.reindex()` |
| `clean_docs` | `tool_clean_docs` | Calls `idx.clean()` |
| `search_docs` | `tool_search_docs` | Hybrid BM25+vector search |
| `watch_docs` | `tool_watch_docs` | Spawns background watcher thread |
| `stop_watch_docs` | `tool_stop_watch_docs` | Signals watcher to stop |
| `index_manifests` | `tool_index_manifests` | Links package manifests to docs |

### CLI

The `infigraph` CLI binary (`crates/infigraph-cli/`) exposes `index-docs`, `reindex-docs`, `clean-docs` subcommands that the MCP tools shell out to for process isolation.

---

## File Discovery

`DocIndex::collect_doc_files()` (`lib.rs:212-216`) calls `walk_doc_dir()` (`lib.rs:218-247`), a recursive `std::fs::read_dir` walker.

### Ignored directories

These directories are always skipped:
`.infigraph`, `.git`, `node_modules`, `__pycache__`, `.venv`, `venv`, `target`, `build`, `dist`, `.tox`, plus any directory starting with `.`

### Supported extensions

`is_document_file(path)` (`lib.rs:430-456`) accepts these extensions (case-insensitive):

| Category | Extensions |
|----------|-----------|
| Text markup | `md`, `markdown`, `txt`, `rst`, `adoc`, `org` |
| Office | `docx`, `pptx`, `xlsx`, `rtf` |
| Web | `html`, `htm` |
| Data | `xml`, `xsl`, `xsd`, `svg`, `plist` |
| Binary docs | `pdf`, `epub` |

> **Note:** `epub` passes file discovery but `extract_document` has no epub handler — these files are silently skipped during extraction.

---

## Document Extraction

`extract_document(path, bytes, ext)` (`extract.rs:51-91`) dispatches on the file extension and returns an `ExtractedDoc`:

```rust
struct ExtractedDoc {
    file: String,
    title: Option<String>,
    content_hash: String,
    format: DocFormat,
    text: String,
    page_count: Option<usize>,
}
```

`DocFormat` enum: `Markdown`, `PlainText`, `Rst`, `Asciidoc`, `Org`, `Pdf`, `Docx`, `Pptx`, `Xlsx`, `Html`, `Rtf`, `Xml`.

### Format-specific extractors

#### Markdown, PlainText, RST, AsciiDoc, Org

All use the same `extract_text()` (`extract.rs:93-101`): UTF-8 lossy decode, title = first line with leading `#` stripped. RST/AsciiDoc/Org get **no format-specific parsing** — they are treated as plain text.

#### PDF

`extract_pdf` (`extract.rs:103-147`): Uses `pdf_oxide::PdfDocument::from_bytes`. Iterates pages, calls `doc.extract_text(page_index)`, joins with `\n`. Title = first non-empty line. Falls back to raw ASCII if PDF parsing fails (for malformed files).

#### DOCX

`extract_docx` (`extract.rs:149-168`): Opens as ZIP (`zip::ZipArchive`), reads `word/document.xml`, strips OOXML markup via `extract_text_from_ooxml`.

#### PPTX

`extract_pptx` (`extract.rs:170-206`): ZIP; scans for `ppt/slides/slideN.xml` entries, sorts by name, extracts each via `extract_text_from_ooxml`, joins with `\n\n`. `page_count` = number of slides.

#### XLSX

`extract_xlsx` (`extract.rs:208-235`): Uses `calamine::open_workbook_auto_from_rs`. For each sheet: tab-joins cell values per row. `page_count` = sheet count, title = first sheet name.

#### HTML

`extract_html` (`extract.rs:237-272`): Hand-rolled tag stripper (char-by-char state machine), extracts `<title>` via substring search, collapses whitespace.

#### RTF

`extract_rtf` (`extract.rs:274-304`): Hand-rolled control-word stripper tracking brace depth. Only emits text at `brace_depth <= 2` (skips font tables and metadata).

#### XML (including XSL, XSD, SVG, plist)

`extract_xml` (`extract.rs:306-353`): Streaming `quick_xml::Reader`, concatenates text nodes. Title = root element's local name.

---

## Chunking

All chunking logic is in `chunk.rs`.

```rust
struct Chunk {
    id: String,         // "{file}::chunk_{index}"
    doc_file: String,
    content_hash: String,
    index: usize,
    heading: Option<String>,
    text: String,
    start_offset: usize,
    end_offset: usize,
    page: Option<usize>,
}
```

### Strategy selection

`ChunkStrategy::for_extension(ext)` (`chunk.rs:23-29`) currently always returns `HeadingBounded` regardless of extension. The per-extension branch is effectively dead code.

### Chunking cascade

The `HeadingBounded` strategy follows a three-level cascade:

```
HeadingBounded
  │
  ├─ Has headings? ──► chunk_by_headings (split on # headings)
  │                     └─ Section > 512 words? ──► sub-chunk with overlap
  │
  └─ No headings? ──► chunk_by_paragraphs (split on blank lines)
                        ├─ Has paragraphs? ──► accumulate up to 512 words
                        │                      └─ Paragraph > 512? ──► sub-chunk
                        └─ No paragraphs? ──► chunk_by_tokens (fixed 512-word windows)
```

### Constants

- `MAX_SECTION_TOKENS`: 512 (word count, not LLM tokens)
- `SUB_CHUNK_OVERLAP`: word overlap between consecutive sub-chunks of oversized sections

### chunk_by_headings (`chunk.rs:49-140`)

Regex: `(?m)^(#{1,6})\s+(.+)$|^([^\n]+)\n[=\-]{3,}$` — matches both ATX (`# heading`) and Setext (underline) Markdown headings. Splits text into `(heading, start, end)` sections. Sections ≤ 512 words become one chunk. Oversized sections are word-sliced into sub-chunks with overlap.

### chunk_by_paragraphs (`chunk.rs:142-273`)

Splits on `\n\n`. Greedily accumulates paragraphs into a buffer up to 512 words, flushing when the next paragraph would overflow. Individual paragraphs over 512 words are sub-chunked. Headings are inferred via `infer_heading(text)`.

### chunk_by_tokens (`chunk.rs:289-349`)

Pure fixed-size word window with overlap. Used as final fallback when text has no headings and no blank-line paragraph structure.

---

## Graph Storage (DocStore)

`DocStore` (`store.rs`) wraps a Kùzu embedded graph database at `<root>/.infigraph/docs.kuzu`.

### Schema

Applied idempotently via `CREATE ... IF NOT EXISTS` in `init_schema` (`store.rs:20-57`):

**Node tables:**

| Table | Primary Key | Fields |
|-------|------------|--------|
| `Document` | `id` | `title`, `file`, `format`, `content_hash`, `page_count`, `chunk_count` |
| `Chunk` | `id` | `doc_file`, `idx`, `heading`, `text`, `start_offset`, `end_offset`, `page`, `content_hash` |
| `Source` | `id` | `source_type`, `base_url`, `space_key`, `last_synced` |
| `PipelineCore` | `id` | `name`, `doc_id`, `plugin_id`, `inputs[]`, `outputs[]` |

**Relationship tables:**

| Relationship | From → To | Properties |
|-------------|-----------|-----------|
| `HAS_CHUNK` | Document → Chunk | — |
| `LINKS_TO` | Document → Document | `url`, `link_type` |
| `FROM_SOURCE` | Document → Source | — |
| `DEFINED_IN` | PipelineCore → Document | — |
| `DEPENDS_ON` | PipelineCore → PipelineCore | `dep_type` |

### Bulk loading via Parquet

`upsert_all_parquet(docs, chunks)` (`store.rs:111-263`) uses Arrow + Parquet for bulk loads instead of row-by-row inserts:

1. Delete existing `Chunk` and `Document` rows for changed files via `DETACH DELETE` Cypher
2. Write an Arrow `RecordBatch` for Document rows → temp Parquet file → `COPY Document FROM`
3. Same for Chunk rows → `COPY Chunk FROM`
4. Same for HAS_CHUNK edges → `COPY HAS_CHUNK FROM`

### Concurrency

A global `DB_LOCK` (`store.rs:64`) mutex serializes all write access — required by Kùzu's single-writer constraint.

### Key methods

| Method | Purpose |
|--------|---------|
| `get_doc_hashes()` | Returns `HashMap<file, content_hash>` for incremental diffing |
| `ensure_document_node(id)` | Creates a Document node if it doesn't exist (for cross-repo targets) |
| `create_link(from, to, url, link_type)` | Creates a LINKS_TO edge |
| `delete_links_from(doc_id)` | Removes all outgoing LINKS_TO edges from a document |
| `delete_docs_by_ids(ids)` | Removes stale documents no longer on disk |
| `stats()` | Returns `DocStoreStats` (doc count, chunk count, link count) |

---

## Link Extraction

Link extraction happens in `links.rs`.

### extract_links (`links.rs:33-59`)

Finds links in document text using two regex patterns:

- **Markdown links:** `\[([^\]]*)\]\(([^)]+)\)` — `[text](url)`
- **HTML links:** `<a\s[^>]*href=["']([^"']+)["']` — `<a href="url">`

Anchor-only links (`#fragment`) are skipped. Each match is classified via `classify_doc_link`.

### classify_doc_link (`links.rs:61-110`)

Classification order matters — first match wins:

| Priority | Condition | `link_type` | `target_doc_id` |
|----------|-----------|-------------|-----------------|
| 1 | URL contains `/wiki/`, `confluence`, or `atlassian` | `confluence` | Confluence page ID if parseable |
| 2 | URL contains `/browse/` or `jira` | `jira` | None |
| 3 | URL contains `/blob/` or `/-/blob/` AND is `http(s)://` | `github` | File path extracted from URL |
| 4 | Any other `http://`, `https://`, or `//` URL | `external` | None |
| 5 | Everything else (relative paths) | `local` | Resolved relative path |

### extract_and_link_doc (`links.rs:16-31`)

The per-document LINKS_TO writer:

1. Extract links from document text
2. Delete all existing outgoing LINKS_TO edges for this document
3. For each link with a resolved `target_doc_id` that exists in `all_doc_ids` and isn't self-referential, create a LINKS_TO edge

Links to non-indexed targets (dangling links) are silently dropped.

### URL resolution helpers

| Function | Purpose |
|----------|---------|
| `extract_doc_path_from_url` (`links.rs:199-237`) | Extracts file path from GitHub `/blob/` or GitLab `/-/blob/` URLs, strips query/fragment |
| `extract_repo_from_url` (`links.rs:241-254`) | Extracts repo name from GitHub/GitLab URLs (works with enterprise hosts) |
| `resolve_relative_path` (`links.rs:112-148`) | Lexical `..`/`.` resolution against document's directory (no disk access) |
| `resolve_link_to_abs_path` (`links.rs:152-174`) | Resolves relative path to absolute, requires file to exist on disk (used by BFS) |
| `extract_confluence_page_id` (`links.rs:176-196`) | Parses `/wiki/spaces/SPACE/pages/PAGEID/` → `confluence://SPACE/PAGEID` |

---

## BFS Crawling

BFS crawling (`bfs_follow_links`, `lib.rs:249-411`) discovers documents **outside the doc root but inside the git repo** by following links from already-indexed documents.

### When it runs

Triggered unconditionally at the end of `DocIndex::index()` (`lib.rs:193-202`), after the main indexing pass and after link extraction. Only runs if a git repo root is found (`find_repo_root`, `lib.rs:414-428` — walks upward looking for `.git`).

### Parameters

- `max_depth`: 2 (hardcoded) — how many hops from the original doc set
- `max_extra`: 50 (hardcoded) — maximum new documents to discover

### Algorithm

```
frontier = all currently-indexed doc files (canonicalized paths)

for depth in 0..max_depth:
    for each doc in frontier:
        read file text
        extract_links()
        for each link:
            resolve_link_to_abs_path() → must exist on disk
            filter: is_document_file(), not symlink, inside repo_root
            filter: not in ignore dirs, not already indexed
            
            extract, chunk, store immediately
            add to next_frontier
    
    frontier = next_frontier

if any new docs discovered:
    re-run link extraction for ALL indexed docs
```

### Safety boundaries

- **Git repo boundary:** resolved path must canonicalize to somewhere inside the git repo root
- **Symlink protection:** checked via `symlink_metadata` before canonicalization to prevent symlink escape
- **Ignore dirs:** same list as file discovery (`.git`, `node_modules`, etc.)
- **Depth and count limits:** max 2 hops, max 50 new documents

### Post-BFS re-linking

If BFS discovers new docs, link extraction is re-run for **every** currently-indexed document (`lib.rs:387-408`). This allows newly discovered docs to be linked to/from all existing docs.

---

## Combined Group Document Store

`build_combined_docs` (`combined.rs`) publishes immutable document generations for a repository group:

```
~/.infigraph/groups/<group>/.infigraph/docs-generations/gen-<timestamp>-<pid>/
  docs.kuzu
  docs_embeddings.bin
  docs_hnsw_index.usearch
  docs_hnsw_index.meta
```

Readers select the newest completely published generation. Legacy flat files remain readable until the first generation is built. Per-repository document stores remain the incremental indexing sources.

### Build pipeline

1. Acquire the group's cross-process `docs-build.lock`.
2. Build a staged database in a temporary directory on the same filesystem; the active store remains queryable.
3. Open each repository's DocStore **sequentially** because `DocStore` serializes embedded Kùzu access process-wide.
4. Export `Document`, `Chunk`, `Source`, `HAS_CHUNK`, `LINKS_TO`, and `FROM_SOURCE` data through Kùzu `COPY TO` Parquet.
5. Prefix repository-scoped identifiers with `"[<repo>]::"` and import the transformed Parquet into the staged DocStore.
6. Merge each repository's `docs_embeddings.bin`, prefixing chunk IDs and dropping embeddings for chunks absent from the combined graph. Corrupt files or inconsistent vector dimensions fail the build.
7. Re-read repository documents and create `cross_repo` LINKS_TO edges directly between real combined Document nodes.
8. Build the staged document HNSW index when the merged embedding count reaches 200,000.
9. Close the staged database and atomically rename its complete artifact directory into `docs-generations`. Readers see either the previous or new generation, never a partial file set.
10. Retain the newest two generations and best-effort remove older ones.

`PipelineCore`, dynamic `Pipeline_*` plugin tables, `DEFINED_IN`, and `DEPENDS_ON` remain per-repository and are not copied into the combined document store.

### Collision handling

Repository prefixes make otherwise identical paths and chunk IDs distinct:

```
[repo-a]::README.md
[repo-b]::README.md
[repo-a]::README.md::chunk_0
[repo-b]::README.md::chunk_0
```

Cross-repo links target the prefixed real document in the destination repository rather than creating a synthetic stub. Repository keys are resolved through the registry name, local directory name, and Git remote slug. Nested GitLab blob URLs and safe path-suffix matching are supported.

### Integration with group_build

`group_build` runs the document pipeline as Step 5 of 5:

1. Index all repos (code)
2. Sync contracts
3. Link cross-service calls
4. Build the combined code graph
5. **Index per-repository docs + build the combined document store**

`group_link_docs` rebuilds only the combined document store from existing per-repository indexes. `group_search_docs` and `infigraph group search-docs` run hybrid BM25+vector search against the combined store. After a successful per-repository watcher reindex, affected existing group stores are refreshed asynchronously and coalesced per group.

### Auto-Recovery

All four store types auto-recover from corruption:

| Store | Recovery behavior |
|-------|------------------|
| **Single doc store** (`DocIndex::init`) | Catches open failure, wipes `docs.kuzu`, reopens, reindexes from source files |
| **Combined doc store** (`combined_doc_search`, `combined_doc_query`) | On query/search failure, wipes the corrupt generation directory and schedules a background `build_combined_docs` rebuild via `REFRESHING_GROUPS` |
| **Single code graph** (`Infigraph::init`) | Catches `GraphStore::open` failure, wipes `graph/` + `graph.wal`, reopens empty |
| **Combined code graph** (`open_combined_graph`) | On open failure, wipes the combined graph directory + WAL. Caller must trigger `build_combined_graph` to rebuild |

Combined doc store recovery is fully automatic (background thread rebuilds). Combined code graph recovery requires an explicit rebuild call (e.g., via `group_build` or `group_index`).

---

## Manifest Integration

`link_manifest_doc_urls` (`links.rs:317-352`) creates LINKS_TO edges from package manifest files to indexed documents.

Called from `tool_index_manifests` (`docs.rs:591-643`), which sources `doc_urls` from `infigraph_core::manifest::index_manifests` (URLs found in `homepage`, `repository`, `documentation` fields of `package.json`, `Cargo.toml`, etc.).

### Resolution strategy

For each URL in the manifest:
1. Try Confluence page ID match → exact match against `all_doc_ids`
2. Try GitHub/GitLab blob path extraction → exact match, then path-segment suffix match
3. Fall back to suffix match (`doc_id.ends_with(doc_path)` or vice versa) to handle path prefix mismatches

Edge type is `"manifest_ref"`. The manifest file itself gets a synthetic Document node.

---

## Incremental Indexing

### Content hashing

SHA-256 over raw file bytes, computed during `index()` (`lib.rs:113-117`) inside a parallel `rayon` `par_iter`.

### Skip logic

1. `store.get_doc_hashes()` loads the previously stored `file → content_hash` map
2. A file is skipped entirely if its freshly computed hash matches the stored hash
3. Changed/new files go through the full extract → chunk → store pipeline
4. Old rows are deleted via `DETACH DELETE` before new rows are inserted (delete+reinsert, not true in-place upsert)

### Stale document pruning

After indexing (`lib.rs:160-180`): any doc ID present in the old hash map but whose file is no longer found by the current file walk is deleted via `store.delete_docs_by_ids`. This handles files removed from disk between runs.

### Full rebuild

`reindex()` = `clean()` + `index()` — wipes everything and rebuilds from scratch. Used when hash-based diffing isn't trusted (e.g., after schema changes).

---

## Embeddings

Embedding logic is in `embed.rs`.

### update_doc_embeddings (`embed.rs:15-101`)

1. Remove embeddings for changed files from the on-disk cache (`docs_embeddings.bin`)
2. Embed only new/changed chunks in batches of 256
3. Prepend a "path context" string to each chunk's text before embedding (via `doc_path_context` — strips generic dir names like `src/doc/docs/documentation/resources`)
4. Prune embeddings whose chunk ID no longer exists in the store
5. Rebuild HNSW index only if chunk count ≥ 200,000 or an HNSW index already exists on disk

### Storage

- Embeddings: `.infigraph/docs_embeddings.bin`
- HNSW index: `.infigraph/docs_hnsw_index.usearch` + `.meta`

---

## Search

### hybrid_doc_search (`search.rs:206-218`)

The primary search entry point, called from `tool_search_docs`. Delegates to `hybrid_doc_search_in_dir` (`search.rs:257-360`) for local mode:

1. Load or build a `DocBM25Index` (see Persistent cache below)
2. BM25 search for top `limit * 3` results
3. Normalize BM25 scores by max score
4. Vector search: embed query → HNSW search (or brute-force linear scan if no HNSW index)
5. Normalize vector scores by max
6. Combine: `score = (1 - alpha) * bm25 + alpha * vector` (default `alpha = 0.5`)
7. Sort, truncate to `limit`, hydrate full chunk details

### DocBM25Index (`search.rs:31-204`)

Standard Okapi BM25 implementation:

- `build(docs)`: tokenizes all docs, builds inverted index `term → [(doc_idx, term_freq)]`, tracks `avg_doc_len`
- `search(query, limit)`: IDF = `ln((N - df + 0.5) / (df + 0.5) + 1)`, standard TF normalization with K1 and B parameters
- Tokenizer: simple whitespace splitting + lowercasing

### Persistent cache

BM25 index is persisted as `.infigraph/docs_bm25_cache.bin`, valid only when at least as new as `docs_embeddings.bin` (the freshness anchor — the same pattern code search uses for `bm25_cache.bin`), so a doc reindex that rewrites embeddings automatically invalidates the cache. A missing, stale, or corrupt cache is not an error: `load_or_build_doc_index` silently falls back to rebuilding the index in-memory from the store and best-effort re-saves it (save failures are non-fatal). The cache is only written when `docs_embeddings.bin` already exists and the chunk set is non-empty — without an anchor there is no invalidation signal, so no cache is written.

### Result format

`DocSearchResult`: `chunk_id`, `doc_file`, `heading`, `text`, `score`, `bm25_score`, `vector_score`, `start_offset`, `end_offset`, `page`. The MCP tool converts byte offsets to 1-indexed line numbers for display.

---

## Watch Mode

`watch_docs(root, debounce_ms, stop_rx, log_prefix)` (`watch.rs:10-69`) uses the `notify` crate's `RecommendedWatcher` in recursive mode.

### Loop behavior

- Polls every 500ms via `recv_timeout`
- Checks `stop_rx` each iteration for a stop signal
- On any filesystem event where a changed path passes `is_document_file`, sets `pending = true`
- Once `pending` is true and `debounce_ms` has elapsed, triggers incremental reindex: `DocIndex::open → init → index` (not `reindex` — content-hash-based incremental, not full rebuild)

### Management

- Started via `tool_watch_docs` or automatically after `tool_index_docs` (via `auto_start_doc_watch`)
- Tracked in global `DOC_WATCHERS` map keyed by timestamp-based ID
- Stopped via `tool_stop_watch_docs` which signals the stop channel
