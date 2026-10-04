//! Deterministic local observations. Never initialize, recover, or repair state.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::graph::{GraphBackend, KuzuBackend};
use crate::lockfile::{self, Observation};
use crate::multi::{git_head_commit, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    #[serde(rename = "ok")]
    Pass,
    Unavailable,
    Unknown,
    Problem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Versioned wire codes: messages may change; these identifiers must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiagnosticCode {
    ProjectReadable,
    ProjectUnreadable,
    ProjectNotIndexed,
    GraphReadable,
    GraphUnreadable,
    GraphEmpty,
    GraphStatusUnknown,
    GraphLockFree,
    GraphLockBusy,
    GraphLockUnknown,
    RegistryInvalid,
    ProjectRegistered,
    ProjectRegistrationUnknown,
    IndexStale,
    IndexRevisionMatch,
    IndexFreshnessUnknown,
    WatcherActive,
    WatcherAbsent,
    WatcherStatusUnknown,
    WatcherPendingReindex,
    EmbeddingsHeaderReadable,
    EmbeddingsMissing,
    EmbeddingsUnreadable,
    McpBinaryFound,
    McpBinaryMissing,
    McpConfigStatusUnknown,
    RemoteStatusUnknown,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticCheck {
    pub code: DiagnosticCode,
    pub status: CheckStatus,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub schema_version: u32,
    pub healthy: bool,
    pub status: Health,
    pub checks: Vec<DiagnosticCheck>,
}

impl Default for DiagnosticReport {
    fn default() -> Self {
        Self {
            schema_version: 1,
            healthy: true,
            status: Health::Healthy,
            checks: Vec::new(),
        }
    }
}

impl DiagnosticReport {
    pub fn push(
        &mut self,
        code: DiagnosticCode,
        status: CheckStatus,
        severity: Severity,
        message: impl Into<String>,
        action: Option<&str>,
    ) {
        self.checks.push(DiagnosticCheck {
            code,
            status,
            severity,
            message: message.into(),
            recommended_action: action.map(str::to_owned),
        });
        self.status = if self.checks.iter().any(|c| c.severity == Severity::Error) {
            Health::Unhealthy
        } else if self.checks.iter().any(|c| c.severity == Severity::Warning) {
            Health::Degraded
        } else {
            Health::Healthy
        };
        self.healthy = self.status == Health::Healthy;
    }
    pub fn exit_code(&self) -> i32 {
        match self.status {
            Health::Healthy => 0,
            Health::Degraded => 1,
            Health::Unhealthy => 2,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphCounts {
    files: u64,
    symbols: u64,
}

const PROBE_ARG: &str = "--infigraph-diagnostic-probe";

/// Called by both executable entry points *before* any normal startup. The
/// hidden subprocess mode never enters the MCP supervisor/recovery path.
pub fn run_probe_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(PROBE_ARG)) {
        return None;
    }
    let Some(path) = args.next() else {
        return Some(2);
    };
    if args.next().is_some() {
        return Some(2);
    }
    // Prevent native parser failures from generating a core dump containing
    // graph data. This affects only this dedicated child process.
    #[cfg(unix)]
    unsafe {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
            return Some(2);
        }
        // A corrupt native size field must not exhaust the host. Stay within
        // any stricter inherited limit; unsupported probes return UNKNOWN.
        let mut address = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_AS, &mut address) != 0 {
            return Some(2);
        }
        address.rlim_cur = address
            .rlim_cur
            .min(libc::rlim_t::try_from(4_u64 << 30).unwrap_or(libc::RLIM_INFINITY));
        if libc::setrlimit(libc::RLIMIT_AS, &address) != 0 {
            return Some(2);
        }
    }
    match probe_graph(Path::new(&path)) {
        Ok(counts) => match serde_json::to_string(&counts) {
            Ok(json) => {
                println!("{json}");
                Some(0)
            }
            Err(_) => Some(2),
        },
        Err(_) => Some(2),
    }
}

fn probe_graph(path: &Path) -> anyhow::Result<GraphCounts> {
    // Repeat preflight in the child; state may change after the parent check.
    anyhow::ensure!(
        path.is_file() && path.metadata()?.len() >= 4096,
        "invalid graph file"
    );
    anyhow::ensure!(!has_wal(path)?, "WAL inspection unsupported");
    let lock = lockfile::observe(&path.with_extension("lock"))?;
    anyhow::ensure!(!matches!(lock, Observation::Held), "graph busy");
    let config = kuzu::SystemConfig::default()
        .buffer_pool_size(64 * 1024 * 1024)
        .max_db_size(1024 * 1024 * 1024)
        .max_num_threads(2);
    let backend = KuzuBackend::from_store(crate::graph::GraphStore::open_read_only_with_config(
        path, config,
    )?);
    let stats = backend.stats()?;
    Ok(GraphCounts {
        files: stats.files,
        symbols: stats.symbols,
    })
}

fn isolated_probe(path: &Path) -> anyhow::Result<GraphCounts> {
    let executable = std::env::current_exe()?;
    // Only our two entry points implement the side-effect-free probe mode.
    // Never launch an arbitrary embedding application's normal startup.
    anyhow::ensure!(
        matches!(
            executable.file_name().and_then(|n| n.to_str()),
            Some("infigraph" | "infigraph.exe" | "infigraph-mcp" | "infigraph-mcp.exe")
        ),
        "host executable does not support diagnostic probes"
    );
    let mut child = Command::new(executable)
        .arg(PROBE_ARG)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("probe unavailable");
            }
        }
    }
    let output = child.wait_with_output()?;
    anyhow::ensure!(output.status.success(), "probe unavailable");
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn has_wal(path: &Path) -> std::io::Result<bool> {
    // Kuzu/LadybugDB WAL-family siblings, including graph.wal.*.
    let wal = path.with_extension("wal");
    let Some(name) = wal.file_name().and_then(|s| s.to_str()) else {
        return Ok(true);
    };
    for entry in std::fs::read_dir(path.parent().unwrap_or(Path::new(".")))? {
        let name_on_disk = entry?.file_name();
        let name_on_disk = name_on_disk.to_string_lossy();
        if name_on_disk == name || name_on_disk.starts_with(&format!("{name}.")) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn diagnose(root: &Path) -> DiagnosticReport {
    diagnose_with(root, &isolated_probe, &Registry::load)
}

fn diagnose_with(
    root: &Path,
    probe: &impl Fn(&Path) -> anyhow::Result<GraphCounts>,
    registry: &impl Fn() -> anyhow::Result<Registry>,
) -> DiagnosticReport {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    let mut report = DiagnosticReport::default();
    let root = match root
        .canonicalize()
        .and_then(|p| std::fs::read_dir(&p).map(|_| p))
    {
        Ok(root) => root,
        Err(_) => {
            report.push(
                ProjectUnreadable,
                Problem,
                Error,
                "Project directory is missing or unreadable.",
                Some("Check the project path and filesystem permissions."),
            );
            return report;
        }
    };
    report.push(
        ProjectReadable,
        Pass,
        Info,
        "Project directory is readable.",
        None,
    );
    let backend = std::env::var("INFIGRAPH_BACKEND").unwrap_or_else(|_| "kuzu".into());
    if backend != "kuzu" {
        report.push(
            RemoteStatusUnknown,
            Unknown,
            Warning,
            "Selected backend is not inspected; no connection was attempted.",
            Some("Check the selected backend configuration and service separately."),
        );
    } else {
        check_graph(&mut report, &root, probe);
        check_registry(&mut report, &root, registry);
        check_watcher(&mut report, &root);
        check_embeddings(&mut report, &root);
    }
    check_installation(&mut report, crate::installation::find_mcp_binary().is_ok());
    report
}

fn check_installation(report: &mut DiagnosticReport, binary_found: bool) {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    if binary_found {
        report.push(
            McpBinaryFound,
            Pass,
            Info,
            "infigraph-mcp executable found beside this binary or on PATH.",
            None,
        );
    } else {
        report.push(
            McpBinaryMissing,
            Unavailable,
            Info,
            "MCP binary not found; CLI-only use is supported.",
            Some("Build with `cargo build -p infigraph-mcp` or put infigraph-mcp on PATH."),
        );
    }
    report.push(
        McpConfigStatusUnknown,
        Unknown,
        Info,
        "Agent registration and managed artifacts are not inspected in this version.",
        None,
    );
}

fn check_graph(
    report: &mut DiagnosticReport,
    root: &Path,
    probe: &impl Fn(&Path) -> anyhow::Result<GraphCounts>,
) {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    let path = root.join(".infigraph/graph");
    let meta = match path.metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.push(
                ProjectNotIndexed,
                Problem,
                Error,
                "No local graph index found.",
                Some("Run `infigraph index`."),
            );
            return;
        }
        Err(_) => {
            report.push(
                GraphUnreadable,
                Problem,
                Error,
                "Graph file metadata is unreadable.",
                Some("Check filesystem permissions; preserve the graph before investigating."),
            );
            return;
        }
    };
    if !meta.is_file() || meta.len() < 4096 {
        report.push(
            GraphUnreadable,
            Problem,
            Error,
            "Graph is not a regular database file or is truncated; it was not opened.",
            Some("Preserve the graph and inspect the database format and filesystem state."),
        );
        return;
    }
    let lock = match lockfile::observe(&path.with_extension("lock")) {
        Ok(Observation::Held) => {
            report.push(
                GraphLockBusy,
                Unknown,
                Warning,
                match lockfile::read_holder(&path.with_extension("lock")) {
                    Some(holder) => format!("Graph write lock is held (recorded PID {}); graph inspection skipped. Recorded identity is not proof of process liveness.", holder.pid),
                    None => "Graph write lock is held by an unknown owner; graph inspection skipped.".into(),
                },
                Some("Wait for indexing to finish; check `infigraph watch-status`."),
            );
            return;
        }
        Ok(lock) => {
            report.push(
                GraphLockFree,
                Pass,
                Info,
                "Graph advisory write lock is not held.",
                None,
            );
            lock
        }
        Err(_) => {
            report.push(
                GraphLockUnknown,
                Unknown,
                Warning,
                "Graph lock could not be observed; graph inspection skipped.",
                Some("Check filesystem permissions."),
            );
            return;
        }
    };
    // Keep the shared lock guard through the isolated open and query.
    let _lock = lock;
    if !matches!(has_wal(&path), Ok(false)) {
        report.push(GraphStatusUnknown, Unknown, Warning, "WAL-family files exist or could not be inspected; database was not opened.", Some("Allow the writer to finish. Preserve graph/WAL files if the writer exited uncleanly."));
        return;
    }
    match probe(&path) {
        Ok(counts) => {
            report.push(GraphReadable, Pass, Info, format!("Read-only graph statistics succeeded: {} files, {} symbols.", counts.files, counts.symbols), None);
            if counts.files == 0 {
                report.push(GraphEmpty, Problem, Warning, "Graph has no indexed files.", Some("Run `infigraph index`; check that the project contains supported files."));
            }
        }
        Err(_) => report.push(GraphStatusUnknown, Unknown, Warning, "Read-only graph probe failed, exceeded its resource budget, or timed out; graph was left untouched.", Some("Check graph size, filesystem permissions and database version; preserve graph files before investigating.")),
    }
}

fn check_registry(
    report: &mut DiagnosticReport,
    root: &Path,
    load: &impl Fn() -> anyhow::Result<Registry>,
) {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    let registry = match load() {
        Ok(registry) => registry,
        Err(_) => {
            report.push(
                RegistryInvalid,
                Problem,
                Warning,
                "Local project registry is unreadable or malformed.",
                Some("Inspect the local registry and permissions; do not discard it."),
            );
            return;
        }
    };
    let entries: Vec<_> = registry
        .repos
        .values()
        .filter(|e| e.path.canonicalize().is_ok_and(|p| p == root))
        .collect();
    if entries.is_empty() {
        report.push(ProjectRegistrationUnknown, Unknown, Info, "Project is not recorded in the registry; local graph use does not require registration.", None);
    } else {
        report.push(
            ProjectRegistered,
            Pass,
            Info,
            "Project is recorded in the local registry.",
            None,
        );
    }
    let head = git_head_commit(root);
    let revisions: Vec<_> = entries
        .iter()
        .filter_map(|e| e.last_indexed_commit.as_deref())
        .collect();
    match head {
        Some(head) if !revisions.is_empty() && revisions.iter().any(|r| *r != head) => report.push(IndexStale, Problem, Warning, "Current HEAD differs from a registry-recorded indexed revision; registry metadata may lag watcher updates.", Some("Run `infigraph index`, then rerun doctor.")),
        Some(_) if !revisions.is_empty() => report.push(IndexRevisionMatch, Pass, Info, "HEAD matches the registry-recorded revision; working-tree freshness is not established.", None),
        _ => report.push(IndexFreshnessUnknown, Unknown, Info, "No comparable Git/index revision metadata; freshness is unknown (non-Git projects are supported).", None),
    }
}

fn check_watcher(report: &mut DiagnosticReport, root: &Path) {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    match lockfile::observe(&root.join(".infigraph/watch.lock")) {
        Ok(Observation::Held) => report.push(
            WatcherActive,
            Pass,
            Info,
            "Watch lock has an owner; watcher progress and index freshness are unknown.",
            None,
        ),
        Ok(_) => report.push(
            WatcherAbsent,
            Unavailable,
            Info,
            "No watch lock owner observed; watching is optional.",
            None,
        ),
        Err(_) => report.push(
            WatcherStatusUnknown,
            Unknown,
            Info,
            "Watch lock could not be observed.",
            Some("Check filesystem permissions; run `infigraph watch-status`."),
        ),
    }
}

fn check_embeddings(report: &mut DiagnosticReport, root: &Path) {
    use CheckStatus::*;
    use DiagnosticCode::*;
    use Severity::*;
    match crate::embed::embedding_count_checked(root) {
        Ok(count) => report.push(EmbeddingsHeaderReadable, Pass, Info, format!("Embedding header readable ({count} entries); full contents/model/HNSW validity not verified."), None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.push(EmbeddingsMissing, Unavailable, Info, "Optional persisted embeddings missing; search can compute embeddings on demand.", None),
        Err(_) => report.push(EmbeddingsUnreadable, Problem, Warning, "Persisted embedding header is unreadable or truncated; search may fail while this asset is present.", Some("Check embedding asset permissions and format.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lang::LanguageRegistry, Infigraph};

    fn codes(report: &DiagnosticReport) -> Vec<DiagnosticCode> {
        report.checks.iter().map(|c| c.code).collect()
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "def greet():\n    return 1\n").unwrap();
        let mut registry = LanguageRegistry::new();
        registry.register(
            crate::lang::LanguagePack::new(
                "python",
                vec![".py"],
                tree_sitter_python::LANGUAGE.into(),
                include_str!("../../infigraph-languages/languages/python/entities.scm"),
                include_str!("../../infigraph-languages/languages/python/relations.scm"),
            )
            .unwrap(),
        );
        let mut graph = Infigraph::open(dir.path(), registry).unwrap();
        graph.init().unwrap();
        assert_eq!(graph.index().unwrap().indexed_files, 1);
        dir
    }
    fn report(root: &Path) -> DiagnosticReport {
        diagnose_with(root, &probe_graph, &|| Ok(Registry::default()))
    }
    #[test]
    fn healthy_graph_is_readable_and_unchanged() {
        let dir = fixture();
        let path = dir.path().join(".infigraph/graph");
        let before = std::fs::read(&path).unwrap();
        let result = report(dir.path());
        assert_eq!(
            result.status,
            Health::Healthy,
            "{result:?}; files: {:?}",
            std::fs::read_dir(dir.path().join(".infigraph"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );
        assert_eq!(result.exit_code(), 0);
        assert!(codes(&result).contains(&DiagnosticCode::GraphReadable));
        assert!(codes(&result).contains(&DiagnosticCode::WatcherAbsent));
        assert!(codes(&result).contains(&DiagnosticCode::EmbeddingsMissing));
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(!dir.path().join(".infigraph/watch.lock").exists());
    }
    #[test]
    fn not_indexed_never_creates_state() {
        let dir = tempfile::tempdir().unwrap();
        let result = report(dir.path());
        assert_eq!(result.status, Health::Unhealthy);
        assert_eq!(result.exit_code(), 2);
        assert!(codes(&result).contains(&DiagnosticCode::ProjectNotIndexed));
        assert!(!dir.path().join(".infigraph").exists());
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert!(json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "PROJECT_NOT_INDEXED"
                && c["recommended_action"] == "Run `infigraph index`."));
    }
    #[test]
    fn invalid_graph_is_not_opened_or_repaired() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".infigraph")).unwrap();
        let path = dir.path().join(".infigraph/graph");
        std::fs::write(&path, b"truncated database").unwrap();
        let result = diagnose_with(
            dir.path(),
            &|_| panic!("must not open invalid graph"),
            &|| Ok(Registry::default()),
        );
        assert_eq!(result.status, Health::Unhealthy);
        assert!(codes(&result).contains(&DiagnosticCode::GraphUnreadable));
        assert_eq!(std::fs::read(path).unwrap(), b"truncated database");
        assert_eq!(
            std::fs::read_dir(dir.path().join(".infigraph"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn wal_is_unknown_without_opening_even_with_no_lock_payload() {
        let dir = fixture();
        let wal = dir.path().join(".infigraph/graph.wal.orphan");
        std::fs::write(&wal, b"retain this WAL").unwrap();
        let result = diagnose_with(dir.path(), &|_| panic!("must not open WAL graph"), &|| {
            Ok(Registry::default())
        });
        assert_eq!(result.status, Health::Degraded);
        assert_eq!(result.exit_code(), 1);
        assert!(codes(&result).contains(&DiagnosticCode::GraphStatusUnknown));
        assert_eq!(std::fs::read(wal).unwrap(), b"retain this WAL");
    }
    #[test]
    fn held_graph_lock_skips_probe_without_changing_payload() {
        let dir = fixture();
        let path = dir.path().join(".infigraph/graph.lock");
        let _guard = lockfile::try_acquire(&path, "test").unwrap().unwrap();
        let before = std::fs::read(&path).unwrap();
        let result = diagnose_with(
            dir.path(),
            &|_| panic!("must not probe busy graph"),
            &|| Ok(Registry::default()),
        );
        assert!(codes(&result).contains(&DiagnosticCode::GraphLockBusy));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn registry_revision_change_is_stale_without_timing() {
        use crate::multi::RepoEntry;
        let dir = fixture();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git failed");
        };
        git(&["init"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "indexed",
        ]);
        let revision = git_head_commit(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.repos.insert(
            "fixture".into(),
            RepoEntry {
                name: "fixture".into(),
                path: dir.path().canonicalize().unwrap(),
                languages: vec![],
                symbol_count: 1,
                module_count: 0,
                last_indexed_commit: Some(revision),
            },
        );
        let loader = || {
            Ok(
                serde_json::from_value::<Registry>(serde_json::to_value(&registry).unwrap())
                    .unwrap(),
            )
        };
        let before = diagnose_with(dir.path(), &probe_graph, &loader);
        assert!(codes(&before).contains(&DiagnosticCode::IndexRevisionMatch));
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "changed HEAD",
        ]);
        let after = diagnose_with(dir.path(), &probe_graph, &loader);
        assert!(codes(&after).contains(&DiagnosticCode::IndexStale));
        assert_eq!(after.status, Health::Degraded);
    }
    #[test]
    fn missing_mcp_binary_and_config_are_informational() {
        let mut report = DiagnosticReport::default();
        check_installation(&mut report, false);
        assert_eq!(report.status, Health::Healthy);
        assert!(codes(&report).contains(&DiagnosticCode::McpBinaryMissing));
        assert!(codes(&report).contains(&DiagnosticCode::McpConfigStatusUnknown));
        assert!(report.checks[0].recommended_action.is_some());
    }
    #[test]
    fn unreadable_persisted_assets_degrade_without_repair() {
        let dir = fixture();
        std::fs::write(dir.path().join(".infigraph/embeddings.bin"), b"bad").unwrap();
        let mut result = DiagnosticReport::default();
        check_embeddings(&mut result, dir.path());
        assert_eq!(result.status, Health::Degraded);
        assert!(codes(&result).contains(&DiagnosticCode::EmbeddingsUnreadable));
        assert_eq!(
            std::fs::read(dir.path().join(".infigraph/embeddings.bin")).unwrap(),
            b"bad"
        );
    }
    #[test]
    fn malformed_registry_does_not_leak_parser_input() {
        let dir = tempfile::tempdir().unwrap();
        let result = diagnose_with(dir.path(), &probe_graph, &|| anyhow::bail!("SECRET_TOKEN"));
        assert!(codes(&result).contains(&DiagnosticCode::RegistryInvalid));
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("SECRET_TOKEN"));
    }
}
