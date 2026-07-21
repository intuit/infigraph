---
name: analysis-subsystems
description: How Infigraph's multi-repo/group mode and taint analysis work internally — HTTP contract extraction heuristics, cross-service edge linking, combined-graph merge, remote mode, plus taint's line-based tracking and sanitizer heuristic. Use when working on crates/infigraph-core/src/multi/ or src/taint/, or investigating taint/cross-service false positives.
---

# Analysis subsystems

Two independent analysis passes over the graph, grouped here since each has real non-obvious complexity worth knowing before touching it.

## Multi-repo groups

Core logic: `crates/infigraph-core/src/multi/`. One of the least AST-driven subsystems — much of the contract-extraction and cross-service-linking logic is string/regex heuristics, not tree-sitter queries.

**Contract extraction** — tiered, most-confident-first: use an already-detected `Route` symbol if one exists, else fall back to decorator/docstring pattern matching, else scrape router-prefix patterns from raw source. Only add lower-tier patterns for frameworks the route-detection pass doesn't already cover.

**Cross-service linking** — matches dynamic URLs (including templated/interpolated ones) against known routes, with self-match suppression and method-preference fallback, to build `CALLS_SERVICE` edges across repos.

**Combined-graph merge** — per-repo graphs merge via a bulk export/prefix/import pipeline rather than row-by-row inserts, for performance at scale. ID-prefixing conventions differ between node and edge tables — a new node/edge type must follow the existing convention exactly, or it produces dangling references silently.

**Remote mode** (`--features remote`) swaps the graph store/registry backends and enables parallel multi-repo indexing (impossible in local single-writer mode). Any new `multi/` feature should consider whether it needs a remote-mode branch, or it may silently no-op or violate single-writer assumptions under concurrent writes.

## Taint analysis

Core engine: `crates/infigraph-core/src/taint/`. `concerns/` and `reflection/` are separate, simpler pattern scanners — not part of this engine.

**Important: line-based, not real dataflow.** The intra-procedural analyzer scans line by line, tracking tainted variables through a heuristic assignment parser — not a true AST/dataflow analysis. Sink sanitization is decided by a **proximity heuristic** (a sanitizer pattern within a few lines of a sink counts as covering it), not precise flow tracking. This is the most common source of both false positives and false negatives — calibrate expectations before "fixing" what looks like an inaccurate heuristic; it's coarse by design.

**Adding a source/sink/sanitizer**: these are per-language string pattern tables keyed by category. A sink and its sanitizer must share the same category key to be linked — adding one without the other means that sink can never be marked sanitized.

**Intra- vs inter-procedural**: intra-procedural gives full path/sanitization detail within one function. Inter-procedural does a depth-bounded BFS over the call graph to connect a source-containing function to a sink-containing function across functions, but carries no path/sanitization detail — it only reports that a connection could exist.

**Known limitations (by design)**: string-substring matching (no real AST/types) plus the proximity heuristic mean both false positives and false negatives are expected. A precision fix here usually means real dataflow tracking, a much bigger change than tuning the heuristic.
