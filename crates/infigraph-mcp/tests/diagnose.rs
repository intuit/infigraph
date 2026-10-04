use serde_json::{json, Value};

#[test]
fn diagnose_dispatch_and_protocol_preserve_json_without_creating_state() {
    let project = tempfile::tempdir().unwrap();
    let args = json!({"path":project.path()});
    let text = infigraph_mcp::dispatch_tool("diagnose", &args).unwrap();
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "unhealthy");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "PROJECT_NOT_INDEXED"
            && c["severity"] == "error"
            && c["recommended_action"].is_string()));
    let session_before = infigraph_mcp::session_context::get_compression_stats();
    let response = infigraph_mcp::handle_tools_call(
        &json!(1),
        &json!({"params":{"name":"diagnose", "arguments":args}}),
    );
    let protocol_report: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(report, protocol_report);
    assert_eq!(
        session_before,
        infigraph_mcp::session_context::get_compression_stats()
    );
    assert!(!project.path().join(".infigraph").exists());
    let definition = infigraph_mcp::build_tools_list()
        .into_iter()
        .find(|t| t["name"] == "diagnose")
        .unwrap();
    assert_eq!(definition["annotations"]["readOnlyHint"], true);
}

#[test]
fn diagnose_rejects_missing_path_without_panicking() {
    assert!(infigraph_mcp::dispatch_tool("diagnose", &json!({})).is_err());
    let response = infigraph_mcp::handle_tools_call(
        &json!(2),
        &json!({"params":{"name":"diagnose", "arguments":{"path":false}}}),
    );
    assert_eq!(response["result"]["isError"], true);
}

#[test]
fn diagnose_consumes_pending_work_from_existing_watcher_registry() {
    use infigraph_mcp::tools::watch::{WatcherEntry, WATCHERS};
    use std::sync::{mpsc, Arc, Mutex};
    let project = tempfile::tempdir().unwrap();
    let path = project
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let (stop_tx, _stop_rx) = mpsc::channel();
    {
        let mut watchers = WATCHERS.lock().unwrap();
        watchers.get_or_insert_with(Default::default).insert(
            "diagnostic-fixture".into(),
            WatcherEntry {
                path: path.clone(),
                stop_tx,
                pending_reindex: Arc::new(Mutex::new(vec!["app.py".into()])),
            },
        );
    }
    let report: Value = serde_json::from_str(
        &infigraph_mcp::dispatch_tool("diagnose", &json!({"path":path})).unwrap(),
    )
    .unwrap();
    WATCHERS
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .remove("diagnostic-fixture");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "WATCHER_PENDING_REINDEX"
            && c["severity"] == "warning"
            && c["recommended_action"].is_string()));
    assert!(!project.path().join(".infigraph").exists());
}

#[test]
fn diagnose_reports_unavailable_pending_state_without_panicking() {
    use infigraph_mcp::tools::watch::{WatcherEntry, WATCHERS};
    use std::sync::{mpsc, Arc, Mutex};
    let project = tempfile::tempdir().unwrap();
    let path = project
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let pending = Arc::new(Mutex::new(Vec::new()));
    let poison = Arc::clone(&pending);
    assert!(std::thread::spawn(move || {
        let _guard = poison.lock().unwrap();
        panic!("fixture pending-state failure");
    })
    .join()
    .is_err());
    let (stop_tx, _stop_rx) = mpsc::channel();
    WATCHERS
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(
            "poisoned-diagnostic-fixture".into(),
            WatcherEntry {
                path: path.clone(),
                stop_tx,
                pending_reindex: pending,
            },
        );
    let report: Value = serde_json::from_str(
        &infigraph_mcp::dispatch_tool("diagnose", &json!({"path":path})).unwrap(),
    )
    .unwrap();
    WATCHERS
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .remove("poisoned-diagnostic-fixture");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "WATCHER_STATUS_UNKNOWN"
            && c["status"] == "unknown"
            && c["severity"] == "warning"));
    assert!(!project.path().join(".infigraph").exists());
}
