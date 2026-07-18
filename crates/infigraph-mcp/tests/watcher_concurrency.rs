use std::sync::Mutex;

use serde_json::json;

use infigraph_mcp::tools::docs::get_doc_watchers;
use infigraph_mcp::tools::graph::*;
use infigraph_mcp::tools::index::tool_index_project;
use infigraph_mcp::tools::search::tool_search;
use infigraph_mcp::tools::watch::*;

// All tests mutate the process-global WATCHERS map, so they must run sequentially.
static WATCHER_LOCK: Mutex<()> = Mutex::new(());

struct WatcherCleanup;

impl Drop for WatcherCleanup {
    fn drop(&mut self) {
        stop_all_watchers();
    }
}

fn make_project(files: &[(&str, &str)]) -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    for (name, content) in files {
        let p = dir.path().join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
    let path = dir.path().to_string_lossy().to_string();
    (dir, path)
}

fn stop_all_watchers() {
    let mut guard = get_watchers();
    let stopped_paths: Vec<String> = if let Some(map) = guard.as_mut() {
        let ids: Vec<String> = map.keys().cloned().collect();
        let mut paths = Vec::new();
        for id in ids {
            if let Some(entry) = map.remove(&id) {
                paths.push(entry.path.clone());
                let _ = entry.stop_tx.send(());
            }
        }
        paths
    } else {
        Vec::new()
    };
    drop(guard);
    let mut doc_guard = get_doc_watchers();
    if let Some(map) = doc_guard.as_mut() {
        let ids: Vec<String> = map.keys().cloned().collect();
        for id in ids {
            if let Some(entry) = map.remove(&id) {
                let _ = entry.stop_tx.send(());
            }
        }
    }
    drop(doc_guard);
    wait_for_watch_locks_released(&stopped_paths);
}

/// A stopped watcher's thread notices `stop_rx` on its own poll cadence and
/// may still be mid-reindex, so it doesn't release `.infigraph/watch.lock`
/// the instant the stop signal is sent. Block (with a generous bound) until
/// each path's lock is confirmed free, so tests that immediately re-watch
/// the same project aren't racing the previous watcher's shutdown.
fn wait_for_watch_locks_released(paths: &[String]) {
    use fs2::FileExt;
    for path in paths {
        let lock_path = std::path::Path::new(path)
            .join(".infigraph")
            .join("watch.lock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let file = match std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
            {
                Ok(f) => f,
                Err(_) => break,
            };
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = file.unlock();
                    break;
                }
                Err(_) => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    }
}

fn extract_watcher_id(output: &str) -> String {
    output
        .lines()
        .find(|l| l.starts_with("ID: "))
        .map(|l| l.trim_start_matches("ID: ").trim().to_string())
        .expect("should find watcher ID in output")
}

/// Graph tools must work while auto-watchers are running on the same projects.
/// This is the core scenario that broke on Windows due to Kuzu mandatory file locking.
#[test]
fn test_graph_tools_with_active_watchers() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    let (_dir_a, path_a) = make_project(&[
        (
            "src/main.py",
            "\
from src.lib import process

def main():
    result = process('hello')
    return result
",
        ),
        (
            "src/lib.py",
            "\
def process(data):
    return validate(data).upper()

def validate(data):
    if not data:
        raise ValueError('empty')
    return data.strip()
",
        ),
    ]);

    let (_dir_b, path_b) = make_project(&[(
        "app.py",
        "\
def serve():
    return handle_request()

def handle_request():
    return 'ok'
",
    )]);

    // Index both projects — auto_start_watch fires for each
    tool_index_project(&json!({"path": &path_a})).expect("index A");
    tool_index_project(&json!({"path": &path_b})).expect("index B");

    // Verify watchers are running
    let status = tool_get_watch_status(&json!({})).unwrap();
    assert!(
        status.contains("watcher") && !status.contains("No watchers"),
        "auto-watchers should be running: {status}"
    );

    // --- Graph tools on project A while its watcher is active ---
    let args_a = |extra: serde_json::Value| -> serde_json::Value {
        let mut map = extra.as_object().cloned().unwrap_or_default();
        map.insert("path".into(), json!(&path_a));
        serde_json::Value::Object(map)
    };

    let result = tool_get_stats(&args_a(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on A with watcher active: {result}"
    );

    let result = tool_get_symbols_in_file(&args_a(json!({"file": "src/lib.py"}))).unwrap();
    assert!(
        result.contains("process"),
        "symbols_in_file on A with watcher active: {result}"
    );

    let result =
        tool_get_code_snippet(&args_a(json!({"symbol_id": "src/lib.py::process"}))).unwrap();
    assert!(
        result.contains("process"),
        "code_snippet on A with watcher active: {result}"
    );

    let result = tool_search(&args_a(json!({"query": "validate"}))).unwrap();
    assert!(
        result.contains("validate"),
        "search on A with watcher active: {result}"
    );

    // --- Graph tools on project B while its watcher is active ---
    let args_b = |extra: serde_json::Value| -> serde_json::Value {
        let mut map = extra.as_object().cloned().unwrap_or_default();
        map.insert("path".into(), json!(&path_b));
        serde_json::Value::Object(map)
    };

    let result = tool_get_stats(&args_b(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on B with watcher active: {result}"
    );

    let result = tool_get_symbols_in_file(&args_b(json!({"file": "app.py"}))).unwrap();
    assert!(
        result.contains("serve"),
        "symbols_in_file on B with watcher active: {result}"
    );

    // --- Start an explicit auto_resolve watcher on A (in addition to the auto-watcher) ---
    // Stop the existing auto-watcher on A first, then start auto_resolve
    stop_all_watchers();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let result = tool_watch_project(&json!({
        "path": &path_a,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();
    assert!(
        result.contains("auto_resolve: ON"),
        "auto_resolve watcher should start: {result}"
    );
    let watcher_id_a = extract_watcher_id(&result);

    let result = tool_watch_project(&json!({
        "path": &path_b,
        "debounce_ms": 200
    }))
    .unwrap();
    assert!(
        result.contains("Watcher started"),
        "non-auto-resolve watcher should start: {result}"
    );
    let watcher_id_b = extract_watcher_id(&result);

    // Graph tools still work with explicit watchers running
    let result = tool_get_stats(&args_a(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on A with auto_resolve watcher: {result}"
    );

    let result = tool_query_graph(&args_a(json!({
        "cypher": "MATCH (s:Symbol) WHERE s.kind = 'Function' RETURN s.name LIMIT 5"
    })))
    .unwrap();
    assert!(
        !result.is_empty(),
        "query_graph on A with watcher: {result}"
    );

    let result = tool_get_stats(&args_b(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on B with watcher: {result}"
    );

    let result =
        tool_find_all_references(&args_a(json!({"symbol_id": "src/lib.py::validate"}))).unwrap();
    assert!(
        result.contains("src/lib.py"),
        "find_all_references on A with watcher: {result}"
    );

    // Clean up — stop all (code + doc) watchers
    tool_stop_watch(&json!({"watcher_id": watcher_id_a})).unwrap();
    tool_stop_watch(&json!({"watcher_id": watcher_id_b})).unwrap();
}

/// Group index starts auto-watchers for all repos in the group.
/// Graph tools on individual repos must work while group watchers are running.
#[test]
fn test_graph_tools_with_group_watchers() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    let home_dir = tempfile::TempDir::new().expect("tmpdir for home");
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home_dir.path());

    let (_dir_a, path_a) = make_project(&[(
        "api.py",
        "\
from flask import Flask
app = Flask(__name__)

@app.route('/orders')
def get_orders():
    return []

def internal_helper():
    return 42
",
    )]);

    let (_dir_b, path_b) = make_project(&[(
        "api.py",
        "\
from flask import Flask
app = Flask(__name__)

@app.route('/users')
def get_users():
    return []
",
    )]);

    // Index both so they're registered
    tool_index_project(&json!({"path": &path_a})).expect("index A");
    tool_index_project(&json!({"path": &path_b})).expect("index B");

    // Stop auto-watchers from index before creating the group
    stop_all_watchers();
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Create group and add repos
    use infigraph_mcp::tools::groups::*;
    tool_group_create(&json!({"name": "watcher-test-group"})).unwrap();
    tool_group_add(&json!({
        "group_name": "watcher-test-group",
        "repo_name": "order-svc",
        "path": &path_a
    }))
    .unwrap();
    tool_group_add(&json!({
        "group_name": "watcher-test-group",
        "repo_name": "user-svc",
        "path": &path_b
    }))
    .unwrap();

    // group_index starts auto-watchers for all repos
    let result = tool_group_index(&json!({"group_name": "watcher-test-group"})).unwrap();
    assert!(result.contains("Indexed"), "group_index: {result}");

    // Verify watchers are running
    let status = tool_get_watch_status(&json!({})).unwrap();
    assert!(
        !status.contains("No watchers"),
        "group_index should have started watchers: {status}"
    );

    // Graph tools on repo A while group watchers are active
    let args_a = |extra: serde_json::Value| -> serde_json::Value {
        let mut map = extra.as_object().cloned().unwrap_or_default();
        map.insert("path".into(), json!(&path_a));
        serde_json::Value::Object(map)
    };

    let result = tool_get_stats(&args_a(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on A with group watcher: {result}"
    );

    let result = tool_get_symbols_in_file(&args_a(json!({"file": "api.py"}))).unwrap();
    assert!(
        result.contains("get_orders"),
        "symbols_in_file on A with group watcher: {result}"
    );

    let result = tool_search(&args_a(json!({"query": "internal_helper"}))).unwrap();
    assert!(
        result.contains("internal_helper"),
        "search on A with group watcher: {result}"
    );

    // Graph tools on repo B while group watchers are active
    let args_b = |extra: serde_json::Value| -> serde_json::Value {
        let mut map = extra.as_object().cloned().unwrap_or_default();
        map.insert("path".into(), json!(&path_b));
        serde_json::Value::Object(map)
    };

    let result = tool_get_stats(&args_b(json!({}))).unwrap();
    assert!(
        result.contains("Symbol"),
        "get_stats on B with group watcher: {result}"
    );

    // Group query also works (opens DB connections to all repos in the group)
    let result = tool_group_query(&json!({
        "group_name": "watcher-test-group",
        "cypher": "MATCH (s:Symbol) WHERE s.kind = 'Function' RETURN s.name LIMIT 3"
    }))
    .unwrap();
    assert!(
        !result.is_empty(),
        "group_query with watchers active: {result}"
    );

    std::env::set_var("HOME", &orig_home);
}

/// MCP auto_start_watch should skip if CLI watcher holds the lock.
#[test]
fn test_mcp_skips_when_cli_lock_held() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("main.py", "def hello(): pass")]);
    tool_index_project(&json!({"path": &path})).unwrap();
    stop_all_watchers();

    // Simulate CLI watcher holding the lock
    let lock_path = std::path::PathBuf::from(&path)
        .join(".infigraph")
        .join("watch.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).ok();
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    lock_file.lock_exclusive().unwrap();

    // MCP auto_start_watch should return None (skip)
    let result = auto_start_watch(&path);
    assert!(result.is_none(), "should skip when CLI lock held");

    lock_file.unlock().unwrap();
}

/// MCP auto_start_watch should succeed when no CLI lock held.
#[test]
fn test_mcp_starts_when_no_cli_lock() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("main.py", "def greet(): pass")]);
    tool_index_project(&json!({"path": &path})).unwrap();

    // tool_index_project auto-starts a watcher — stop it first
    stop_all_watchers();

    // Now manually start — should succeed with no CLI lock
    let result = auto_start_watch(&path);
    assert!(
        result.is_some(),
        "should start when no CLI lock, got None for path: {path}"
    );
}

/// Regression test: two `tool_watch_project` calls for the same path — as
/// two different MCP worker processes would each make independently, with
/// no shared in-process state — must not both succeed. Before this fix,
/// `tool_watch_project` never acquired `.infigraph/watch.lock` itself (only
/// `auto_start_watch`'s `cli_watcher_holds_lock` pre-check read it, and only
/// to detect a CLI-held lock), so nothing gated a second MCP-started watcher
/// on the same project — every worker built its own duplicate watcher set.
/// Calling `tool_watch_project` directly here (bypassing `auto_start_watch`'s
/// own in-process `is_watching` map, which would otherwise mask this) proves
/// the dedup now comes from the lock itself, not from in-process state that
/// a second worker process wouldn't share.
#[test]
fn test_second_watch_project_call_declines_when_already_watching() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("main.py", "def hello(): pass")]);

    let first = tool_watch_project(&json!({"path": &path}));
    assert!(first.is_ok(), "first watcher should start: {first:?}");

    let second = tool_watch_project(&json!({"path": &path}));
    assert!(
        second.is_err(),
        "a second watch_project call for an already-watched path must decline, not start a duplicate watcher"
    );
    let err = second.unwrap_err().to_string();
    assert!(
        err.contains("already running"),
        "expected an 'already running' decline message, got: {err}"
    );
}

/// Search auto-starts a watcher when none is running (no stale warning).
#[test]
fn test_search_auto_starts_watcher_when_none_running() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("lib.py", "def compute(): return 42")]);
    tool_index_project(&json!({"path": &path})).unwrap();

    // tool_index_project auto-starts watcher — stop it
    stop_all_watchers();

    // Search should auto-start a watcher rather than warn about stale results
    let result = tool_search(&json!({"path": &path, "query": "compute"})).unwrap();
    assert!(
        result.contains("Auto-started watcher") || !result.contains("No file watcher running"),
        "search should auto-start watcher or not warn, got: {result}"
    );
}

/// Search output should NOT contain stale warning when MCP watcher running.
#[test]
fn test_no_stale_warning_with_mcp_watcher() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("app.py", "def serve(): pass")]);
    tool_index_project(&json!({"path": &path})).unwrap();
    // tool_index_project already started a watcher — it should be active

    let result = tool_search(&json!({"path": &path, "query": "serve"})).unwrap();
    assert!(
        !result.contains("No file watcher running"),
        "should not warn when watcher is active, got: {result}"
    );
}

/// Search output should NOT contain stale warning when CLI lock held.
#[test]
fn test_no_stale_warning_with_cli_watcher() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();
    let (_dir, path) = make_project(&[("util.py", "def parse(): pass")]);
    tool_index_project(&json!({"path": &path})).unwrap();
    stop_all_watchers();

    // Simulate CLI watcher holding lock
    let lock_path = std::path::PathBuf::from(&path)
        .join(".infigraph")
        .join("watch.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    lock_file.lock_exclusive().unwrap();

    let result = tool_search(&json!({"path": &path, "query": "parse"})).unwrap();
    assert!(
        !result.contains("No file watcher running"),
        "should not warn when CLI watcher holds lock"
    );

    lock_file.unlock().unwrap();
}
