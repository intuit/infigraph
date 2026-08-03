//! Tracks the git commit a project's graph was last built from, so callers can
//! detect when the graph may no longer match the working tree (branch switch,
//! rebase, uncommitted edits, or a watcher that missed changes while down).

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

const META_FILE: &str = "index_meta.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexMeta {
    indexed_head: Option<String>,
    indexed_at: String,
}

fn meta_path(root: &Path) -> std::path::PathBuf {
    root.join(".infigraph").join(META_FILE)
}

fn git_rev_parse_head(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_is_dirty(root: &Path) -> bool {
    // `.infigraph/` itself is Infigraph's own bookkeeping (this file included).
    // On a project that hasn't gitignored it yet, its mere presence would
    // otherwise make every freshness check report "dirty" forever.
    std::process::Command::new("git")
        .args(["status", "--porcelain", "--", ".", ":!.infigraph"])
        .current_dir(root)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Stamp the current git HEAD as the commit this project's graph was just
/// built from. Called after a successful `Infigraph::index()`/`index_files()`.
/// No-ops (does not error) if `root` isn't a git repository.
pub fn write_index_meta(root: &Path) -> Result<()> {
    let meta = IndexMeta {
        indexed_head: git_rev_parse_head(root),
        indexed_at: chrono_now_rfc3339(),
    };
    let path = meta_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

fn chrono_now_rfc3339() -> String {
    // Avoid pulling in a datetime crate for a single timestamp field: seconds
    // since epoch is sufficient for "how stale is this" comparisons.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessStatus {
    /// Indexed HEAD matches current HEAD, tree is clean, no pending reindex.
    Fresh,
    /// Indexed HEAD differs from current HEAD, or the working tree is dirty.
    Stale,
    /// HEAD matches, but a watcher has files queued for reindex.
    Updating,
    /// Not a git repo, or the graph has no recorded indexed HEAD yet.
    Unknown,
}

impl std::fmt::Display for FreshnessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FreshnessStatus::Fresh => "fresh",
            FreshnessStatus::Stale => "stale",
            FreshnessStatus::Updating => "updating",
            FreshnessStatus::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub struct Freshness {
    pub status: FreshnessStatus,
    pub indexed_head: Option<String>,
    pub current_head: Option<String>,
    pub working_tree_dirty: bool,
    pub pending_changes: usize,
}

impl Freshness {
    /// A one-line warning suitable for prepending to a tool response.
    /// Returns `None` when status is `Fresh` (nothing to warn about) or
    /// `Unknown` (not a git repo, or never indexed with freshness tracking —
    /// nothing wrong has actually been detected, so there's nothing to warn).
    pub fn warning_line(&self) -> Option<String> {
        if matches!(
            self.status,
            FreshnessStatus::Fresh | FreshnessStatus::Unknown
        ) {
            return None;
        }
        let indexed = self.indexed_head.as_deref().unwrap_or("unknown");
        let current = self.current_head.as_deref().unwrap_or("unknown");
        let mut reasons = Vec::new();
        if self.indexed_head != self.current_head {
            reasons.push("branch/commit changed".to_string());
        }
        if self.working_tree_dirty {
            reasons.push("uncommitted changes".to_string());
        }
        if self.pending_changes > 0 {
            reasons.push(format!("{} file(s) pending reindex", self.pending_changes));
        }
        let reason = if reasons.is_empty() {
            self.status.to_string()
        } else {
            reasons.join(", ")
        };
        Some(format!(
            "⚠ {status}: indexed_head={indexed} current_head={current} ({reason}) — run index_project to refresh\n\n",
            status = self.status
        ))
    }
}

/// Compute freshness for `root` by comparing the recorded indexed HEAD against
/// the live git state. `pending_changes` should be the number of files a
/// running watcher has queued for reindex (0 if no watcher is tracked).
pub fn compute_freshness(root: &Path, pending_changes: usize) -> Freshness {
    let current_head = git_rev_parse_head(root);
    let working_tree_dirty = current_head.is_some() && git_is_dirty(root);

    let indexed_head = std::fs::read_to_string(meta_path(root))
        .ok()
        .and_then(|s| serde_json::from_str::<IndexMeta>(&s).ok())
        .and_then(|m| m.indexed_head);

    let status = if current_head.is_none() || indexed_head.is_none() {
        FreshnessStatus::Unknown
    } else if indexed_head != current_head || working_tree_dirty {
        FreshnessStatus::Stale
    } else if pending_changes > 0 {
        FreshnessStatus::Updating
    } else {
        FreshnessStatus::Fresh
    };

    Freshness {
        status,
        indexed_head,
        current_head,
        working_tree_dirty,
        pending_changes,
    }
}
