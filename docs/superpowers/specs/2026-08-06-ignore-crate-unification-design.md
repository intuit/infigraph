# Shared Ignore-Rules Component — Design

## Background

Infigraph has five independent places that decide "which directories/files
should I skip while walking or watching a project," and only one of them is
actually correct:

1. `crates/infigraph-core/src/lib.rs::collect_files` — the real file
   discovery behind `Infigraph::index()`. Already uses `ignore::WalkBuilder`
   with `.hidden(true)`, `.git_ignore(true)`, and
   `.add_custom_ignore_filename(".infigraphignore")`, plus a small hardcoded
   `filter_entry` safety list (`.infigraph`, `node_modules`, `__pycache__`,
   `.tox`). This one genuinely respects `.gitignore`/`.infigraphignore`.
2. `crates/infigraph-core/src/watch/mod.rs::should_ignore` (used by
   `watch_project_with_periodic`'s notify-event filter and by
   `register_watch_dirs`/`register_subdirs`'s directory-registration walk) —
   a hardcoded list (`.infigraph`, `.git`, `node_modules`, `__pycache__`,
   `.venv`, `venv`, `target`, `build`, `dist`, `.tox`), no gitignore
   awareness at all.
3. `crates/infigraph-docs/src/lib.rs::walk_doc_dir` — the same style of
   hardcoded list, independently maintained, no gitignore awareness.
4. `crates/infigraph-core/src/search/mod.rs::IGNORE_DIRS` (used by
   `grep_search`) — another independent hardcoded list.
5. `crates/infigraph-core/src/security/detect.rs::IGNORE_DIRS` (used by
   `walk_and_scan`) — a longer independent hardcoded list (also `vendor`,
   `.idea`, `.mypy_cache`, `coverage`, `.pytest_cache`).

`docs/CODE-PARSING.md`'s own "File Discovery" section documents the
hardcoded-list behavior as if it were current for code indexing — it is not;
it describes an earlier implementation that `collect_files` has since moved
past, and the doc was never updated.

### The incident that surfaced this

On 2026-08-06, this repository's own doc-watch daemon got stuck in an
infinite loop: `[doc-watch-daemon] document change detected, reindexing...`
followed immediately by `reindexed: 0 files, 0 chunks`, repeating
continuously for hours. Root cause: `scratchpad/` — this repo's own
gitignored convention for agent worktree scratch space, populated with 9+
full copies of `docs/` from that session's dispatched agents — is not in
`walk_doc_dir`'s hardcoded ignore list and does not start with `.`, so it
gets walked, indexed as real project content, and watched. Any file touch
anywhere under any live `scratchpad/wt-*/` worktree re-triggers the watcher,
which reindexes the (unchanged) scratchpad copies, finds nothing new
(`0 chunks`), and never advances `docs_embeddings.bin`'s freshness — the
doctor tool's "stale sidecar" warning is a symptom of this loop, not an
independent bug.

Investigating further surfaced a second, more serious gap: `index_files()`
(`crates/infigraph-core/src/lib.rs::index_files`), the incremental per-path
indexer the watcher's drain step calls, does **not** re-check ignore rules
on the paths it's given — it trusts its caller entirely. Since the code
watcher's `should_ignore` also lacks `scratchpad` (and lacks any real
gitignore awareness), a live edit under `scratchpad/wt-*/` is not just a
wasted reindex cycle the way the doc case is — it can actually be written
into the *main* project's code graph via the incremental path, something a
full `infigraph index --full` (which goes through the correct
`collect_files`) would never have included in the first place.

## Goal

Replace all 5 ignore-decision sites with one shared, `.gitignore`- and
`.infigraphignore`-aware component, so every one of them behaves like
`collect_files` already does, and a project-specific gitignored convention
(like `scratchpad/`) is honored everywhere by construction, not by
remembering to add it to N separate lists.

## Non-goals

- **Not hardening `index_files()` itself.** The fix is to stop the watcher
  from ever enqueueing an ignored path (its directory-registration and
  event-filtering layers, described below); `index_files()` continues to
  trust its caller, as today. Explicitly decided against adding a second
  ignore check at the ingestion boundary — the watcher's enqueue path is
  the single source of truth for what gets queued.
- **Not changing what counts as ignorable via `.gitignore`/
  `.infigraphignore` semantics.** These are consumed exactly as the `ignore`
  crate already interprets them for `collect_files` today — no
  Infigraph-specific dialect.

## Architecture

New module: `crates/infigraph-core/src/ignore_rules.rs`.

`infigraph-docs` already depends on `infigraph-core` (see its `Cargo.toml`),
so no new crate or dependency-graph change is needed — the `ignore` crate
is already a dependency of `infigraph-core` (used today by `collect_files`).

### Safety list

A single `const IGNORE_SAFETY_LIST: &[&str]` — the **union** of all 5
current lists, so unifying them cannot silently regress protection in a repo
whose own `.gitignore` happens to be sparse:

```
.infigraph, .git, node_modules, __pycache__, .venv, venv, target, build,
dist, .tox, vendor, .idea, .mypy_cache, coverage, .pytest_cache
```

This list is excluded unconditionally, regardless of what any `.gitignore`
or `.infigraphignore` says (a project without `.infigraph` in its own
`.gitignore` must not have Infigraph recursively index its own index state).
Everything else — including project-specific conventions like `scratchpad/`
— is governed by real `.gitignore`/`.infigraphignore` rules.

### Two consumption forms, one configuration

```rust
/// Pre-configured WalkBuilder for directory-tree walks. Caller may add
/// further config (e.g. max_depth) before calling .build().
pub fn walk_builder(root: &Path) -> ignore::WalkBuilder { ... }

/// Point-wise matcher for a single path (no tree to walk) — e.g. a notify
/// event. Rebuild when the underlying ignore files may have changed.
pub struct IgnoreMatcher(ignore::gitignore::Gitignore);
impl IgnoreMatcher {
    pub fn build(root: &Path) -> Self { ... }
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool { ... }
}
```

Both are built from the same root, the same `.infigraphignore` custom
filename, and the same safety list — one place defines "ignored," exposed
two ways depending on whether the caller has a tree to walk or a single path
to check.

## Call-site changes

1. **`collect_files`** (`crates/infigraph-core/src/lib.rs`) — replace its
   inline `WalkBuilder` construction with `ignore_rules::walk_builder(&self.root)`.
   Behavior-preserving: its current filter list is a subset of the new
   union, so nothing it currently includes starts being excluded, and
   nothing it currently indexes changes.
2. **`walk_doc_dir`** (`crates/infigraph-docs/src/lib.rs`) — replace the
   hand-rolled recursive `read_dir` walk with
   `ignore_rules::walk_builder(&self.root).build()`, filtered to
   `is_document_file` matches. This is the direct fix for the incident.
3. **`watch_project_with_periodic`** (`crates/infigraph-core/src/watch/mod.rs`)
   — two changes:
   - `register_watch_dirs`/`register_subdirs` use `ignore_rules::walk_builder`
     to decide which subdirectories to call `watcher.watch()` on, so an
     ignored tree (e.g. `scratchpad/`) is never subscribed to in the first
     place — this is what closes the `index_files()` incremental-leak gap,
     since a path that's never watched can never be enqueued.
   - `should_ignore`'s hardcoded-list check is replaced by an
     `IgnoreMatcher`, built once at watcher startup and rebuilt on the
     loop's existing `periodic_secs` tick (no new timer), checked per notify
     event as a second layer — this catches an ignore-file edit made while
     the watcher is already running, and covers events on paths that
     `register_subdirs` didn't anticipate (e.g. a new top-level directory
     appearing).
4. **`grep_search`** (`crates/infigraph-core/src/search/mod.rs`) — replace
   the `IGNORE_DIRS`-based walk with `ignore_rules::walk_builder`.
5. **`walk_and_scan`** (`crates/infigraph-core/src/security/detect.rs`) —
   same replacement.

## Documentation fix

`docs/CODE-PARSING.md`'s "File Discovery" → "Ignored directories" section
currently states the hardcoded-list behavior as current for code discovery.
Correct it to describe the real `ignore`-crate-based mechanism (safety list
+ `.gitignore` + `.infigraphignore`), matching what `collect_files` has
actually done for some time. `docs/DOCUMENT-INDEXING.md`'s equivalent
section gets the same correction once `walk_doc_dir` is fixed.

## Testing

- New unit tests for `ignore_rules` directly: a fixture tree with a
  `.gitignore`, an `.infigraphignore`, and a `scratchpad/`-style directory —
  assert both `walk_builder` and `IgnoreMatcher` agree on what's excluded.
- Update `test_code_watcher_ignores_excluded_dirs`
  (`crates/infigraph-mcp/tests/watcher_reindex.rs`) and the doc-watcher
  equivalent to additionally assert a *gitignored, non-hardcoded* directory
  (e.g. `scratchpad/`) is skipped — the direct regression test for this
  incident.
- Existing tests asserting the old hardcoded-list names (`node_modules`,
  `.git`, etc.) should pass unmodified, since the union safety list is a
  superset of every list being replaced.
- A test asserting the watcher's `IgnoreMatcher` picks up a `.gitignore`
  edit made mid-run, within one `periodic_secs` tick.

## Migration / compatibility notes

This is destined for an upstream PR to `github.com/intuit/infigraph`. Local
`main` was fast-forwarded to `upstream/main` (`e72a6ae`, `v3.2.10`) and
pushed to `origin/main` before this design was written, so the feature
branch for implementation should fork from current `main`, not
`feat/hardening`.
