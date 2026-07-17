use infigraph_mcp::tools::watch::{
    auto_start_watch_opportunistic, disable_watchers, get_watchers, init_watchers,
};

fn make_project() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    std::fs::write(dir.path().join("main.py"), "def hello(): pass").unwrap();
    let path = dir.path().to_string_lossy().to_string();
    (dir, path)
}

/// Stops any watcher(s) started during the test, even on panic.
struct WatcherCleanup;

impl Drop for WatcherCleanup {
    fn drop(&mut self) {
        let mut guard = get_watchers();
        if let Some(map) = guard.as_mut() {
            for (_, entry) in map.drain() {
                let _ = entry.stop_tx.send(());
            }
        }
    }
}

/// Regression test for issue #9 defect 4b: `auto_start_watch_opportunistic`
/// bypasses the disabled-watchers guard. Own file since `disable_watchers()`
/// has no reset and would otherwise leak into other watcher tests.
#[test]
fn test_opportunistic_watch_respects_disabled_guard() {
    let _cleanup = WatcherCleanup;
    disable_watchers();
    init_watchers();

    let (_dir, path) = make_project();

    let result = auto_start_watch_opportunistic(&path);

    assert!(
        result.is_none(),
        "watcher started despite disable_watchers(): {result:?}"
    );
}
