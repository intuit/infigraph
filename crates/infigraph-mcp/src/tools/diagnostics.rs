use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::diagnostics::{diagnose, CheckStatus, DiagnosticCode, Severity};
use serde_json::{json, Value};

pub fn definition() -> Value {
    json!({
        "name": "diagnose",
        "description": "Read-only local diagnostics: graph safety/readability, registry revision, locks, optional search assets and MCP installation. No repair, network access, or watcher startup. Returns schema-versioned JSON; unknown states are explicit.",
        "inputSchema": {"type":"object", "properties":{"path":{"type":"string", "description":"Project root path"}}, "required":["path"]},
        "annotations": {"readOnlyHint":true, "destructiveHint":false, "openWorldHint":false}
    })
}

pub fn tool_diagnose(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .context("missing 'path'")?;
    let root = Path::new(path);
    let mut report = diagnose(root);
    // Consume the owning watcher registry; no second watcher/process manager.
    if let Ok(root) = root.canonicalize() {
        let root = root.to_string_lossy().replace('\\', "/");
        let (pending, unknown) = match super::watch::WATCHERS.lock() {
            Ok(guard) => {
                let mut pending = 0;
                let mut unknown = false;
                if let Some(map) = guard.as_ref() {
                    for entry in map.values().filter(|e| e.path == root) {
                        match entry.pending_reindex.lock() {
                            Ok(work) => pending += work.len(),
                            Err(_) => unknown = true,
                        }
                    }
                }
                (pending, unknown)
            }
            Err(_) => (0, true),
        };
        if unknown {
            report.push(
                DiagnosticCode::WatcherStatusUnknown,
                CheckStatus::Unknown,
                Severity::Warning,
                "This MCP worker's watcher registry or pending-work state could not be inspected.",
                Some("Review MCP worker logs and watcher state."),
            );
        }
        if pending > 0 {
            report.push(
                DiagnosticCode::WatcherPendingReindex,
                CheckStatus::Problem,
                Severity::Warning,
                format!("This MCP worker has {pending} pending cross-file reindex entries."),
                Some("Run `infigraph index` or call index_project."),
            );
        }
    }
    Ok(serde_json::to_string_pretty(&report)?)
}
