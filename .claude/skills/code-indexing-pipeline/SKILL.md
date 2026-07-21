---
name: code-indexing-pipeline
description: How Infigraph turns source into a graph — adding a language (tree-sitter vs ANTLR grammar-plugin), cross-file call resolution, SCIP compiler-grade enrichment, and file-watch/reindex triage. Use when adding language support, debugging unresolved calls or SCIP import, or triaging stale index/watcher issues.
---

# Code indexing pipeline

Source → tree-sitter/grammar-plugin extraction → cross-file resolution → graph → optional SCIP enrichment → file-watch keeps it fresh. One skill since these stages feed each other directly.

## Adding a language

If a `tree-sitter-<lang>` crate exists, use the **tree-sitter path**: `crates/infigraph-languages/languages/<lang>/` needs `lang.toml` (metadata + optional custom edge-kind declarations), `entities.scm` (symbol queries), `relations.scm` (call/import/inheritance queries — calls can capture a receiver, feeding cross-file resolution below). Registration happens in `infigraph-languages`'s bundled registry; a pack that fails to load is skipped with a warning, not a hard error.

If no tree-sitter grammar exists, use the **ANTLR grammar-plugin path**: `.g4` grammars + a Java extractor + a config file, discovered from a directory, running via a shared JVM subprocess — no Rust recompile either way (see `GRAMMAR_PLUGINS.md`). Plugins are discovered from a bundled dir, user-level dir, then project-level dir, in that order — later registration for the same extension overrides earlier, so a grammar plugin can supersede tree-sitter handling if needed.

After adding: index the test fixtures and confirm symbols/relations show up via search. If the language exposes HTTP routes, check it against the route-coverage table in the README.

## Cross-file call resolution

Core logic: `crates/infigraph-core/src/resolve/`. Matching strategy, in order:

1. **Learned cache first** — if a prior SCIP-derived correction exists for this call site with enough confidence, use it directly.
2. **Receiver-aware resolution** — if the call has an `obj.method()`-style receiver, match against known class methods, preferring one reachable via the caller's imports.
3. **Enclosing-class / import-scope fallback** — otherwise, gather same-named candidates from other files and disambiguate by receiver type, enclosing class, then import scope.

**No guessing on ambiguity**: if nothing disambiguates, the call is left unresolved rather than pointed at an arbitrary candidate.

The learned-resolution cache persists separately from the graph DB (survives a full reindex, same as sessions) — once SCIP corrects a call, later plain-tree-sitter reindexes reproduce that correction, since the cache is checked first.

Debugging: check resolve stats for unresolved-call counts. A call resolving to the wrong file usually means multiple same-named candidates without a disambiguating signal — expected, not a bug, unless a real tie-breaker exists to add.

## SCIP enrichment

Flow: detect languages present → select/download the matching SCIP indexer binary (self-provisions any runtime it needs) → run it (background after `infigraph index`, or foreground) → import the `.scip` file, enriching existing tree-sitter symbols in place and adding CALLS edges from SCIP reference data.

**Merge semantics — augment, never replace.** When SCIP-derived reference data disagrees with an existing tree-sitter edge, the discrepancy is recorded as a learned correction (feeding the cache above) and a new edge is added — the old edge stays, nothing is retroactively rewritten.

For languages without a dedicated SCIP indexer, a generic LSP bridge (`crates/lsp-to-scip`) spawns any LSP server and emits a SCIP index from `documentSymbol`/`references` requests — it only captures same-file references reliably, cross-file linking for these languages still depends on tree-sitter's own resolution.

Adding a new indexer: add a catalog entry with the right download strategy; a new runtime type needs the runtime-provisioning code extended too.

Debugging a failed/silent enrichment: check the background enrichment log first. A skipped indexer (missing project precondition) isn't a bug. If enrichment appears to never run, confirm the child process actually launched and exited cleanly rather than assuming the log's mere existence means success.

## Debugging indexing/watch issues

Events batch over a short debounce window before triggering reindex. The watch root is canonicalized before comparison (some backends deliver symlink-resolved absolute paths regardless of how the root was specified). A cross-process lock file prevents two CLI/MCP processes from double-watching the same project.

Checklist:
1. **Is a watcher running?** Check the lock file's holder, or use the MCP watch-status tool.
2. **Why isn't a changed file triggering reindex?** In order of likelihood: unrecognized file extension (silently dropped, no log line), file under an ignored directory, a batch/reindex failure logged by the watcher, or — cross-file calls changed with auto-resolve off (expected, needs a manual reindex).
3. **Watcher stopped entirely?** The underlying OS watch can crash and retry a bounded number of times before giving up permanently — needs a manual restart.
4. **FD-leak reports (macOS)** — check whether a kqueue-based watch backend is enabled anywhere in the dependency tree via feature unification; FSEvents (the default) should be used instead.
