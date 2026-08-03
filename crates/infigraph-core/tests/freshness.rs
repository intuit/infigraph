use infigraph_core::freshness::{compute_freshness, write_index_meta, FreshnessStatus};
use std::process::Command;
use tempfile::TempDir;

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git command failed to run");
    assert!(status.success(), "git {:?} failed", args);
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    dir
}

#[test]
fn unknown_when_not_a_git_repo() {
    let dir = TempDir::new().unwrap();
    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Unknown);
    // Unknown means "can't tell", not "known stale" — a non-git project is a
    // supported use case and shouldn't get a permanent bogus warning.
    assert!(fresh.warning_line().is_none());
}

#[test]
fn unknown_when_never_indexed() {
    let dir = init_repo();
    // No index_meta.json written yet.
    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Unknown);
    assert!(fresh.warning_line().is_none());
}

#[test]
fn fresh_when_head_matches_and_clean() {
    let dir = init_repo();
    write_index_meta(dir.path()).unwrap();
    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Fresh);
    assert!(fresh.warning_line().is_none());
}

#[test]
fn stale_after_new_commit() {
    let dir = init_repo();
    write_index_meta(dir.path()).unwrap();

    std::fs::write(dir.path().join("b.txt"), "world").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "second"]);

    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Stale);
    let warning = fresh.warning_line().unwrap();
    assert!(warning.contains("stale"));
    assert!(warning.contains("branch/commit changed"));
}

#[test]
fn stale_after_branch_switch() {
    let dir = init_repo();
    write_index_meta(dir.path()).unwrap();

    git(dir.path(), &["checkout", "-q", "-b", "other"]);
    std::fs::write(dir.path().join("c.txt"), "branch").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "on other branch"]);

    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Stale);
    assert_ne!(fresh.indexed_head, fresh.current_head);
}

#[test]
fn stale_when_working_tree_dirty() {
    let dir = init_repo();
    write_index_meta(dir.path()).unwrap();

    // HEAD unchanged, but a tracked file has uncommitted edits.
    std::fs::write(dir.path().join("a.txt"), "modified, uncommitted").unwrap();

    let fresh = compute_freshness(dir.path(), 0);
    assert_eq!(fresh.status, FreshnessStatus::Stale);
    assert!(fresh.working_tree_dirty);
    assert_eq!(fresh.indexed_head, fresh.current_head);
    let warning = fresh.warning_line().unwrap();
    assert!(warning.contains("uncommitted changes"));
}

#[test]
fn updating_when_pending_changes_but_head_matches() {
    let dir = init_repo();
    write_index_meta(dir.path()).unwrap();

    let fresh = compute_freshness(dir.path(), 3);
    assert_eq!(fresh.status, FreshnessStatus::Updating);
    let warning = fresh.warning_line().unwrap();
    assert!(warning.contains("3 file(s) pending reindex"));
}
