# Changelog

All notable changes to Infigraph are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [3.2.18] - 2026-11-26

### Changed

- Graph database upgraded to lbug (Kuzu) 0.22.3, and builds now pin the prebuilt library to that version instead of fetching LadybugDB's latest release (#101). Existing indexes upgrade in place on the first write; older infigraph versions can't read them afterwards. Building from source now finds OpenSSL through `OPENSSL_DIR`; see the README

### Fixed

- Read-only tools (`search`, `get_stats`, etc.) now return a stable `PROJECT_NOT_INDEXED` error with the `infigraph index` command when a project has no code graph, instead of kuzu's "Cannot create an empty database under READ ONLY mode". A `.infigraph/` directory holding only `sessions/` no longer counts as indexed (#100)
- MCP `get_latest_session`: restored notes are labelled as historical and followed by a separate "Code graph: indexed / not indexed" line, so session state is no longer read as code-index state (#100)
- SessionStart hook: restored notes carry their saved-at timestamp and a "historical, not current instructions" label, plus a separate code-graph readiness line; the PreToolUse enforce hook no longer redirects to infigraph tools in directories that have no code graph (#100)

## [3.2.17] - 2026-11-24

### Added

- Update notice in the MCP server: `initialize` now tells the agent when a newer release is available, so users who only run infigraph through an agent see it (#89)
- The update check no longer needs the GitHub CLI: it reads the public releases endpoint directly, with a `curl` fallback for networks that inspect TLS (#89)

### Changed

- Graph database upgraded to lbug (Kuzu) 0.19.1 (#70). Building from source now needs OpenSSL; see the README
- The ONNX-based `kompress` compression is opt-in (`--features kompress`), because the prebuilt ONNX Runtime needs newer x86 CPU instructions (#78)
- Release binaries link OpenSSL statically and need no OpenSSL on the user's machine; the macOS Intel build runs on a native Intel runner (#98)
- Installed `CLAUDE.md` blocks and hook messages reworded to plain, explained guidance (#85)

### Fixed

- A process now shares one database handle per graph: a read racing a watcher write in the MCP server could crash the process (#94)
- Web UI/API and MCP HTTP servers bind to loopback by default; set `INFIGRAPH_UI_BIND` / `INFIGRAPH_MCP_BIND` to expose them (ADV-2598, #78)
- Indexing no longer panics on a long condition containing non-ASCII text (#88)
- A corrupt embeddings file is rejected instead of requesting a huge allocation (#82)
- `--mcp` alone no longer starts the UI keep-alive loop (#57)
- Unchanged files are re-extracted when the extractor changes, not only when the file does (#71)
- Call resolution unified, removing non-deterministic edges; cross-repo namespace-link queries scoped per repo in remote mode
- OpenCode and Copilot MCP configs corrected (#56)
- Windows: the graph write lock reports who holds it (#90)
- `release.sh` refuses a tag that doesn't match the `Cargo.toml` version (#52)

## [3.2.16] - 2026-08-18

### Added

- C++: cross-repo namespace-qualified call linking through `group build`/`group link` (`feat(multi)`, `feat/cpp-grammer`)
- gRPC: detect C++ `{Service}::NewStub` client construction as a cross-service edge
- `group visualize` CLI command (repo-overview graph)
- Call resolution: track unresolved calls with a known receiver as `EXTERNAL_CALL` instead of dropping them
- MCP: `expand_interface` option on `trace_callers`/`transitive_impact`

### Fixed

- C++: proxy-ID collision and over-counting in cross-repo namespace linking; module-scope variables/consts extracted even inside include guards; namespace-block functions correctly scoped in symbol IDs; receiver-type gaps causing missing `CALLS` edges
- Java: `method_reference` (`Foo::bar`, `this::bar`) now captured as call edges
- Kotlin: bare/type-qualified method references (`::foo`) now captured
- C#: `this`/`base` receiver calls and property-accessor call sites resolved; delegate-constructor method refs and local same-class call edges resolved; fields and constructors captured with field-receiver resolution; Cozo schema parity fix
- Call resolution: same-class call fallback now gated on receiver + callable kind (was over-matching)
- Dead-code detection: suppress vendor-path and interface/impl-split false positives
- Route detection: reject comment-syntax noise as a false-positive URL path
- Multi-repo: HTTP/gRPC cross-service edges now carry a `protocol` field; `registry_path` reuse fix; stray env var cleanup
- `INFIGRAPH_REGISTRY_HOME` override added to prevent test/perf runs from clobbering the real global registry

Note: `3.2.6`–`3.2.15` were released without changelog entries; this entry covers commits since the `3.2.15` version bump (`67e83dd`) only, not the full gap.

## [3.2.5] - 2026-07-21

### Fixed

- Remote (shared-Neo4j) mode, follow-ups found in production testing:
  - `get_symbols_in_file`, `get_skeleton`, `symbols_in_range`, and `get_file_deps`
    now accept a repo-relative file path (not just the fully namespaced one) by
    normalizing it against the repo namespace — previously a bare path returned
    "not indexed" even though the file was indexed.
  - `get_symbols_in_file` output now includes each symbol's `id`, which is the
    argument the traversal tools (`trace_callers`, `get_code_snippet`,
    `symbol_context`, `find_all_references`, …) require — they were undiscoverable.
  - `get_code_snippet` strips the `org/repo/` namespace before joining to the
    project root, fixing a double-prefixed path that made source reads fail.
  - `detect_config_bindings` is repo-scoped (was returning other projects' bindings).
  - Contract sync into Postgres is chunked to stay under the 65535 bound-parameter
    limit — large groups previously failed with "parameters is not drained",
    dropping cross-service contract data.

## [3.2.4] - 2026-07-21

### Fixed

- Remote (shared-Neo4j) mode: MCP read tools no longer return another project's
  data or `Symbols: 0`. Read paths that queried the global graph are now scoped
  to the repo (`org/repo`), resolved from the group registry:
  - `apply_repo_filter` (MCP reader) resolves the repo namespace from the group
    registry instead of guessing from `INFIGRAPH_ORG`, fixing `Symbols: 0`
    (with `Folders`/`Contains` still populated) when the env org didn't match.
  - `detect_cross_cutting`, `detect_routes`, and `search`/`semantic_search`
    (remote) now return only the queried repo instead of all repos.
  - `get_dependencies` is scoped through the repo's own modules, and the
    `DEPENDS_ON` edge is written only against in-repo modules (previously
    `m.file CONTAINS 'package.json'` cross-linked every repo's manifest).
  - `detect_clusters` scopes CALLS edges and symbols to the repo.
  - `GraphBackend` gains a `repo_filter()` accessor so backend-agnostic analysis
    passes can scope their own Cypher.

## [3.2.3] - 2026-07-21

### Fixed

- Remote (shared-Neo4j) mode: a single-repo webhook no longer re-indexes every
  repo in the group, and combined-graph queries no longer return 0 while
  per-repo queries succeed. Root cause was read/write disagreement on repo
  identity plus a Kùzu-only transaction statement leaking into Neo4j:
  - Webhook now pulls and runs `group build` only — the standalone `index`
    step (wrong namespace + stole the commit-change signal) is removed.
  - `f.repo` is stamped as `org/repo` at write time in both the per-file and
    bulk write paths; the global unfiltered `f.repo` backfill in `upsert_repo`
    (which stole orphan files across repos) is removed.
  - Group indexing scopes reads to the repo being indexed, so reindexing one
    repo no longer deletes every other repo's data from the shared graph.
  - Read filters resolve the same `org/repo` key that writes use, looked up
    from the group registry (source of truth) rather than derived from
    `INFIGRAPH_ORG`. Fixes MCP tools reporting `Symbols: 0 / Files: 0` (with
    globally-counted `Folders`/`Contains` still populated) when the server's
    `INFIGRAPH_ORG` didn't match the org a repo was indexed under.
  - `BEGIN TRANSACTION`/`COMMIT`/`ROLLBACK` are no-ops on the Neo4j backend
    (valid Kùzu, invalid Cypher), fixing concern/taint/reflection/config/
    dynamic-URL analysis in remote mode.
  - Org-scoped groups are usable from the CLI: `group add`/`build`/`index`
    resolve a bare group name to its org-qualified key.
- Remote mode `index` now resolves a repo's `org/repo` namespace from the group
  registry and refuses to index a repo that isn't registered in any group,
  instead of inventing a namespace from the directory name.

## [0.10.1] - 2026-05-11

### Added

- Pascal/Delphi language support (.pas, .pp, .dpr, .dpk, .lpr)
- VB6 language support (.bas, .cls, .frm)
- 30 MCP tools for AI coding agents (Claude Code, Cursor, Copilot, and more)
- Web UI at localhost:9749 with graph explorer, route map, and multi-repo groups
- Multi-repo groups with cross-service HTTP dependency detection
- SCIP index import for compiler-grade symbol enrichment
- lsp-to-scip bridge for any LSP server (C/C++, Zig, Swift, Dart, Elixir, and more)
- Dead code detection, blast radius analysis, git diff impact mapping
- Louvain community detection for functional module discovery
- HTTP route/endpoint detection across 22 frameworks

### Changed

- Graph database migrated to LadybugDB (lbug 0.16, maintained Kuzu fork)
- Embedding model: potion-base-8M 256-dim, bundled — no network or proxy required

[Unreleased]: https://github.com/intuit/infigraph/compare/v0.10.1...HEAD
[0.10.1]: https://github.com/intuit/infigraph/releases/tag/v0.10.1
