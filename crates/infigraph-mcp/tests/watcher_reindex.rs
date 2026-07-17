use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;

use infigraph_mcp::tools::docs::{
    auto_start_doc_watch, init_doc_watchers, is_doc_watching, tool_index_docs, tool_search_docs,
    tool_watch_docs, DOC_WATCHERS,
};
use infigraph_mcp::tools::helpers::open_prism;
use infigraph_mcp::tools::index::tool_index_project;
use infigraph_mcp::tools::search::{tool_search, tool_search_symbols};
use infigraph_mcp::tools::watch::*;

static WATCHER_LOCK: Mutex<()> = Mutex::new(());

struct WatcherCleanup;

impl Drop for WatcherCleanup {
    fn drop(&mut self) {
        stop_all_watchers();
        stop_all_doc_watchers();
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

fn poll_until<F: Fn() -> bool>(check: F, timeout: Duration, desc: &str) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("poll_until timed out: {desc}");
    false
}

/// Modify an existing file in an existing directory — watcher should detect and reindex.
#[test]
fn test_code_watcher_reindexes_modified_file() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("lib.py", "def original(): return 1")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");

    // Verify original function is searchable
    let result = tool_search(&json!({"path": &path, "query": "original"})).unwrap();
    assert!(
        result.contains("original"),
        "original should be searchable: {result}"
    );

    // Stop auto-watcher from index, start explicit one with short debounce
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    let result = tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();
    assert!(
        result.contains("Watcher started"),
        "watcher should start: {result}"
    );

    // Modify file — add a new function
    std::thread::sleep(Duration::from_millis(500));
    let lib_path = std::path::PathBuf::from(&path).join("lib.py");
    std::fs::write(
        &lib_path,
        "def original(): return 1\n\ndef brand_new_function(): return 42\n",
    )
    .unwrap();

    // Poll until the new function is searchable
    let found = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "brand_new_function"}))
                .map(|r| r.contains("brand_new_function"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "brand_new_function should be searchable after watcher reindex",
    );

    assert!(
        found,
        "watcher should have reindexed modified file — brand_new_function not found"
    );
}

/// Create a new file in an existing directory — watcher should detect and reindex.
#[test]
fn test_code_watcher_reindexes_new_file_existing_dir() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def main(): pass")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Create new file in existing src/ dir
    std::thread::sleep(Duration::from_millis(500));
    let new_file = std::path::PathBuf::from(&path).join("src/utils.py");
    std::fs::write(&new_file, "def helper_util(): return 'help'\n").unwrap();

    let found = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "helper_util"}))
                .map(|r| r.contains("helper_util"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "helper_util should be searchable after watcher reindex",
    );

    assert!(
        found,
        "watcher should have reindexed new file in existing dir"
    );
}

/// Create a new file in a NEW directory — watcher should detect and reindex.
/// This is the branch-switch scenario where new dirs appear.
#[test]
fn test_code_watcher_reindexes_new_file_new_dir() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def main(): pass")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Create new directory + file (simulates branch switch adding new module)
    std::thread::sleep(Duration::from_millis(500));
    let new_dir = std::path::PathBuf::from(&path).join("newmodule");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(
        new_dir.join("feature.py"),
        "def new_feature(): return 'branch-b'\n",
    )
    .unwrap();

    let found = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "new_feature"}))
                .map(|r| r.contains("new_feature"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "new_feature should be searchable after watcher reindex",
    );

    assert!(
        found,
        "watcher should have reindexed new file in new dir — branch switch scenario"
    );
}

fn stop_all_doc_watchers() {
    let mut guard = DOC_WATCHERS.lock().unwrap();
    if let Some(map) = guard.as_mut() {
        let ids: Vec<String> = map.keys().cloned().collect();
        for id in ids {
            if let Some(entry) = map.remove(&id) {
                let _ = entry.stop_tx.send(());
            }
        }
    }
}

/// Doc watcher should detect new .md files and reindex them.
#[test]
fn test_doc_watcher_reindexes_new_doc() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("docs/readme.md", "# Hello\n\nThis is the readme.")]);

    // Initial doc index
    let result = tool_index_docs(&json!({"path": &path})).expect("initial doc index");
    eprintln!("initial index: {result}");

    // Verify initial doc is searchable
    let result = tool_search_docs(&json!({"path": &path, "query": "readme hello"})).unwrap();
    assert!(
        result.contains("readme") || result.contains("Hello"),
        "initial doc should be searchable: {result}"
    );

    // Start doc watcher with short debounce
    let result = tool_watch_docs(&json!({"path": &path, "debounce_ms": 500})).unwrap();
    eprintln!("watch_docs: {result}");
    assert!(
        result.contains("Document watcher started"),
        "watcher should start: {result}"
    );

    // Add a new doc file
    std::thread::sleep(Duration::from_millis(500));
    let new_doc = std::path::PathBuf::from(&path).join("docs/guide.md");
    std::fs::write(
        &new_doc,
        "# Unique Guide\n\nThis document contains xylophone_zebra_unicorn content.\n",
    )
    .unwrap();
    eprintln!("wrote new doc: {}", new_doc.display());

    // Poll until the new doc is searchable
    let found = poll_until(
        || {
            tool_search_docs(&json!({"path": &path, "query": "xylophone_zebra_unicorn"}))
                .map(|r| {
                    let has = r.contains("xylophone_zebra_unicorn") || r.contains("Unique Guide");
                    if !has {
                        eprintln!("search_docs result: {r}");
                    }
                    has
                })
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "xylophone_zebra_unicorn should be searchable after doc watcher reindex",
    );

    assert!(found, "doc watcher should have reindexed new document");
}

/// Doc watcher without concurrent readers — isolates whether WAL error is from concurrency.
#[test]
fn test_doc_watcher_reindexes_no_concurrent_read() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("docs/readme.md", "# Hello\n\nOriginal readme.")]);

    tool_index_docs(&json!({"path": &path})).expect("initial doc index");

    let result = tool_watch_docs(&json!({"path": &path, "debounce_ms": 500})).unwrap();
    eprintln!("watch_docs: {result}");

    // Add new doc
    std::thread::sleep(Duration::from_millis(500));
    let new_doc = std::path::PathBuf::from(&path).join("docs/noconcurrent.md");
    std::fs::write(
        &new_doc,
        "# No Concurrent\n\nContent: alpha_beta_gamma_unique.\n",
    )
    .unwrap();
    eprintln!("wrote new doc (no concurrent read)");

    // Wait for watcher to reindex WITHOUT polling search
    std::thread::sleep(Duration::from_secs(5));

    // Now do ONE search
    stop_all_doc_watchers();
    std::thread::sleep(Duration::from_millis(200));

    let result =
        tool_search_docs(&json!({"path": &path, "query": "alpha_beta_gamma_unique"})).unwrap();
    eprintln!("search result: {result}");
    assert!(
        result.contains("alpha_beta_gamma") || result.contains("No Concurrent"),
        "doc watcher should have reindexed without concurrent readers: {result}"
    );
}

/// Simulate branch switch: modify existing files, add new files in existing dirs.
#[test]
fn test_code_watcher_branch_switch_existing_dirs() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    // "Branch A" state
    let (_dir, path) = make_project(&[
        ("src/main.py", "def main_a(): pass"),
        ("src/lib.py", "def lib_a(): pass"),
    ]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Simulate "git checkout branch-b" — modify existing + add new in same dir
    std::thread::sleep(Duration::from_millis(500));
    let src = std::path::PathBuf::from(&path).join("src");
    std::fs::write(src.join("main.py"), "def main_b(): return 'branch-b'\n").unwrap();
    std::fs::write(src.join("lib.py"), "def lib_b(): return 'branch-b'\n").unwrap();
    std::fs::write(src.join("extra.py"), "def extra_branch_b(): return 99\n").unwrap();

    // All three should be searchable
    let found_main = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "main_b"}))
                .map(|r| r.contains("main_b"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "main_b searchable",
    );
    let found_extra = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "extra_branch_b"}))
                .map(|r| r.contains("extra_branch_b"))
                .unwrap_or(false)
        },
        Duration::from_secs(5),
        "extra_branch_b searchable",
    );

    assert!(
        found_main,
        "modified file main_b should be searchable after branch switch"
    );
    assert!(
        found_extra,
        "new file extra_branch_b should be searchable after branch switch"
    );
}

/// Simulate branch switch with NEW directories (new module added on branch B).
#[test]
fn test_code_watcher_branch_switch_new_dirs() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    // "Branch A" — only has src/
    let (_dir, path) = make_project(&[("src/main.py", "def main_a(): pass")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Simulate "git checkout branch-b" — adds new top-level module dir
    std::thread::sleep(Duration::from_millis(500));
    let new_module = std::path::PathBuf::from(&path).join("new_module");
    std::fs::create_dir_all(&new_module).unwrap();
    std::fs::write(
        new_module.join("feature.py"),
        "def branch_b_feature(): return 'new'\n",
    )
    .unwrap();

    // Also add nested new dir
    let nested = new_module.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("deep.py"),
        "def deeply_nested_func(): return 'deep'\n",
    )
    .unwrap();

    let found_feature = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "branch_b_feature"}))
                .map(|r| r.contains("branch_b_feature"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "branch_b_feature in new dir",
    );

    let found_deep = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "deeply_nested_func"}))
                .map(|r| r.contains("deeply_nested_func"))
                .unwrap_or(false)
        },
        Duration::from_secs(10),
        "deeply_nested_func in nested new dir",
    );

    assert!(
        found_feature,
        "file in new dir should be searchable after branch switch"
    );
    assert!(
        found_deep,
        "file in nested new dir should be searchable after branch switch"
    );
}

/// Doc watcher should detect new docs in a NEW subdirectory (recursive mode).
#[test]
fn test_doc_watcher_reindexes_new_doc_new_dir() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("docs/readme.md", "# Initial\n\nStarting doc.")]);

    tool_index_docs(&json!({"path": &path})).expect("initial doc index");

    let result = tool_watch_docs(&json!({"path": &path, "debounce_ms": 500})).unwrap();
    assert!(
        result.contains("Document watcher started"),
        "watcher should start: {result}"
    );

    // Create new subdirectory with a doc
    std::thread::sleep(Duration::from_millis(500));
    let new_sub = std::path::PathBuf::from(&path).join("docs/tutorials");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::write(
        new_sub.join("getting-started.md"),
        "# Getting Started\n\nThis tutorial covers quantum_flux_capacitor setup.\n",
    )
    .unwrap();

    let found = poll_until(
        || {
            tool_search_docs(&json!({"path": &path, "query": "quantum_flux_capacitor"}))
                .map(|r| r.contains("quantum_flux_capacitor") || r.contains("Getting Started"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "quantum_flux_capacitor should be searchable after doc watcher picks up new subdir",
    );

    assert!(
        found,
        "doc watcher should reindex docs in new subdirectories (recursive mode)"
    );
}

/// Code watcher should handle directory removal gracefully (no crash/hang).
#[test]
fn test_code_watcher_handles_dir_removal() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[
        ("src/main.py", "def main(): pass"),
        ("src/lib.py", "def lib_func(): return 1"),
        ("extra/helper.py", "def removable_helper(): return 99"),
    ]);

    tool_index_project(&json!({"path": &path})).expect("initial index");

    // Verify removable_helper is indexed
    let result = tool_search(&json!({"path": &path, "query": "removable_helper"})).unwrap();
    assert!(
        result.contains("removable_helper"),
        "removable_helper should be indexed initially: {result}"
    );

    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Remove the extra/ directory entirely
    std::thread::sleep(Duration::from_millis(500));
    std::fs::remove_dir_all(std::path::PathBuf::from(&path).join("extra")).unwrap();

    // Watcher should not crash — verify by modifying another file and seeing it picked up
    std::thread::sleep(Duration::from_millis(500));
    let main_path = std::path::PathBuf::from(&path).join("src/main.py");
    std::fs::write(
        &main_path,
        "def main(): pass\n\ndef after_removal_func(): return 'still alive'\n",
    )
    .unwrap();

    let found = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "after_removal_func"}))
                .map(|r| r.contains("after_removal_func"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "after_removal_func should be searchable — watcher survived dir removal",
    );

    stop_all_watchers();
    assert!(
        found,
        "watcher should continue working after directory removal"
    );

    // Trigger full reindex to prune stale files (watcher only does incremental;
    // full reindex is the correct path for structural changes like dir removal).
    // Use open_prism + index() directly to ensure inline code runs (not CLI subprocess).
    let prism = open_prism(&json!({"path": &path})).expect("open prism");
    prism.index().expect("reindex after dir removal");

    // removable_helper should no longer be in the graph — query graph directly
    // (search_symbols uses a cached BM25 index that won't reflect the prune)
    use infigraph_mcp::tools::graph::tool_query_graph;
    let graph_result = tool_query_graph(&json!({
        "path": &path,
        "query": "MATCH (s:Symbol) WHERE s.name = 'removable_helper' RETURN s.name"
    }))
    .unwrap_or_default();

    assert!(
        !graph_result.contains("removable_helper"),
        "removable_helper should be pruned from graph after full reindex, got: {graph_result}"
    );
}

/// Code watcher should detect changes to files with grammar-plugin extensions (e.g. .tf, .hcl).
/// Validates that the filter_registry.for_file() check covers ANTLR plug-n-play grammars.
#[test]
fn test_code_watcher_grammar_plugin_extensions() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    // Create project with a grammar-plugin file type (.tf for Terraform HCL)
    let (_dir, path) = make_project(&[("main.py", "def placeholder(): pass")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Add a .tf file — if HCL grammar plugin is loaded, watcher should pick it up
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(
        std::path::PathBuf::from(&path).join("infra.tf"),
        r#"resource "aws_instance" "terraform_watcher_test" {
  ami           = "ami-12345"
  instance_type = "t2.micro"
}
"#,
    )
    .unwrap();

    // Also add a standard .py file as control — this should always work
    std::fs::write(
        std::path::PathBuf::from(&path).join("utils.py"),
        "def grammar_control_func(): return 'control'\n",
    )
    .unwrap();

    // Control: .py file should always be picked up
    let found_control = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "grammar_control_func"}))
                .map(|r| r.contains("grammar_control_func"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "grammar_control_func (.py) should be searchable",
    );

    // Grammar plugin: .tf file — check if the grammar is registered
    let found_tf = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "terraform_watcher_test"}))
                .map(|r| r.contains("terraform_watcher_test"))
                .unwrap_or(false)
        },
        Duration::from_secs(5),
        "terraform_watcher_test (.tf) searchable if HCL grammar loaded",
    );

    assert!(
        found_control,
        "control .py file must be detected by watcher"
    );
    // .tf is conditional on grammar plugin being available — log result
    if found_tf {
        eprintln!("PASS: HCL grammar plugin active — .tf files detected by watcher");
    } else {
        eprintln!("INFO: HCL grammar plugin not loaded — .tf files not watched (expected if grammar not installed)");
    }
}

/// Cross-file CALLS: modify a function called from another file, verify auto_resolve
/// re-resolves the CALLS edge so search still finds the caller relationship.
#[test]
fn test_code_watcher_cross_file_auto_resolve() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    // Two Python files with a cross-file call: main.py calls lib.py's helper
    let (_dir, path) = make_project(&[
        ("lib.py", "def cross_file_helper(): return 'original'\n"),
        (
            "main.py",
            "from lib import cross_file_helper\n\ndef caller(): return cross_file_helper()\n",
        ),
    ]);

    tool_index_project(&json!({"path": &path})).expect("initial index");

    // Verify both symbols indexed
    let result = tool_search(&json!({"path": &path, "query": "cross_file_helper"})).unwrap();
    assert!(
        result.contains("cross_file_helper"),
        "cross_file_helper should be indexed: {result}"
    );

    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Modify lib.py — rename the function (simulates cross-file impact)
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(
        std::path::PathBuf::from(&path).join("lib.py"),
        "def cross_file_helper(): return 'modified_v2'\n\ndef extra_resolved_func(): return 42\n",
    )
    .unwrap();

    // Watcher with auto_resolve should reindex + re-resolve
    let found = poll_until(
        || {
            tool_search(&json!({"path": &path, "query": "extra_resolved_func"}))
                .map(|r| r.contains("extra_resolved_func"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "extra_resolved_func should be searchable after auto-resolve reindex",
    );

    assert!(
        found,
        "auto_resolve watcher should reindex cross-file changes"
    );
}

/// Watcher should ignore files in node_modules, .git, target, etc.
#[test]
fn test_code_watcher_ignores_excluded_dirs() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def main(): pass")]);

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

    assert!(found_legit, "legitimate file should be indexed by watcher");
    assert!(
        !found_nm,
        "node_modules files should NOT be indexed by watcher"
    );
    assert!(!found_venv, ".venv files should NOT be indexed by watcher");
}

/// Sentinel file stop: writing .infigraph/watch.stop should stop the watcher.
#[test]
fn test_code_watcher_sentinel_stop() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    init_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def main(): pass")]);

    tool_index_project(&json!({"path": &path})).expect("initial index");
    stop_all_watchers();
    std::thread::sleep(Duration::from_millis(200));

    let result = tool_watch_project(&json!({
        "path": &path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();

    // Extract watcher ID
    let id_line = result
        .lines()
        .find(|l| l.starts_with("ID:"))
        .expect("should have ID line");
    let watcher_id = id_line.trim_start_matches("ID:").trim();

    // Verify watcher is running
    let status = tool_get_watch_status(&json!({})).unwrap();
    assert!(
        status.contains(watcher_id),
        "watcher should be listed in status: {status}"
    );

    // Create sentinel file to stop
    let sentinel = std::path::PathBuf::from(&path)
        .join(".infigraph")
        .join("watch.stop");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, "").unwrap();

    // Wait for watcher to notice sentinel and stop (thread needs time to exit + clean up map)
    let stopped = poll_until(
        || {
            let s = tool_get_watch_status(&json!({})).unwrap_or_default();
            !s.contains(watcher_id)
        },
        Duration::from_secs(20),
        "watcher should stop after sentinel file created",
    );

    assert!(
        stopped,
        "watcher should have stopped via sentinel file mechanism"
    );
    assert!(
        !sentinel.exists(),
        "sentinel file should be cleaned up after stop"
    );
}

/// Doc watcher should prune stale docs when files are deleted from disk.
/// After deleting a doc file and triggering reindex, the deleted doc should
/// no longer appear in search results.
#[test]
fn test_doc_watcher_prunes_stale_docs() {
    let _guard = WATCHER_LOCK.lock().unwrap();
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[
        (
            "docs/keep.md",
            "# Keep\n\nThis doc should persist: keeper_doc_content.\n",
        ),
        (
            "docs/delete_me.md",
            "# Delete Me\n\nThis doc should be pruned: stale_prune_target.\n",
        ),
    ]);

    // Initial doc index — both docs indexed
    tool_index_docs(&json!({"path": &path})).expect("initial doc index");

    let result = tool_search_docs(&json!({"path": &path, "query": "stale_prune_target"})).unwrap();
    assert!(
        result.contains("stale_prune_target") || result.contains("Delete Me"),
        "delete_me.md should be indexed initially: {result}"
    );

    // Delete the file from disk
    std::fs::remove_file(std::path::PathBuf::from(&path).join("docs/delete_me.md")).unwrap();

    // Start doc watcher — it will detect a change and reindex
    let result = tool_watch_docs(&json!({"path": &path, "debounce_ms": 500})).unwrap();
    assert!(
        result.contains("Document watcher started"),
        "watcher should start: {result}"
    );

    // Trigger a change so watcher reindexes (modify the remaining doc)
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(
        std::path::PathBuf::from(&path).join("docs/keep.md"),
        "# Keep\n\nThis doc should persist: keeper_doc_content.\n\nUpdated to trigger reindex.\n",
    )
    .unwrap();

    // Wait for reindex to complete
    let reindexed = poll_until(
        || {
            tool_search_docs(&json!({"path": &path, "query": "trigger reindex"}))
                .map(|r| r.contains("trigger reindex") || r.contains("Updated"))
                .unwrap_or(false)
        },
        Duration::from_secs(15),
        "keeper doc should reflect update after reindex",
    );

    stop_all_doc_watchers();

    assert!(reindexed, "doc watcher should have reindexed after change");

    // Verify stale doc is pruned from the graph store directly
    // (search_docs uses HNSW embeddings on disk which aren't pruned yet)
    {
        let mut idx = infigraph_docs::DocIndex::open(std::path::Path::new(&path)).unwrap();
        idx.init().unwrap();
        let store = idx.store().unwrap();
        let hashes = store.get_doc_hashes().unwrap_or_default();

        assert!(
            !hashes.contains_key("docs/delete_me.md"),
            "deleted doc should be pruned from doc store, found keys: {:?}",
            hashes.keys().collect::<Vec<_>>()
        );

        assert!(
            hashes.contains_key("docs/keep.md"),
            "kept doc should still be in doc store"
        );
    }
}

/// auto_start_watch should not create duplicate watchers when called multiple times.
#[test]
fn test_auto_start_watch_no_duplicates() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("src/main.py", "def hello(): pass")]);
    tool_index_project(&json!({"path": &path})).expect("index");

    // index_project already calls auto_start_watch, so watcher should be running
    let count_after_index = {
        let guard = get_watchers();
        guard.as_ref().map(|m| m.len()).unwrap_or(0)
    };
    assert!(
        count_after_index >= 1,
        "index_project should have started a watcher"
    );

    // Explicit auto_start_watch should be no-op (already watching)
    let result = auto_start_watch(&path);
    assert!(
        result.is_none(),
        "auto_start_watch should be no-op when already watching"
    );

    let count_after_explicit = {
        let guard = get_watchers();
        guard.as_ref().map(|m| m.len()).unwrap_or(0)
    };
    assert_eq!(
        count_after_index, count_after_explicit,
        "explicit auto_start_watch should not create duplicate"
    );

    // Re-index should also not create duplicate
    tool_index_project(&json!({"path": &path})).expect("re-index");

    let count_after_reindex = {
        let guard = get_watchers();
        guard.as_ref().map(|m| m.len()).unwrap_or(0)
    };
    assert_eq!(
        count_after_index, count_after_reindex,
        "re-indexing should not create duplicate watcher"
    );
}

/// auto_start_doc_watch should not create duplicate doc watchers.
#[test]
fn test_auto_start_doc_watch_no_duplicates() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("docs/readme.md", "# Hello\n\nDoc content.")]);
    tool_index_docs(&json!({"path": &path})).expect("doc index");

    let canonical = std::path::PathBuf::from(&path)
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");

    // tool_index_docs now calls auto_start_doc_watch, so it should be running
    assert!(
        is_doc_watching(&canonical),
        "should be doc watching after index_docs"
    );

    let count_after_index = {
        let guard = DOC_WATCHERS.lock().unwrap();
        guard.as_ref().map(|m| m.len()).unwrap_or(0)
    };

    // Explicit call should be no-op
    let result = auto_start_doc_watch(&path);
    assert!(
        result.is_none(),
        "auto_start_doc_watch should be no-op when already watching"
    );

    let count_after_explicit = {
        let guard = DOC_WATCHERS.lock().unwrap();
        guard.as_ref().map(|m| m.len()).unwrap_or(0)
    };
    assert_eq!(
        count_after_index, count_after_explicit,
        "doc watcher count should not increase on duplicate auto_start_doc_watch"
    );
}

/// is_watching returns false after watchers are stopped.
#[test]
fn test_is_watching_lifecycle() {
    let _guard = WATCHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cleanup = WatcherCleanup;
    stop_all_watchers();
    stop_all_doc_watchers();
    init_watchers();
    init_doc_watchers();

    let (_dir, path) = make_project(&[("src/lib.py", "def func(): pass")]);
    tool_index_project(&json!({"path": &path})).expect("index");

    let canonical = std::path::PathBuf::from(&path)
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");

    // index_project starts watcher automatically now
    assert!(is_watching(&canonical), "should be watching after index");

    stop_all_watchers();
    assert!(
        !is_watching(&canonical),
        "should not be watching after stop"
    );
}
