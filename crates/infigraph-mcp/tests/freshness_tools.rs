use std::sync::Mutex;

use serde_json::json;

use infigraph_mcp::tools::analysis::{
    tool_trace_callees, tool_trace_callers, tool_transitive_impact,
};
use infigraph_mcp::tools::graph::{tool_find_all_references, tool_get_symbols_in_file};
use infigraph_mcp::tools::index::tool_index_project;
use infigraph_mcp::tools::watch::get_watchers;

static WATCHER_LOCK: Mutex<()> = Mutex::new(());

/// These tests assert on a graph frozen at a specific commit, but
/// `tool_index_project` auto-starts a watcher with `auto_resolve=true` — if
/// left running it will notice the file mutations below and reindex before
/// the assertions run, racing "stale" back to "fresh". Stop it immediately
/// after the initial index so the fixture stays frozen for the rest of the test.
fn stop_all_watchers() {
    let mut guard = get_watchers();
    if let Some(map) = guard.as_mut() {
        let ids: Vec<String> = map.keys().cloned().collect();
        for id in ids {
            if let Some(entry) = map.remove(&id) {
                let _ = entry.stop_tx.send(());
            }
        }
    }
    drop(guard);
    std::thread::sleep(std::time::Duration::from_millis(300));
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git command failed to run");
    assert!(status.success(), "git {:?} failed", args);
}

/// Sets up a git-backed fixture, indexes it, and returns (tempdir, path,
/// symbol_id of `helper`) for use across the freshness assertions below.
fn make_indexed_git_project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    std::fs::write(
        dir.path().join("lib.py"),
        "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
    )
    .unwrap();
    // Infigraph writes its own .claude/CLAUDE.md as a side effect of indexing
    // (crates/infigraph-core/src/claude_md.rs) — a real project ignores it,
    // same as this repo's own .gitignore does; without this the working tree
    // would look "dirty" from Infigraph's own bookkeeping, not the user's edits.
    std::fs::write(dir.path().join(".gitignore"), ".claude/\n.infigraph/\n").unwrap();

    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let path = dir.path().to_string_lossy().to_string();
    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();

    let symbols = tool_get_symbols_in_file(&json!({"path": &path, "file": "lib.py"})).unwrap();
    let symbol_id = symbols
        .lines()
        .find(|l| l.contains("helper"))
        .and_then(|l| l.split("id=").nth(1))
        .map(|s| s.trim().to_string())
        .expect("helper symbol id should be present in get_symbols_in_file output");

    (dir, path, symbol_id)
}

/// A freshly indexed git repo with no changes since should report no
/// staleness warning on any of the graph-query tools named in the issue.
#[test]
fn fresh_graph_has_no_warning() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let (_dir, path, symbol_id) = make_indexed_git_project();
    let args = json!({"path": &path, "symbol_id": &symbol_id});

    for (name, out) in [
        ("trace_callers", tool_trace_callers(&args).unwrap()),
        ("trace_callees", tool_trace_callees(&args).unwrap()),
        ("transitive_impact", tool_transitive_impact(&args).unwrap()),
        (
            "find_all_references",
            tool_find_all_references(&args).unwrap(),
        ),
    ] {
        assert!(
            !out.contains("⚠"),
            "{name} should have no staleness warning on a freshly indexed repo: {out}"
        );
    }
}

/// After a commit that the graph was never reindexed against, every one of
/// the four tools named in the issue should prepend a stale warning with the
/// correct indexed_head/current_head SHAs — instead of silently answering
/// from outdated data.
#[test]
fn stale_after_commit_warns_on_all_named_tools() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let (dir, path, symbol_id) = make_indexed_git_project();
    let args = json!({"path": &path, "symbol_id": &symbol_id});

    // Commit a change without reindexing — simulates a branch switch/rebase
    // the graph hasn't caught up with yet.
    std::fs::write(
        dir.path().join("lib.py"),
        "def helper():\n    return 2\n\ndef caller():\n    return helper()\n\ndef extra(): pass\n",
    )
    .unwrap();
    git(dir.path(), &["commit", "-q", "-am", "second"]);

    for (name, out) in [
        ("trace_callers", tool_trace_callers(&args).unwrap()),
        ("trace_callees", tool_trace_callees(&args).unwrap()),
        ("transitive_impact", tool_transitive_impact(&args).unwrap()),
        (
            "find_all_references",
            tool_find_all_references(&args).unwrap(),
        ),
    ] {
        assert!(
            out.contains("⚠ stale"),
            "{name} should warn when indexed HEAD no longer matches current HEAD: {out}"
        );
        assert!(
            out.contains("indexed_head=") && out.contains("current_head="),
            "{name} warning should include both SHAs: {out}"
        );
        assert!(
            out.contains("branch/commit changed"),
            "{name} warning should explain why it's stale: {out}"
        );
    }
}

/// Uncommitted edits to a tracked file (no new commit) should also trigger
/// the warning, distinctly reasoned as "uncommitted changes" rather than a
/// HEAD mismatch.
#[test]
fn dirty_working_tree_warns_without_a_new_commit() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let (dir, path, symbol_id) = make_indexed_git_project();
    let args = json!({"path": &path, "symbol_id": &symbol_id});

    std::fs::write(
        dir.path().join("lib.py"),
        "def helper():\n    return 999  # uncommitted edit\n\ndef caller():\n    return helper()\n",
    )
    .unwrap();

    let out = tool_trace_callers(&args).unwrap();
    assert!(
        out.contains("⚠ stale"),
        "dirty working tree should warn: {out}"
    );
    assert!(
        out.contains("uncommitted changes"),
        "warning should call out uncommitted changes specifically: {out}"
    );
}
