//! Local analysis-stage receipts. These describe execution, not graph freshness
//! or proof that every possible relationship has been discovered.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::graph::GraphBackend;
use crate::lockfile::{self, LockFile};

const SCHEMA_VERSION: u32 = 1;
const MAX_REPORT_BYTES: u64 = 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageOutcome {
    pub state: StageState,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}
impl StageOutcome {
    pub fn new(state: StageState, reason: &str) -> Self {
        Self {
            state,
            reason: reason.into(),
            exit_code: None,
            signal: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexCoverageReport {
    pub schema_version: u32,
    pub run_id: String,
    pub indexed_file_fingerprint: String,
    pub languages: BTreeSet<String>,
    pub resolution: StageOutcome,
    pub scip: BTreeMap<String, StageOutcome>,
}
impl IndexCoverageReport {
    pub fn has_failures(&self) -> bool {
        self.resolution.state == StageState::Failed
            || self.scip.values().any(|s| s.state == StageState::Failed)
    }
}

pub fn file_fingerprint(backend: &dyn GraphBackend) -> Result<String> {
    let hashes: BTreeMap<_, _> = backend.get_file_hashes()?.into_iter().collect();
    let mut hash = Sha256::new();
    for (file, fingerprint) in hashes {
        for part in [file, fingerprint] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn load(root: &Path) -> Result<IndexCoverageReport> {
    let file = std::fs::File::open(root.join(".infigraph/index-status.json"))?;
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_REPORT_BYTES,
        "coverage report too large"
    );
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_REPORT_BYTES,
        "coverage report too large"
    );
    let report: IndexCoverageReport = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        report.schema_version == SCHEMA_VERSION,
        "unsupported coverage schema"
    );
    Ok(report)
}

fn lock(root: &Path) -> Result<LockFile> {
    lockfile::acquire(
        &root.join(".infigraph/index-status.lock"),
        "index-status",
        Duration::from_secs(30),
    )
}
fn save(root: &Path, report: &IndexCoverageReport) -> Result<()> {
    let dir = root.join(".infigraph");
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer_pretty(&mut file, report)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(dir.join("index-status.json"))?;
    Ok(())
}

/// Serializes an AST mutation with enrichment imports. Dropping an unfinished
/// run records failure; an abrupt process exit leaves a visibly unverified stage.
pub(crate) struct AstRun<'a> {
    root: &'a Path,
    _lock: LockFile,
    report: IndexCoverageReport,
    previous: Option<IndexCoverageReport>,
    finished: bool,
}
impl<'a> AstRun<'a> {
    pub(crate) fn begin(root: &'a Path) -> Result<Self> {
        let guard = lock(root)?;
        let previous = load(root).ok();
        let run_id = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let report = IndexCoverageReport {
            schema_version: SCHEMA_VERSION,
            run_id,
            indexed_file_fingerprint: String::new(),
            languages: BTreeSet::new(),
            resolution: StageOutcome::new(StageState::Running, "AST_RUNNING"),
            scip: BTreeMap::new(),
        };
        save(root, &report)?;
        Ok(Self {
            root,
            _lock: guard,
            report,
            previous,
            finished: false,
        })
    }
    pub(crate) fn finish(
        mut self,
        backend: &dyn GraphBackend,
        resolution_failed: bool,
        resolution_attempted: bool,
    ) -> Result<()> {
        self.report.indexed_file_fingerprint = file_fingerprint(backend)?;
        self.report.languages = backend
            .raw_query("MATCH (m:Module) RETURN DISTINCT m.language")?
            .into_iter()
            .filter_map(|r| r.into_iter().next())
            .collect();
        if let Some(previous) = &self.previous {
            if previous.indexed_file_fingerprint == self.report.indexed_file_fingerprint {
                self.report.scip.clone_from(&previous.scip);
            }
        }
        self.report.resolution = if resolution_failed {
            StageOutcome::new(StageState::Failed, "CALL_RESOLUTION_FAILED")
        } else {
            StageOutcome::new(StageState::Succeeded, "CALL_RESOLUTION_SUCCEEDED")
        };
        if !resolution_attempted {
            if let Some(previous) = &self.previous {
                if previous.indexed_file_fingerprint == self.report.indexed_file_fingerprint {
                    self.report.resolution = previous.resolution.clone();
                }
            }
        }
        save(self.root, &self.report)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for AstRun<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.report.resolution = StageOutcome::new(StageState::Failed, "AST_INTERRUPTED");
            let _ = save(self.root, &self.report);
        }
    }
}

/// Apply a receipt update (and optionally an import) only for the current AST
/// generation. All local AST mutations acquire this same lock first.
pub fn update_current<T>(
    root: &Path,
    run_id: &str,
    backend: Option<&dyn GraphBackend>,
    update: impl FnOnce(&mut IndexCoverageReport) -> Result<T>,
) -> Result<Option<T>> {
    let _guard = lock(root)?;
    let mut report = load(root)?;
    if report.run_id != run_id {
        return Ok(None);
    }
    if let Some(backend) = backend {
        if report.indexed_file_fingerprint != file_fingerprint(backend)? {
            return Ok(None);
        }
    }
    let result = update(&mut report)?;
    save(root, &report)?;
    Ok(Some(result))
}

/// Reads only the receipt and indexed module hashes; never creates a lock,
/// starts a watcher, repairs an index, or claims working-tree freshness.
pub fn coverage_notice(root: &Path, backend: Option<&dyn GraphBackend>) -> String {
    let Some(backend) = backend else {
        return "Warning: analysis coverage unknown (remote backend).\n".into();
    };
    let report = match load(root).and_then(|r| {
        anyhow::ensure!(
            r.indexed_file_fingerprint == file_fingerprint(backend)?,
            "index generation differs"
        );
        Ok(r)
    }) {
        Ok(report) => report,
        Err(_) => {
            return "Warning: analysis coverage unknown; no valid receipt for this graph.\n".into()
        }
    };
    let mut warnings = Vec::new();
    if report.resolution.state != StageState::Succeeded {
        warnings.push(format!(
            "call resolution {:?} ({})",
            report.resolution.state, report.resolution.reason
        ));
    }
    for (label, stage) in &report.scip {
        if stage.state != StageState::Succeeded {
            warnings.push(format!("{label} {:?} ({})", stage.state, stage.reason));
        }
    }
    if report.scip.is_empty() {
        warnings.push("no compiler enrichment recorded".into());
    }
    if warnings.is_empty() {
        "Coverage: recorded analysis stages succeeded; relationships remain best-effort.\n".into()
    } else {
        format!("Warning: partial or unknown analysis coverage: {}. Pending/running stages may have been interrupted; empty results do not prove absence.\n", warnings.join("; "))
    }
}

/// Run-owned artifacts are removed on drop, including failed imports.
pub fn scip_scratch(root: &Path) -> Result<tempfile::TempDir> {
    std::fs::create_dir_all(root.join(".infigraph"))?;
    Ok(tempfile::Builder::new()
        .prefix("scip-run-")
        .tempdir_in(root.join(".infigraph"))?)
}

pub fn current_run(root: &Path) -> Result<IndexCoverageReport> {
    load(root).context("analysis coverage receipt unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::LanguageRegistry;
    use crate::Infigraph;

    #[test]
    fn receipts_are_project_scoped_and_fail_closed_without_writes() {
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for dir in &dirs {
            std::fs::write(dir.path().join("a.ts"), "export const helper = () => 1;").unwrap();
        }
        let mut a = Infigraph::open(dirs[0].path(), LanguageRegistry::new()).unwrap();
        a.init().unwrap();
        a.index().unwrap();
        let mut b = Infigraph::open(dirs[1].path(), LanguageRegistry::new()).unwrap();
        b.init().unwrap();
        b.index().unwrap();
        let run = load(dirs[0].path()).unwrap().run_id;
        update_current(dirs[0].path(), &run, a.backend(), |r| {
            r.scip.insert(
                "scip-typescript".into(),
                StageOutcome::new(StageState::Failed, "SCIP_IMPORT_FAILED"),
            );
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(a.coverage_notice().contains("SCIP_IMPORT_FAILED"));
        assert!(!b.coverage_notice().contains("SCIP_IMPORT_FAILED"));
        update_current(dirs[0].path(), &run, a.backend(), |r| {
            r.scip.insert(
                "scip-typescript".into(),
                StageOutcome::new(StageState::Succeeded, "SCIP_IMPORTED"),
            );
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(a.coverage_notice().starts_with("Coverage:"));
        update_current(dirs[0].path(), &run, a.backend(), |r| {
            r.resolution = StageOutcome::new(StageState::Failed, "CALL_RESOLUTION_FAILED");
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(a.index().unwrap().indexed_files, 0);
        assert_eq!(
            load(dirs[0].path()).unwrap().resolution.reason,
            "CALL_RESOLUTION_FAILED"
        );
        let path = dirs[0].path().join(".infigraph/index-status.json");
        let mut report = load(dirs[0].path()).unwrap();
        report.indexed_file_fingerprint = "obsolete".into();
        save(dirs[0].path(), &report).unwrap();
        assert!(a.coverage_notice().contains("unknown"));
        assert!(
            update_current::<()>(dirs[0].path(), &report.run_id, a.backend(), |_| {
                panic!("mismatched graph must not update its receipt")
            })
            .unwrap()
            .is_none()
        );
        report.schema_version += 1;
        save(dirs[0].path(), &report).unwrap();
        let unsupported = std::fs::read(&path).unwrap();
        assert!(a.coverage_notice().contains("unknown"));
        assert_eq!(std::fs::read(&path).unwrap(), unsupported);
        for bytes in [b"{broken".as_slice(), b"{}".as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            assert!(a.coverage_notice().contains("unknown"));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        assert!(a.coverage_notice().contains("unknown"));
        assert!(!path.exists());
        assert!(coverage_notice(dirs[0].path(), None).contains("remote"));
    }

    #[test]
    fn unfinished_and_obsolete_runs_cannot_claim_completion() {
        let dir = tempfile::tempdir().unwrap();
        let mut prism = Infigraph::open(dir.path(), LanguageRegistry::new()).unwrap();
        prism.init().unwrap();
        prism.index().unwrap();
        let old = load(dir.path()).unwrap().run_id;
        {
            let _unfinished = AstRun::begin(dir.path()).unwrap();
        }
        assert_eq!(
            load(dir.path()).unwrap().resolution.reason,
            "AST_INTERRUPTED"
        );
        assert!(
            update_current(dir.path(), &old, prism.backend(), |_| Ok(()))
                .unwrap()
                .is_none()
        );
        let run = load(dir.path()).unwrap().run_id;
        update_current(dir.path(), &run, None, |r| {
            r.scip.insert(
                "scip-typescript".into(),
                StageOutcome::new(StageState::Running, "SCIP_RUNNING"),
            );
            Ok(())
        })
        .unwrap();
        assert!(prism.coverage_notice().contains("unknown"));
    }
}
