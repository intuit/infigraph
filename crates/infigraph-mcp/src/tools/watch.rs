use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::watch::WatchEventKind;
use infigraph_languages::bundled_registry;

static WATCHERS_DISABLED: AtomicBool = AtomicBool::new(false);

pub fn disable_watchers() {
    WATCHERS_DISABLED.store(true, Ordering::Relaxed);
}

pub fn watchers_disabled() -> bool {
    WATCHERS_DISABLED.load(Ordering::Relaxed)
}

fn watch_log(level: &str, msg: &str) {
    use std::io::Write;
    let path = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".infigraph")
        .join("mcp.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {level}: {msg}");
    }
}

pub struct WatcherEntry {
    pub stop_tx: mpsc::Sender<()>,
    pub path: String,
    pub pending_reindex: Arc<Mutex<Vec<String>>>,
}

pub static WATCHERS: Mutex<Option<HashMap<String, WatcherEntry>>> = Mutex::new(None);

pub fn get_watchers() -> std::sync::MutexGuard<'static, Option<HashMap<String, WatcherEntry>>> {
    WATCHERS.lock().unwrap()
}

pub fn init_watchers() {
    let mut guard = WATCHERS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
}

pub fn is_watching(path: &str) -> bool {
    let guard = WATCHERS.lock().unwrap();
    guard
        .as_ref()
        .is_some_and(|map| map.values().any(|e| e.path == path))
}

pub fn auto_start_watch(path: &str) -> Option<String> {
    auto_start_watch_inner(path, false)
}

pub fn auto_start_watch_opportunistic(path: &str) -> Option<String> {
    auto_start_watch_inner(path, true)
}

fn auto_start_watch_inner(path: &str, skip_disabled_check: bool) -> Option<String> {
    if is_remote_mode() {
        return None;
    }
    if !skip_disabled_check && watchers_disabled() {
        return None;
    }
    let root = std::path::PathBuf::from(path).canonicalize().ok()?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    if is_watching(&root_str) {
        return None;
    }

    let args = serde_json::json!({
        "path": path,
        "auto_resolve": true,
        "debounce_ms": 500
    });
    match tool_watch_project(&args) {
        Ok(msg) => {
            eprintln!("[auto-watch] Started watcher for {root_str}");
            Some(msg)
        }
        Err(e) => {
            eprintln!("[auto-watch] Failed to start watcher: {e}");
            None
        }
    }
}

/// Acquire the per-project watch lock (`.infigraph/watch.lock`), the same
/// lock the CLI's `infigraph watch` holds for its lifetime. Retries briefly:
/// a watcher that was just told to stop may take up to its poll interval
/// (~200ms) to actually exit and release this lock, and without a retry
/// window a start attempt landing in that gap would spuriously fail.
fn acquire_project_watch_lock(lock_path: &std::path::Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;

    const ATTEMPTS: u32 = 10;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
    let mut last_err = None;
    for _ in 0..ATTEMPTS {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(RETRY_DELAY);
            }
        }
    }
    Err(last_err.unwrap().into())
}

fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

pub fn tool_watch_project(args: &Value) -> Result<String> {
    if is_remote_mode() {
        return Ok(
            "File watching is not supported in remote mode (Neo4j backend). \
                    Reindexing is triggered via webhooks instead."
                .to_string(),
        );
    }

    init_watchers();

    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let debounce_ms = args
        .get("debounce_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(500);
    let auto_resolve = args
        .get("auto_resolve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;
    let root_str = root.to_string_lossy().replace('\\', "/");

    let lock_path = root.join(".infigraph").join("watch.lock");
    let watch_lock = acquire_project_watch_lock(&lock_path).map_err(|_| {
        anyhow::anyhow!(
            "another watcher is already running for {root_str} \
             (CLI `infigraph watch` or another MCP worker) — \
             use get_watch_status to check, or stop it first"
        )
    })?;

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let watcher_id = format!(
        "watch-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let pending_reindex: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let pending_clone = Arc::clone(&pending_reindex);

    {
        let mut guard = get_watchers();
        if let Some(map) = guard.as_mut() {
            map.insert(
                watcher_id.clone(),
                WatcherEntry {
                    stop_tx,
                    path: root_str.clone(),
                    pending_reindex,
                },
            );
        }
    }

    let watcher_id_clone = watcher_id.clone();
    std::thread::spawn(move || {
        // Held for the watcher's lifetime; released when this thread exits.
        let _watch_lock = watch_lock;
        let id_short = watcher_id_clone[..12.min(watcher_id_clone.len())].to_string();
        if auto_resolve {
            if let Err(e) = infigraph_core::watch::watch_project_auto_resolve(
                &root,
                bundled_registry,
                debounce_ms,
                stop_rx,
                &id_short,
            ) {
                let msg = format!("watcher {id_short} auto_resolve error: {e}");
                eprintln!("[watch] {msg}");
                watch_log("ERROR", &msg);
            }
        } else {
            let on_event = {
                let id_short = id_short.clone();
                move |evt: infigraph_core::watch::WatchEvent| match evt.kind {
                    WatchEventKind::WatcherRestarted => {
                        let msg = format!("watcher {id_short} restarted after internal failure");
                        eprintln!("[watch {id_short}] {msg}");
                        watch_log("WARN", &msg);
                    }
                    WatchEventKind::WatcherDied => {
                        let msg = format!(
                            "watcher {id_short} died permanently for {}",
                            evt.path.display()
                        );
                        eprintln!("[watch {id_short}] {msg}");
                        watch_log("ERROR", &msg);
                    }
                    _ if evt.has_cross_file_calls => {
                        let file = evt.path.to_string_lossy().replace('\\', "/");
                        eprintln!("[watch {id_short}] {evt}");
                        let mut pending = pending_clone.lock().unwrap();
                        if !pending.contains(&file) {
                            pending.push(file);
                        }
                        eprintln!("[watch {id_short}] ⚠ cross-file calls affected — call index_project to re-resolve (or use auto_resolve=true)");
                    }
                    _ => {
                        eprintln!("[watch {id_short}] {evt}");
                    }
                }
            };
            if let Err(e) = infigraph_core::watch::watch_project(
                &root,
                bundled_registry,
                debounce_ms,
                stop_rx,
                on_event,
            ) {
                let msg = format!("watcher {id_short} error: {e}");
                eprintln!("[watch] {msg}");
                watch_log("ERROR", &msg);
            }
        }
        let mut guard = WATCHERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(&watcher_id_clone);
        }
    });

    let auto_note = if auto_resolve {
        "\nauto_resolve: ON — full reindex runs automatically when cross-file calls are affected"
    } else {
        "\nauto_resolve: OFF — call index_project when notified of cross-file call changes, or use get_watch_status to check"
    };

    Ok(format!(
        "Watcher started.\nID: {watcher_id}\nPath: {root_str}\nDebounce: {debounce_ms}ms{auto_note}\nUse stop_watch to stop."
    ))
}

pub fn tool_stop_watch(args: &Value) -> Result<String> {
    let watcher_id = args
        .get("watcher_id")
        .and_then(|v| v.as_str())
        .context("missing 'watcher_id'")?;

    let mut guard = get_watchers();
    if let Some(map) = guard.as_mut() {
        if let Some(entry) = map.remove(watcher_id) {
            let _ = entry.stop_tx.send(());
            return Ok(format!("Watcher {watcher_id} stopped."));
        }
    }
    Ok(format!("No watcher found with ID: {watcher_id}"))
}

pub fn tool_get_watch_status(args: &Value) -> Result<String> {
    let watcher_id = args.get("watcher_id").and_then(|v| v.as_str());

    if let Some(id) = watcher_id {
        // Check code watchers
        {
            let guard = get_watchers();
            if let Some(map) = guard.as_ref() {
                if let Some(entry) = map.get(id) {
                    let pending = entry.pending_reindex.lock().unwrap();
                    let mut out = format!("Watcher: {id}\nType: code\nPath: {}\n", entry.path);
                    if pending.is_empty() {
                        out.push_str("Status: OK — no pending reindex needed\n");
                    } else {
                        out.push_str(&format!(
                            "⚠ {} file(s) changed with cross-file calls — run index_project to re-resolve:\n",
                            pending.len()
                        ));
                        for f in pending.iter() {
                            out.push_str(&format!("  - {f}\n"));
                        }
                    }
                    return Ok(out);
                }
            }
        }
        // Check doc watchers
        {
            let guard = super::docs::get_doc_watchers();
            if let Some(map) = guard.as_ref() {
                if let Some(entry) = map.get(id) {
                    return Ok(format!(
                        "Watcher: {id}\nType: docs\nPath: {}\nStatus: active\n",
                        entry.path
                    ));
                }
            }
        }
        return Ok(format!("No watcher found with ID: {id}"));
    }

    // List all watchers from both registries
    let mut total = 0usize;
    let mut out = String::new();

    {
        let guard = get_watchers();
        if let Some(map) = guard.as_ref() {
            for (id, entry) in map.iter() {
                total += 1;
                let pending_count = entry.pending_reindex.lock().unwrap().len();
                let warn = if pending_count > 0 {
                    format!(" ⚠ {pending_count} pending reindex")
                } else {
                    String::new()
                };
                out.push_str(&format!("  {id} — [code] {}{warn}\n", entry.path));
            }
        }
    }

    {
        let guard = super::docs::get_doc_watchers();
        if let Some(map) = guard.as_ref() {
            for (id, entry) in map.iter() {
                total += 1;
                out.push_str(&format!("  {id} — [docs] {}\n", entry.path));
            }
        }
    }

    if total == 0 {
        return Ok("No watchers running.".to_string());
    }

    Ok(format!("{total} watcher(s) running:\n{out}"))
}
