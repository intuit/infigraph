# Shared Ignore-Rules Component Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 5 independently-hardcoded ignore-directory lists across Infigraph's file walkers and watcher with one shared, `.gitignore`- and `.infigraphignore`-aware component, so a project convention like `scratchpad/` is honored everywhere by construction.

**Architecture:** New `crates/infigraph-core/src/ignore_rules.rs` module exposes `walk_builder(root)` (a pre-configured `ignore::WalkBuilder` for directory-tree walks) and `IgnoreMatcher` (a point-wise matcher for single paths, e.g. watcher events), both built from the same safety list + `.gitignore` + `.infigraphignore` configuration. Five call sites migrate to it one at a time, each independently testable.

**Tech Stack:** Rust, the `ignore` crate (already a dependency of `infigraph-core`, version `"0.4"` — see `crates/infigraph-core/Cargo.toml:39`). No new dependencies.

## Global Constraints

- The `ignore` crate is already a dependency of `infigraph-core` (`ignore = "0.4"`) — do not add it again or bump its version.
- `infigraph-docs` already depends on `infigraph-core` (`crates/infigraph-docs/Cargo.toml:14`) — no new crate dependency needed for Task 3.
- The safety list (directories always excluded regardless of any ignore file) is the **union** of all 5 current lists:
  `.infigraph`, `.git`, `node_modules`, `__pycache__`, `.venv`, `venv`, `target`, `build`, `dist`, `.tox`, `vendor`, `.idea`, `.mypy_cache`, `coverage`, `.pytest_cache`.
- `index_files()` (`crates/infigraph-core/src/lib.rs::index_files`) is explicitly **not** touched by this plan — it continues to trust its caller. The fix is entirely in the walkers/watcher that decide what to enqueue.
- `docs/CODE-PARSING.md` and `docs/DOCUMENT-INDEXING.md` get corrected as part of this plan (Task 7) — they currently describe the old hardcoded-list behavior as current, which is stale.
- This branch (`feat/gitignore-aware-file-discovery`) was forked from `main` at `e72a6ae` (fast-forwarded from `upstream/main`), for eventual upstream PR submission — commits should stand alone cleanly, without referencing `feat/hardening`-specific context (e.g. do not add `/scratchpad/` to this repo's own `.gitignore` as part of this work; that's fork-specific, not upstream-worthy).

---

### Task 1: Build the shared `ignore_rules` module

**Files:**
- Create: `crates/infigraph-core/src/ignore_rules.rs`
- Modify: `crates/infigraph-core/src/lib.rs:13-14` (add `pub mod ignore_rules;` between `pub mod graph;` and `pub mod lang;`, alphabetical order)
- Test: inline `#[cfg(test)] mod tests` in `crates/infigraph-core/src/ignore_rules.rs`

**Interfaces:**
- Produces: `pub const IGNORE_SAFETY_LIST: &[&str]`, `pub fn walk_builder(root: &Path) -> ignore::WalkBuilder`, `pub struct IgnoreMatcher` with `pub fn build(root: &Path) -> Self` and `pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool`.

- [ ] **Step 1: Write the failing tests**

Create `crates/infigraph-core/src/ignore_rules.rs`:

```rust
//! Shared, .gitignore- and .infigraphignore-aware ignore rules used by
//! every directory walker and the file watcher, so a project convention
//! excluded via .gitignore (or .infigraphignore) is honored everywhere
//! consistently, instead of each call site maintaining its own hardcoded
//! directory-name list.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

/// Directories always excluded, regardless of what any .gitignore or
/// .infigraphignore says. Union of every previously-independent hardcoded
/// list this module replaces (collect_files, the watcher, doc indexing,
/// grep search, security scanning) -- unifying them must not silently
/// reduce protection in a repo whose own .gitignore happens to be sparse.
pub const IGNORE_SAFETY_LIST: &[&str] = &[
    ".infigraph",
    ".git",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "target",
    "build",
    "dist",
    ".tox",
    "vendor",
    ".idea",
    ".mypy_cache",
    "coverage",
    ".pytest_cache",
];

fn is_safety_excluded(name: &str) -> bool {
    IGNORE_SAFETY_LIST.contains(&name)
}

/// A pre-configured `WalkBuilder` for `root`: respects `.gitignore`,
/// `.infigraphignore`, and the safety list above. Callers may add further
/// configuration (e.g. `.max_depth`) before calling `.build()`.
pub fn walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .add_custom_ignore_filename(".infigraphignore")
        .filter_entry(|entry| !is_safety_excluded(&entry.file_name().to_string_lossy()));
    builder
}

/// Point-wise matcher for a single path (e.g. a file-watcher event), where
/// there's no directory tree to walk. Built from the same safety list and
/// the same `.gitignore`/`.infigraphignore` files `walk_builder` would
/// discover -- rebuild when those files may have changed (the watcher
/// rebuilds this on its periodic tick; see `watch_project_with_periodic`).
pub struct IgnoreMatcher {
    gitignore: Gitignore,
}

impl IgnoreMatcher {
    /// Discovers every `.gitignore`/`.infigraphignore` under `root`
    /// (skipping the safety list, same as `walk_builder`, so this never
    /// wastes time descending into e.g. `node_modules/` hunting for nested
    /// ignore files there -- nothing inside is ever relevant since the
    /// whole directory is always excluded), then builds one matcher from
    /// all of them. `.hidden(false)` here (unlike `walk_builder`) because
    /// the ignore files themselves are dot-prefixed and must be visited as
    /// walk results to be found; `.git_ignore(true)` still prunes any
    /// subtree an already-discovered ancestor `.gitignore` excludes, so
    /// this stays proportional to directory count, not full file count.
    pub fn build(root: &Path) -> Self {
        let mut gi_builder = GitignoreBuilder::new(root);

        let mut discovery = WalkBuilder::new(root);
        discovery
            .hidden(false)
            .git_ignore(true)
            .add_custom_ignore_filename(".infigraphignore")
            .filter_entry(|entry| !is_safety_excluded(&entry.file_name().to_string_lossy()));

        for result in discovery.build() {
            let Ok(entry) = result else { continue };
            let name = entry.file_name().to_string_lossy();
            if name == ".gitignore" || name == ".infigraphignore" {
                let _ = gi_builder.add(entry.path());
            }
        }

        let gitignore = gi_builder.build().unwrap_or_else(|_| Gitignore::empty());
        IgnoreMatcher { gitignore }
    }

    /// True if `path` should be excluded -- either via the safety list
    /// (checked against every path component, so a nested occurrence like
    /// `foo/node_modules/bar` is still caught) or via a discovered
    /// `.gitignore`/`.infigraphignore` rule.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        if path
            .components()
            .any(|c| is_safety_excluded(&c.as_os_str().to_string_lossy()))
        {
            return true;
        }
        self.gitignore.matched(path, is_dir).is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "scratchpad/\n*.log\n").unwrap();
        fs::create_dir_all(dir.path().join("scratchpad/wt-foo")).unwrap();
        fs::write(dir.path().join("scratchpad/wt-foo/README.md"), "# copy").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("debug.log"), "noisy").unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), "//").unwrap();
        dir
    }

    #[test]
    fn walk_builder_skips_gitignored_scratchpad_and_safety_list() {
        let dir = make_fixture();
        let mut found = Vec::new();
        for result in walk_builder(dir.path()).build() {
            let entry = result.unwrap();
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                found.push(entry.path().to_path_buf());
            }
        }
        assert!(found.iter().any(|p| p.ends_with("src/main.rs")));
        assert!(
            !found.iter().any(|p| p.to_string_lossy().contains("scratchpad")),
            "scratchpad/ is gitignored and must not be walked: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.to_string_lossy().contains("node_modules")),
            "node_modules/ is in the safety list and must not be walked: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.ends_with("debug.log")),
            "*.log is gitignored and must not be walked: {found:?}"
        );
    }

    #[test]
    fn ignore_matcher_agrees_with_walk_builder() {
        let dir = make_fixture();
        let matcher = IgnoreMatcher::build(dir.path());

        assert!(!matcher.is_ignored(&dir.path().join("src/main.rs"), false));
        assert!(matcher.is_ignored(&dir.path().join("scratchpad/wt-foo/README.md"), false));
        assert!(matcher.is_ignored(&dir.path().join("scratchpad"), true));
        assert!(matcher.is_ignored(&dir.path().join("debug.log"), false));
        assert!(matcher.is_ignored(&dir.path().join("node_modules/pkg/index.js"), false));
    }

    #[test]
    fn infigraphignore_is_honored_like_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".infigraphignore"), "vendored/\n").unwrap();
        fs::create_dir_all(dir.path().join("vendored")).unwrap();
        fs::write(dir.path().join("vendored/lib.rs"), "// vendored").unwrap();
        fs::write(dir.path().join("real.rs"), "fn f() {}").unwrap();

        let matcher = IgnoreMatcher::build(dir.path());
        assert!(matcher.is_ignored(&dir.path().join("vendored/lib.rs"), false));
        assert!(!matcher.is_ignored(&dir.path().join("real.rs"), false));

        let mut found = Vec::new();
        for result in walk_builder(dir.path()).build() {
            let entry = result.unwrap();
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                found.push(entry.path().to_path_buf());
            }
        }
        assert!(!found.iter().any(|p| p.to_string_lossy().contains("vendored")));
        assert!(found.iter().any(|p| p.ends_with("real.rs")));
    }
}
```

- [ ] **Step 2: Register the module and run the tests to verify they fail to compile (module doesn't exist yet in lib.rs)**

In `crates/infigraph-core/src/lib.rs`, add this line between `pub mod graph;` and `pub mod lang;`:

```rust
pub mod ignore_rules;
```

Run: `cargo test -p infigraph-core ignore_rules:: -- --nocapture`
Expected: compiles and PASSES immediately, since the module's own implementation is written in Step 1 — this task has no red-then-green step because the module is self-contained (no other code depends on it yet). Confirm all 3 tests pass.

- [ ] **Step 3: Run the full infigraph-core test suite to confirm nothing else broke**

Run: `cargo test -p infigraph-core`
Expected: PASS (this task only adds a new module and one `pub mod` line; nothing existing is touched yet).

- [ ] **Step 4: Commit**

```bash
git add crates/infigraph-core/src/ignore_rules.rs crates/infigraph-core/src/lib.rs
git commit -m "feat: add shared gitignore/.infigraphignore-aware ignore_rules module"
```

---

### Task 2: Migrate `collect_files` to the shared module

**Files:**
- Modify: `crates/infigraph-core/src/lib.rs` (`collect_files`, currently ~L825-855)

**Interfaces:**
- Consumes: `crate::ignore_rules::walk_builder` from Task 1.

- [ ] **Step 1: Replace `collect_files`'s inline `WalkBuilder` construction**

Find `collect_files` (search for `fn collect_files(&self) -> Result<Vec<PathBuf>>`) and replace its body:

```rust
fn collect_files(&self) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = crate::ignore_rules::walk_builder(&self.root).build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            let path = entry.path();
            if self.registry.for_file(&path.to_string_lossy()).is_some() {
                files.push(path.to_path_buf());
            }
        }
    }
    Ok(files)
}
```

This removes the function's local `use ignore::WalkBuilder;` and its own filter_entry closure — both now live in `ignore_rules`. Behavior-preserving: `collect_files`'s old safety list (`.infigraph`, `node_modules`, `__pycache__`, `.tox`) is a subset of the new union list, so nothing it used to index becomes excluded.

- [ ] **Step 2: Run the existing test suite covering `collect_files` to confirm no regression**

Run: `cargo test -p infigraph-core index_perf:: facade_integration:: -- --nocapture` (and any other test names containing `collect_files`, `index_incremental`, or `scan_changed_files` — grep the test names via `mcp__infigraph__search_symbols` if unsure which files cover it)
Expected: PASS, identical results to before this change.

- [ ] **Step 3: Run the full infigraph-core test suite**

Run: `cargo test -p infigraph-core`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/infigraph-core/src/lib.rs
git commit -m "refactor: collect_files uses the shared ignore_rules::walk_builder"
```

---

### Task 3: Migrate doc indexing (`walk_doc_dir`) — the direct incident fix

**Files:**
- Modify: `crates/infigraph-docs/src/lib.rs` (`collect_doc_files` at ~L314-318, `walk_doc_dir` at ~L320-349 — the latter is deleted entirely)
- Test: `crates/infigraph-docs/tests/modules.rs` (extend `test_docindex_ignores_hidden_and_build_dirs`, ~L669-690)

**Interfaces:**
- Consumes: `infigraph_core::ignore_rules::walk_builder` from Task 1.

- [ ] **Step 1: Write the failing test**

In `crates/infigraph-docs/tests/modules.rs`, extend `test_docindex_ignores_hidden_and_build_dirs` to also cover a gitignored, non-hardcoded directory — this is the direct regression test for the 2026-08-06 incident:

```rust
#[test]
fn test_docindex_ignores_hidden_and_build_dirs() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config.txt"), "git config").unwrap();

    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/readme.md"), "# Pkg").unwrap();

    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/output.txt"), "build output").unwrap();

    // A project-specific gitignored convention (e.g. an agent worktree
    // scratch directory) is NOT in any hardcoded list -- only a real
    // .gitignore rule can exclude it. Regression test for the 2026-08-06
    // incident where scratchpad/ was walked and indexed as real content,
    // causing the doc watcher to loop forever re-indexing 0 changed chunks.
    std::fs::write(dir.path().join(".gitignore"), "scratchpad/\n").unwrap();
    std::fs::create_dir_all(dir.path().join("scratchpad/wt-foo")).unwrap();
    std::fs::write(dir.path().join("scratchpad/wt-foo/copy.md"), "# Copy").unwrap();

    std::fs::write(dir.path().join("real.md"), "# Real Doc\n\nContent.\n").unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();
    assert_eq!(
        result.total_files, 1,
        "should only find real.md, not files in ignored or gitignored dirs"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p infigraph-docs test_docindex_ignores_hidden_and_build_dirs -- --nocapture`
Expected: FAIL — `result.total_files` is `2` (real.md + scratchpad/wt-foo/copy.md), since `walk_doc_dir` doesn't consult `.gitignore` yet.

- [ ] **Step 3: Replace `collect_doc_files` and delete `walk_doc_dir`**

Find `collect_doc_files` (search for `fn collect_doc_files(&self) -> Result<Vec<PathBuf>>`) and replace it, then delete the `walk_doc_dir` method entirely:

```rust
fn collect_doc_files(&self) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = infigraph_core::ignore_rules::walk_builder(&self.root).build();
    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            let path = entry.path().to_path_buf();
            if is_document_file(&path) {
                files.push(path);
            }
        }
    }
    Ok(files)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p infigraph-docs test_docindex_ignores_hidden_and_build_dirs -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full infigraph-docs test suite**

Run: `cargo test -p infigraph-docs`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-docs/src/lib.rs crates/infigraph-docs/tests/modules.rs
git commit -m "fix: doc indexing honors .gitignore/.infigraphignore via shared ignore_rules

Fixes the 2026-08-06 incident where scratchpad/ (a gitignored agent
worktree convention, not in any hardcoded list) was walked and indexed
as real document content, causing the doc watcher to loop forever
re-indexing 0 changed chunks and never advancing docs_embeddings.bin."
```

---

### Task 4: Migrate the code watcher (directory registration + event filter)

**Files:**
- Modify: `crates/infigraph-core/src/watch/mod.rs` (`watch_project_with_periodic` ~L112-593, `should_ignore` ~L1582-1587 deleted, `register_watch_dirs`/`register_subdirs` ~L503-531 merged into one function)
- Test: `crates/infigraph-mcp/tests/watcher_reindex.rs` (extend `test_code_watcher_ignores_excluded_dirs`, ~L773-839)

**Interfaces:**
- Consumes: `crate::ignore_rules::walk_builder`, `crate::ignore_rules::IgnoreMatcher` from Task 1.
- Produces: `register_watch_dirs(watcher: &mut RecommendedWatcher, root: &Path) -> Result<()>` (signature changes — drops the `ignore_dirs: &[&str]` parameter). `register_subdirs` is deleted; its recursion is now internal to `ignore::Walk`.

- [ ] **Step 1: Write the failing test**

In `crates/infigraph-mcp/tests/watcher_reindex.rs`, extend `test_code_watcher_ignores_excluded_dirs` to also cover a gitignored, non-hardcoded directory:

```rust
#[test]
fn test_code_watcher_ignores_excluded_dirs() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def main(): pass")]);

    // A project-specific gitignored convention, not in any hardcoded list --
    // only a real .gitignore rule can exclude it. Regression coverage for
    // the 2026-08-06 incident: without this, a live edit under such a
    // directory could be written into the main project's graph via the
    // watcher's incremental index_files() path, which never re-checks
    // ignore rules on the paths it's handed.
    std::fs::write(
        std::path::PathBuf::from(&path).join(".gitignore"),
        "scratchpad/\n",
    )
    .unwrap();

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Create files in ignored directories
    std::thread::sleep(Duration::from_millis(500));
    let nm = std::path::PathBuf::from(&path).join("node_modules/pkg");
    std::fs::create_dir_all(&nm).unwrap();
    std::fs::write(nm.join("index.py"), "def ignored_nm_func(): pass\n").unwrap();

    let venv = std::path::PathBuf::from(&path).join(".venv/lib");
    std::fs::create_dir_all(&venv).unwrap();
    std::fs::write(venv.join("mod.py"), "def ignored_venv_func(): pass\n").unwrap();

    let scratchpad = std::path::PathBuf::from(&path).join("scratchpad/wt-foo");
    std::fs::create_dir_all(&scratchpad).unwrap();
    std::fs::write(
        scratchpad.join("copy.py"),
        "def ignored_scratchpad_func(): pass\n",
    )
    .unwrap();

    // Also add a legitimate file as control
    std::fs::write(
        std::path::PathBuf::from(&path).join("src/legit.py"),
        "def legit_not_ignored(): return True\n",
    )
    .unwrap();

    // Control should be found
    let found_legit = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "legit_not_ignored"}))
                .map(|r| r.contains("legit_not_ignored"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "legit_not_ignored should be searchable",
    );

    // Wait a bit more then check ignored files are NOT in the graph index
    // Use tool_search_symbols (graph-only) since tool_search includes grep fallback
    // that finds files on disk regardless of watcher indexing
    std::thread::sleep(Duration::from_secs(2));

    let found_nm = tool_search_symbols(&json!({"path": &path, "query": "ignored_nm_func"}))
        .map(|r| r.contains("ignored_nm_func"))
        .unwrap_or(false);

    let found_venv = tool_search_symbols(&json!({"path": &path, "query": "ignored_venv_func"}))
        .map(|r| r.contains("ignored_venv_func"))
        .unwrap_or(false);

    let found_scratchpad =
        tool_search_symbols(&json!({"path": &path, "query": "ignored_scratchpad_func"}))
            .map(|r| r.contains("ignored_scratchpad_func"))
            .unwrap_or(false);

    assert!(found_legit, "legitimate file should be indexed by watcher");
    assert!(
        !found_nm,
        "node_modules files should NOT be indexed by watcher"
    );
    assert!(!found_venv, ".venv files should NOT be indexed by watcher");
    assert!(
        !found_scratchpad,
        "gitignored scratchpad/ files should NOT be indexed by watcher, \
         even though it isn't in any hardcoded ignore list"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p infigraph-mcp test_code_watcher_ignores_excluded_dirs -- --nocapture --test-threads=1`
Expected: FAIL — `found_scratchpad` is `true`, since `should_ignore` doesn't consult `.gitignore` yet and `scratchpad/` isn't in its hardcoded list.

- [ ] **Step 3: Replace directory registration and the event-time filter**

In `crates/infigraph-core/src/watch/mod.rs`:

Delete the `ignore_dirs: &[&str] = &[...]` const block (currently ~L134-145).

Replace `register_watch_dirs` and delete `register_subdirs` entirely (currently ~L503-531):

```rust
fn register_watch_dirs(watcher: &mut RecommendedWatcher, root: &Path) -> Result<()> {
    for result in crate::ignore_rules::walk_builder(root).build() {
        let Ok(entry) = result else { continue };
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            let _ = watcher.watch(entry.path(), RecursiveMode::NonRecursive);
        }
    }
    Ok(())
}
```

(`ignore::Walk` yields `root` itself as its first entry, so this single loop covers what the old two-function split — one explicit `watcher.watch(root, ...)` call plus a hand-recursed `register_subdirs` — used to do.)

Delete the `fn should_ignore(path: &Path, ignore_dirs: &[&str]) -> bool { ... }` function (currently ~L1582-1587) — it has no remaining callers after this task.

In `watch_project_with_periodic`, find the `create_watcher` closure and update its signature and body:

```rust
let create_watcher = |root: &Path| -> Result<(RecommendedWatcher, mpsc::Receiver<notify::Result<Event>>)> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));
    let mut watcher = RecommendedWatcher::new(tx, config)?;
    register_watch_dirs(&mut watcher, root)?;
    Ok((watcher, rx))
};

let (mut watcher, mut rx) = create_watcher(root)?;
```

Update the restart call site further down (inside the `Err(mpsc::RecvTimeoutError::Disconnected)` arm):

```rust
match create_watcher(root) {
```

Add the point-wise matcher and its own rebuild timer near the other loop-local mutable state (alongside `let mut held_prism: Option<Arc<Infigraph>> = None;`):

```rust
let mut ignore_matcher = crate::ignore_rules::IgnoreMatcher::build(root);
let mut last_ignore_rebuild = std::time::Instant::now();
```

Near the top of the `loop { ... }` body, right after the existing `if sentinel.exists() { ... }` block, add the periodic rebuild — reuses the function's existing `periodic_secs` cadence value (the same one gating the periodic SCIP-refresh block further down), tracked with its own `Instant` since it must fire independently of whether other changes occurred (an edit to `.gitignore` itself doesn't increment `changes_since_periodic`):

```rust
if periodic_secs > 0 && last_ignore_rebuild.elapsed() >= Duration::from_secs(periodic_secs) {
    ignore_matcher = crate::ignore_rules::IgnoreMatcher::build(root);
    last_ignore_rebuild = std::time::Instant::now();
}
```

Finally, replace the event-time filter inside the `for path in event.paths { ... }` loop:

```rust
for path in event.paths {
    if ignore_matcher.is_ignored(&path, path.is_dir()) {
        continue;
    }
    // ... unchanged from here (rel = path.strip_prefix(root)... etc.)
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p infigraph-mcp test_code_watcher_ignores_excluded_dirs -- --nocapture --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Run the full watcher test suites**

Run: `cargo test -p infigraph-core --test watch_daemon --test daemon_protocol_watcher_wiring -- --test-threads=1`
Run: `cargo test -p infigraph-mcp --test watcher_reindex --test watcher_daemon_mode --test groups_watch_perf --test startup_watch -- --test-threads=1`
Expected: PASS. If any test fails to compile due to calling the old `register_subdirs`/`should_ignore`/`register_watch_dirs(..., ignore_dirs)` signatures directly (rather than only through `watch_project_with_periodic`), update that call site to match the new signature — the fix is mechanical (drop the `ignore_dirs` argument).

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-core/src/watch/mod.rs crates/infigraph-mcp/tests/watcher_reindex.rs
git commit -m "fix: code watcher honors .gitignore/.infigraphignore for directory registration and event filtering

Directory registration (register_watch_dirs) now uses the shared
ignore_rules::walk_builder, so an ignored tree is never subscribed to via
notify in the first place -- this is what actually closes the gap where a
live edit under a gitignored-but-not-hardcoded directory (e.g.
scratchpad/) could be written into the main project's graph via the
watcher's incremental index_files() path, which never re-checks ignore
rules on the paths it's handed. Event-time filtering (should_ignore) is
replaced by IgnoreMatcher, rebuilt on the existing periodic_secs cadence
so a live .gitignore edit takes effect without a watcher restart."
```

---

### Task 5: Migrate `grep_search`

**Files:**
- Modify: `crates/infigraph-core/src/search/mod.rs` (`walk_and_search` ~L409-474, `IGNORE_DIRS` const ~L433-444 deleted)
- Test: `crates/infigraph-core/tests/search_hybrid.rs` (extend `test_grep_search_skips_ignored_dirs`, ~L298-307)

**Interfaces:**
- Consumes: `crate::ignore_rules::walk_builder` from Task 1.

- [ ] **Step 1: Write the failing test**

In `crates/infigraph-core/tests/search_hybrid.rs`, extend `test_grep_search_skips_ignored_dirs`:

```rust
#[test]
fn test_grep_search_skips_ignored_dirs() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("node_modules").join("dep.js"), "findme\n").unwrap();
    std::fs::write(dir.path().join("app.js"), "findme\n").unwrap();

    // Gitignored, non-hardcoded directory -- only a real .gitignore rule
    // can exclude it.
    std::fs::write(dir.path().join(".gitignore"), "scratchpad/\n").unwrap();
    std::fs::create_dir(dir.path().join("scratchpad")).unwrap();
    std::fs::write(dir.path().join("scratchpad").join("copy.js"), "findme\n").unwrap();

    let results = search::grep_search(dir.path(), "findme", None, 100).unwrap();
    assert_eq!(results.len(), 1, "should skip node_modules and gitignored scratchpad/");
    assert!(results[0].file.contains("app.js"));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p infigraph-core test_grep_search_skips_ignored_dirs -- --nocapture`
Expected: FAIL — `results.len()` is `2` (app.js + scratchpad/copy.js).

- [ ] **Step 3: Replace `walk_and_search`'s manual recursion**

Find `walk_and_search` (search for `fn walk_and_search`) and replace its directory-walking with the shared walker, keeping the file-matching logic (glob filter, binary skip, line matching, limit) identical:

```rust
fn walk_and_search(
    base: &Path,
    re: &Regex,
    glob_pat: &Option<glob::Pattern>,
    limit: usize,
    matches: &mut Vec<GrepMatch>,
) -> Result<()> {
    for result in crate::ignore_rules::walk_builder(base).build() {
        if matches.len() >= limit {
            return Ok(());
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if let Some(ref gp) = glob_pat {
            if !gp.matches(&rel) {
                continue;
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            if matches.len() >= limit {
                return Ok(());
            }
            if re.is_match(line) {
                matches.push(GrepMatch {
                    file: rel.clone(),
                    line_number: idx + 1,
                    line_text: line.to_string(),
                });
            }
        }
    }
    Ok(())
}
```

Note the signature drops the `dir: &Path` parameter (the old function recursed manually with `dir` tracking the current recursion depth; `ignore::Walk` handles that internally now, so only `base` remains). Update `grep_search`'s call site accordingly:

```rust
pub fn grep_search(
    root: &Path,
    pattern: &str,
    file_pattern: Option<&str>,
    limit: usize,
) -> Result<Vec<GrepMatch>> {
    let re =
        Regex::new(pattern).map_err(|e| anyhow::anyhow!("invalid regex '{}': {}", pattern, e))?;

    let glob_pat = file_pattern
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid file pattern: {}", e))?;

    let mut matches = Vec::new();
    walk_and_search(root, &re, &glob_pat, limit, &mut matches)?;
    Ok(matches)
}
```

Delete the `IGNORE_DIRS` const (currently ~L433-444) — it has no remaining callers after this.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p infigraph-core test_grep_search_skips_ignored_dirs -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full search test suite**

Run: `cargo test -p infigraph-core --test search_hybrid`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-core/src/search/mod.rs crates/infigraph-core/tests/search_hybrid.rs
git commit -m "fix: grep_search honors .gitignore/.infigraphignore via shared ignore_rules"
```

---

### Task 6: Migrate security scanning (`walk_and_scan`)

**Files:**
- Modify: `crates/infigraph-core/src/security/detect.rs` (`walk_and_scan` ~L43-66, `IGNORE_DIRS` const ~L25-41 deleted)
- Test: add a new test in `crates/infigraph-core/src/security/detect.rs`'s own `#[cfg(test)]` module (or `crates/infigraph-core/tests/` if detection has integration-level tests already — check via `mcp__infigraph__search` for existing `scan_project` tests and extend the closest one if found, otherwise add the test below)

**Interfaces:**
- Consumes: `crate::ignore_rules::walk_builder` from Task 1.

- [ ] **Step 1: Write the failing test**

Add to `crates/infigraph-core/src/security/detect.rs` (inside its `#[cfg(test)] mod tests` block if one exists at the bottom of the file; otherwise add one):

```rust
#[test]
fn scan_project_skips_gitignored_non_hardcoded_dirs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.py"),
        "import os\nos.system(user_input)\n",
    )
    .unwrap();

    // Gitignored, non-hardcoded directory -- only a real .gitignore rule
    // can exclude it.
    std::fs::write(dir.path().join(".gitignore"), "scratchpad/\n").unwrap();
    std::fs::create_dir_all(dir.path().join("scratchpad")).unwrap();
    std::fs::write(
        dir.path().join("scratchpad/copy.py"),
        "import os\nos.system(user_input)\n",
    )
    .unwrap();

    let stats = scan_project(dir.path()).unwrap();
    let flagged_files: std::collections::HashSet<&str> =
        stats.findings.iter().map(|f| f.file.as_str()).collect();
    assert!(flagged_files.contains("app.py"));
    assert!(
        !flagged_files.iter().any(|f| f.contains("scratchpad")),
        "gitignored scratchpad/ should not be scanned: {flagged_files:?}"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p infigraph-core scan_project_skips_gitignored_non_hardcoded_dirs -- --nocapture`
Expected: FAIL — `flagged_files` contains an entry under `scratchpad/`.

- [ ] **Step 3: Replace `walk_and_scan`'s manual recursion**

Find `walk_and_scan` (search for `fn walk_and_scan`) and replace it:

```rust
fn walk_and_scan(root: &Path, stats: &mut ScanStats) -> Result<()> {
    for result in crate::ignore_rules::walk_builder(root).build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            scan_file(path, &rel, ext, stats)?;
        }
    }
    Ok(())
}
```

Update `scan_project`'s call site (it currently passes `root, root` since the old function took a separate `dir` recursion parameter):

```rust
pub fn scan_project(root: &Path) -> Result<ScanStats> {
    let mut stats = ScanStats::default();

    walk_and_scan(root, &mut stats)?;
    stats.findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    Ok(stats)
}
```

Delete the `IGNORE_DIRS` const (currently ~L25-41) — it has no remaining callers after this.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p infigraph-core scan_project_skips_gitignored_non_hardcoded_dirs -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full security test suite**

Run: `cargo test -p infigraph-core security::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-core/src/security/detect.rs
git commit -m "fix: security scanning honors .gitignore/.infigraphignore via shared ignore_rules"
```

---

### Task 7: Correct the stale documentation

**Files:**
- Modify: `docs/CODE-PARSING.md` (`## File Discovery` → `### Ignored directories`, currently L152-165)
- Modify: `docs/DOCUMENT-INDEXING.md` (`## File Discovery` → `### Ignored directories`, currently L101-108)

**Interfaces:** None — documentation only, no code deliverable, but bundled here rather than into an earlier task since it describes the *end state* of all 5 call sites, not any single one.

- [ ] **Step 1: Correct `docs/CODE-PARSING.md`**

Replace lines 152-165 (the `## File Discovery` section through the end of `### Ignored directories`):

```markdown
## File Discovery

File discovery walks the project directory via a shared component
(`infigraph_core::ignore_rules`, `crates/infigraph-core/src/ignore_rules.rs`),
applying:

### Ignored directories

A fixed safety list is always excluded regardless of any ignore file:
`.infigraph`, `.git`, `node_modules`, `__pycache__`, `.venv`, `venv`,
`target`, `build`, `dist`, `.tox`, `vendor`, `.idea`, `.mypy_cache`,
`coverage`, `.pytest_cache`.

Beyond that, real `.gitignore` rules are honored (via the `ignore` crate),
plus a custom `.infigraphignore` file recognized with the same syntax and
directory-level semantics as `.gitignore` — so a project-specific
convention (e.g. an agent worktree scratch directory) is excluded as long
as it's listed in either file, without needing a code change.
```

- [ ] **Step 2: Correct `docs/DOCUMENT-INDEXING.md`**

Replace lines 101-108 (the `## File Discovery` section through the end of `### Ignored directories`):

```markdown
## File Discovery

`DocIndex::collect_doc_files()` (`lib.rs`) walks the project directory via
the same shared `infigraph_core::ignore_rules` component code discovery
uses (`crates/infigraph-core/src/ignore_rules.rs`).

### Ignored directories

A fixed safety list is always excluded regardless of any ignore file:
`.infigraph`, `.git`, `node_modules`, `__pycache__`, `.venv`, `venv`,
`target`, `build`, `dist`, `.tox`, `vendor`, `.idea`, `.mypy_cache`,
`coverage`, `.pytest_cache`.

Beyond that, real `.gitignore` rules are honored (via the `ignore` crate),
plus a custom `.infigraphignore` file recognized with the same syntax and
directory-level semantics as `.gitignore`.
```

- [ ] **Step 3: Commit**

```bash
git add docs/CODE-PARSING.md docs/DOCUMENT-INDEXING.md
git commit -m "docs: correct File Discovery sections to describe the shared ignore_rules component

These sections described a hardcoded-list-only implementation that
Infigraph::collect_files had already moved past (it's used ignore::WalkBuilder
with real .gitignore/.infigraphignore support for some time); the docs were
never updated. Now accurate for all 5 call sites after this plan's tasks."
```

---

## Final verification

After all 7 tasks:

- [ ] Run `cargo fmt --all -- --check`
- [ ] Run `cargo clippy --all-targets -- -D warnings`
- [ ] Run `cargo test --all` (or per this repo's disk-constrained-build-workflow convention, batch per-crate: `cargo test -p infigraph-core && cargo test -p infigraph-docs && cargo test -p infigraph-mcp && cargo test -p infigraph-cli`)
- [ ] Manually verify no remaining references to the deleted `should_ignore`, `register_subdirs`, or either `IGNORE_DIRS` const: search for each name with `mcp__infigraph__search` and confirm zero results outside this plan's own commits' history.
