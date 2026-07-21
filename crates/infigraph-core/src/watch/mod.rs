pub mod batch;

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::Result;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::Infigraph;
use batch::ChangeBatch;

/// A single file-change event emitted by the watcher.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub kind: WatchEventKind,
    pub path: PathBuf,
    /// True if this file has cross-file CALLS edges — full reindex needed to re-resolve them.
    pub has_cross_file_calls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEventKind {
    Modified,
    Created,
    Removed,
    WatcherRestarted,
    WatcherDied,
}

impl std::fmt::Display for WatchEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            WatchEventKind::Modified => "modified",
            WatchEventKind::Created => "created",
            WatchEventKind::Removed => "removed",
            WatchEventKind::WatcherRestarted => "watcher-restarted",
            WatchEventKind::WatcherDied => "watcher-died",
        };
        if self.has_cross_file_calls {
            write!(
                f,
                "{kind}: {} [cross-file calls detected — full reindex recommended]",
                self.path.display()
            )
        } else {
            write!(f, "{kind}: {}", self.path.display())
        }
    }
}

/// Watch a project directory and auto-reindex on file changes.
///
/// Opens a short-lived DB connection for each batch of changes rather than
/// holding one open continuously. This avoids Kuzu file-lock conflicts on
/// Windows where mandatory locking prevents concurrent DB connections.
///
/// Blocks until `stop_rx` receives a signal.
pub fn watch_project<MR>(
    root: &Path,
    make_registry: MR,
    debounce_ms: u64,
    stop_rx: mpsc::Receiver<()>,
    on_event: impl Fn(WatchEvent) + Send + 'static,
) -> Result<()>
where
    MR: Fn() -> Result<crate::lang::LanguageRegistry> + Send + 'static,
{
    watch_project_with_periodic(
        root,
        make_registry,
        debounce_ms,
        stop_rx,
        on_event,
        0,
        None::<fn(&crate::IndexResult)>,
    )
}

pub fn watch_project_with_periodic<MR, F>(
    root: &Path,
    make_registry: MR,
    debounce_ms: u64,
    stop_rx: mpsc::Receiver<()>,
    on_event: impl Fn(WatchEvent) + Send + 'static,
    periodic_secs: u64,
    on_periodic: Option<F>,
) -> Result<()>
where
    MR: Fn() -> Result<crate::lang::LanguageRegistry> + Send + 'static,
    F: Fn(&crate::IndexResult) + Send + 'static,
{
    // Some watch backends (e.g. FSEvents on macOS) deliver absolute,
    // symlink-resolved event paths regardless of how `root` was specified.
    // If `root` is relative, or traverses a symlink (macOS temp dirs live
    // under /var, itself a symlink to /private/var), `path.strip_prefix(root)`
    // below silently fails for every event and all changes are dropped.
    let root = &root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let ignore_dirs: &[&str] = &[
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
    ];

    // Build a registry once for file-extension filtering (no DB needed).
    let filter_registry = make_registry()?;

    let mut changes_since_periodic: usize = 0;
    let mut last_periodic = std::time::Instant::now();

    // Batch accumulator: collect file changes over a 1-second window
    // then index them all at once using the bulk write path.
    let mut batch = ChangeBatch::new(1000);

    let sentinel = root.join(".infigraph").join("watch.stop");

    const MAX_RESTARTS: u32 = 3;
    let mut restart_count: u32 = 0;

    // Create initial watcher — factored into a closure for restart.
    let create_watcher =
        |root: &Path,
         ignore_dirs: &[&str]|
         -> Result<(RecommendedWatcher, mpsc::Receiver<notify::Result<Event>>)> {
            let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
            let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));
            let mut watcher = RecommendedWatcher::new(tx, config)?;
            register_watch_dirs(&mut watcher, root, ignore_dirs)?;
            Ok((watcher, rx))
        };

    let (mut watcher, mut rx) = create_watcher(root, ignore_dirs)?;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if sentinel.exists() {
            let _ = std::fs::remove_file(&sentinel);
            break;
        }

        // Periodic SCIP refresh: if changes accumulated and enough time passed
        if periodic_secs > 0
            && changes_since_periodic > 0
            && last_periodic.elapsed() >= Duration::from_secs(periodic_secs)
        {
            if let Some(ref cb) = on_periodic {
                if let Ok(prism) = open_transient(root, &make_registry) {
                    match prism.index() {
                        Ok(result) => {
                            if !result.extractions.is_empty() {
                                cb(&result);
                            }
                        }
                        Err(e) => eprintln!("[watch] periodic reindex failed: {e}"),
                    }
                }
            }
            changes_since_periodic = 0;
            last_periodic = std::time::Instant::now();
        }

        // Flush the batch when the window has closed
        if !batch.is_empty() && batch.is_ready() {
            let paths = batch.drain();
            let count = paths.len();
            eprintln!("[watch] batch indexing {count} files");

            if let Ok(prism) = open_transient(root, &make_registry) {
                match prism.index_files(&paths) {
                    Ok(result) => {
                        changes_since_periodic += result.indexed_files;

                        if let Some(backend) = prism.backend() {
                            let changed: Vec<&str> =
                                result.extractions.iter().map(|e| e.file.as_str()).collect();
                            if !changed.is_empty() {
                                if let Err(e) =
                                    crate::embed::update_embeddings(backend, root, &changed)
                                {
                                    eprintln!("[watch] batch embedding update failed: {e}");
                                }
                            }
                        }

                        for extraction in &result.extractions {
                            let cross = has_cross_file_calls(&prism, &extraction.file);
                            let abs_path = root.join(&extraction.file);
                            on_event(WatchEvent {
                                kind: WatchEventKind::Modified,
                                path: abs_path,
                                has_cross_file_calls: cross,
                            });
                        }
                    }
                    Err(e) => eprintln!("[watch] batch reindex failed: {e}"),
                }
                // prism drops here, releasing the DB lock
            }
        }

        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => {
                let watch_kind = match event.kind {
                    EventKind::Create(_) => WatchEventKind::Created,
                    EventKind::Modify(_) => WatchEventKind::Modified,
                    EventKind::Remove(_) => WatchEventKind::Removed,
                    _ => continue,
                };

                for path in event.paths {
                    if should_ignore(&path, ignore_dirs) {
                        continue;
                    }

                    let rel = match path.strip_prefix(root) {
                        Ok(r) => r.to_string_lossy().replace('\\', "/"),
                        Err(_) => continue,
                    };

                    match watch_kind {
                        WatchEventKind::Removed => {
                            if let Ok(prism) = open_transient(root, &make_registry) {
                                let _ = prism.remove_file(&path);
                                // Also remove files under this path (handles directory removal)
                                let _ = prism.remove_files_by_prefix(&path);
                            }
                            changes_since_periodic += 1;
                            on_event(WatchEvent {
                                kind: watch_kind.clone(),
                                path,
                                has_cross_file_calls: false,
                            });
                        }
                        WatchEventKind::Created | WatchEventKind::Modified => {
                            if path.is_dir() {
                                register_subdirs(&mut watcher, &path, ignore_dirs);
                            } else if filter_registry.for_file(&rel).is_some() {
                                batch.add(path);
                            }
                        }
                        WatchEventKind::WatcherRestarted | WatchEventKind::WatcherDied => {}
                    }
                }
            }
            Ok(Err(e)) => eprintln!("watch error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Watcher's internal thread died (e.g. kqueue panic on dir deletion).
                // Attempt restart with backoff.
                restart_count += 1;
                if restart_count > MAX_RESTARTS {
                    eprintln!("[watch] watcher died {restart_count} times, giving up");
                    on_event(WatchEvent {
                        kind: WatchEventKind::WatcherDied,
                        path: root.to_path_buf(),
                        has_cross_file_calls: false,
                    });
                    break;
                }
                let backoff = Duration::from_secs(restart_count as u64);
                eprintln!(
                    "[watch] watcher disconnected, restarting ({restart_count}/{MAX_RESTARTS}) after {}s",
                    backoff.as_secs()
                );
                std::thread::sleep(backoff);
                match create_watcher(root, ignore_dirs) {
                    Ok((new_watcher, new_rx)) => {
                        watcher = new_watcher;
                        rx = new_rx;
                        eprintln!("[watch] watcher restarted successfully");
                        on_event(WatchEvent {
                            kind: WatchEventKind::WatcherRestarted,
                            path: root.to_path_buf(),
                            has_cross_file_calls: false,
                        });
                    }
                    Err(e) => {
                        eprintln!("[watch] watcher restart failed: {e}");
                        on_event(WatchEvent {
                            kind: WatchEventKind::WatcherDied,
                            path: root.to_path_buf(),
                            has_cross_file_calls: false,
                        });
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Like `watch_project` but automatically re-resolves cross-file call edges
/// when affected by a change, keeping call resolution accurate without user intervention.
///
/// Instead of running a full `prism.index()` (re-parsing every file), this collects
/// the changed file plus its cross-file dependents and uses `prism.index_files()` to
/// re-index only the affected subset, then runs targeted re-resolution via
/// `resolve::re_resolve_for_files()`.
pub fn watch_project_auto_resolve<MR>(
    root: &Path,
    make_registry: MR,
    debounce_ms: u64,
    stop_rx: mpsc::Receiver<()>,
    log_prefix: &str,
) -> Result<()>
where
    MR: Fn() -> Result<crate::lang::LanguageRegistry> + Send + Sync + 'static,
{
    let root_owned = root.to_path_buf();
    let prefix = log_prefix.to_string();
    let factory: Arc<dyn Fn() -> Result<crate::lang::LanguageRegistry> + Send + Sync> =
        Arc::new(make_registry);
    let factory_for_event = Arc::clone(&factory);
    watch_project(root, move || factory(), debounce_ms, stop_rx, {
        move |evt: WatchEvent| {
            match evt.kind {
                WatchEventKind::WatcherRestarted => {
                    eprintln!("[watch {prefix}] watcher restarted after internal failure");
                    return;
                }
                WatchEventKind::WatcherDied => {
                    eprintln!("[watch {prefix}] watcher died permanently");
                    return;
                }
                _ => {}
            }
            if evt.has_cross_file_calls {
                eprintln!("[watch {prefix}] {evt}");
                if let Ok(reg) = factory_for_event() {
                    if let Ok(mut p) = Infigraph::open(&root_owned, reg) {
                        if p.init().is_ok() {
                            let changed_rel = evt
                                .path
                                .strip_prefix(&root_owned)
                                .map(|r| r.to_string_lossy().replace('\\', "/"))
                                .unwrap_or_else(|_| evt.path.to_string_lossy().replace('\\', "/"));
                            let mut affected_files = vec![evt.path.clone()];

                            if let Some(backend) = p.backend() {
                                let deps = get_cross_file_dependents(backend, &changed_rel);
                                for dep_rel in deps {
                                    let dep_abs = root_owned.join(&dep_rel);
                                    if dep_abs.exists() {
                                        affected_files.push(dep_abs);
                                    }
                                }
                            }

                            match p.index_files(&affected_files) {
                                Ok(r) => {
                                    eprintln!(
                                        "[watch {prefix}] targeted reindex: {}/{} affected files",
                                        r.indexed_files, r.total_files
                                    );

                                    if let Some(backend) = p.backend() {
                                        let file_strs: Vec<String> =
                                            r.extractions.iter().map(|e| e.file.clone()).collect();
                                        match backend.re_resolve_for_files(
                                            &file_strs,
                                            &r.extractions,
                                            None,
                                        ) {
                                            Ok(stats) => {
                                                eprintln!("[watch {prefix}] re-resolved: {stats}")
                                            }
                                            Err(e) => {
                                                eprintln!("[watch {prefix}] re-resolve failed: {e}")
                                            }
                                        }

                                        let changed: Vec<&str> =
                                            r.extractions.iter().map(|e| e.file.as_str()).collect();
                                        if let Some(eb) = p.backend() {
                                            match crate::embed::update_embeddings(
                                                eb,
                                                &root_owned,
                                                &changed,
                                            ) {
                                                Ok(n) => {
                                                    eprintln!(
                                                        "[watch {prefix}] updated {n} embeddings"
                                                    )
                                                }
                                                Err(e) => eprintln!(
                                                    "[watch {prefix}] embedding update failed: {e}"
                                                ),
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[watch {prefix}] targeted reindex failed: {e}")
                                }
                            }
                            // p drops here, releasing the DB lock
                        }
                    }
                }
            } else {
                eprintln!("[watch {prefix}] {evt}");
            }
        }
    })
}

/// Open a short-lived Infigraph instance for batch work.
fn open_transient<MR>(root: &Path, make_registry: &MR) -> Result<Infigraph>
where
    MR: Fn() -> Result<crate::lang::LanguageRegistry>,
{
    let registry = make_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    Ok(prism)
}

/// Returns the relative paths of files that have cross-file CALLS edges to/from the given file.
fn get_cross_file_dependents(
    backend: &dyn crate::graph::GraphBackend,
    rel_path: &str,
) -> Vec<String> {
    let escaped = rel_path.replace('\'', "\\'");
    let mut dependents = std::collections::HashSet::new();

    let q1 = format!(
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE a.file = '{escaped}' AND b.file <> '{escaped}' RETURN DISTINCT b.file"
    );
    if let Ok(result) = backend.raw_query(&q1) {
        for row in result {
            if let Some(val) = row.first() {
                dependents.insert(val.to_string());
            }
        }
    }

    let q2 = format!(
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE b.file = '{escaped}' AND a.file <> '{escaped}' RETURN DISTINCT a.file"
    );
    if let Ok(result) = backend.raw_query(&q2) {
        for row in result {
            if let Some(val) = row.first() {
                dependents.insert(val.to_string());
            }
        }
    }

    dependents.into_iter().collect()
}

/// Returns true if the file has any resolved CALLS edges to/from symbols in other files.
fn has_cross_file_calls(prism: &Infigraph, rel_path: &str) -> bool {
    let backend = match prism.backend() {
        Some(b) => b,
        None => return false,
    };
    let escaped = rel_path.replace('\'', "\\'");
    let q = format!(
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE a.file = '{escaped}' AND b.file <> '{escaped}' RETURN count(*) LIMIT 1"
    );
    if let Ok(result) = backend.raw_query(&q) {
        if let Some(row) = result.first() {
            if let Some(val) = row.first() {
                if val.to_string().parse::<u64>().unwrap_or(0) > 0 {
                    return true;
                }
            }
        }
    }
    let q2 = format!(
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE b.file = '{escaped}' AND a.file <> '{escaped}' RETURN count(*) LIMIT 1"
    );
    if let Ok(result) = backend.raw_query(&q2) {
        if let Some(row) = result.first() {
            if let Some(val) = row.first() {
                return val.to_string().parse::<u64>().unwrap_or(0) > 0;
            }
        }
    }
    false
}

fn should_ignore(path: &Path, ignore_dirs: &[&str]) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        ignore_dirs.contains(&s.as_ref()) || s.starts_with('.')
    })
}

fn register_watch_dirs(
    watcher: &mut RecommendedWatcher,
    root: &Path,
    ignore_dirs: &[&str],
) -> Result<()> {
    watcher.watch(root, RecursiveMode::NonRecursive)?;
    register_subdirs(watcher, root, ignore_dirs);
    Ok(())
}

fn register_subdirs(watcher: &mut RecommendedWatcher, dir: &Path, ignore_dirs: &[&str]) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ignore_dirs.contains(&name_str.as_ref()) || name_str.starts_with('.') {
            continue;
        }
        let _ = watcher.watch(&path, RecursiveMode::NonRecursive);
        register_subdirs(watcher, &path, ignore_dirs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Regression test for a macOS-specific bug: `watch_project_with_periodic`
    /// used to compare raw filesystem-watch event paths against the caller's
    /// `root` exactly as given. FSEvents delivers absolute, symlink-resolved
    /// event paths, so a non-canonical `root` (e.g. a relative path, or one
    /// that traverses a symlink) made `path.strip_prefix(root)` fail for
    /// every event, silently dropping all changes with no error. This is the
    /// same class of bug that made the `infigraph watch` CLI command — which
    /// watched the unresolved `.` — appear to receive no events at all,
    /// prompting a workaround (the kqueue backend) that caused a much larger
    /// file-descriptor leak.
    ///
    /// A custom-prefixed `tempfile::TempDir` reproduces a non-canonical root
    /// deterministically on macOS: it lives under `/var/folders/...`, itself
    /// a symlink to `/private/var/folders/...`. (The default `TempDir::new()`
    /// prefix starts with a dot, which the watcher's own hidden-file filter
    /// would ignore regardless of this bug, so a custom prefix is used to
    /// keep the test isolated to the canonicalization behavior.)
    #[test]
    #[cfg(target_os = "macos")]
    fn watch_project_detects_changes_through_symlinked_root() {
        let tmp = tempfile::Builder::new()
            .prefix("infigraph-watch-test-")
            .tempdir()
            .unwrap();
        let raw_root = tmp.path().to_path_buf();
        let canonical_root = raw_root.canonicalize().unwrap();
        assert_ne!(
            raw_root, canonical_root,
            "test assumption broken: TempDir root is already canonical on this machine"
        );

        let file_path = raw_root.join("watched.txt");
        std::fs::write(&file_path, "v1").unwrap();

        let events: Arc<Mutex<Vec<WatchEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        let (stop_tx, stop_rx) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            watch_project(
                &raw_root,
                || Ok(crate::lang::LanguageRegistry::new()),
                50,
                stop_rx,
                move |evt| events_clone.lock().unwrap().push(evt),
            )
        });

        // Give the watcher time to register before triggering a change.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::remove_file(&file_path).unwrap();

        // Poll rather than a single fixed sleep: fast on a quiet machine,
        // robust on a loaded one.
        let mut seen = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            if !events.lock().unwrap().is_empty() {
                seen = true;
                break;
            }
        }

        let _ = stop_tx.send(());
        let _ = handle.join();

        assert!(
            seen,
            "watch_project delivered no events for a change under a non-canonical \
             (symlinked) root — the root.canonicalize() call in \
             watch_project_with_periodic may have regressed"
        );
    }
}
