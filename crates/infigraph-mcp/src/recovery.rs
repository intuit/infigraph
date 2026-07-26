//! Crash / corrupt-index recovery helpers (code graph + document store).

use std::path::{Path, PathBuf};

/// Collect the set of project roots to reindex after a crash.
///
/// The MCP server may be launched in a repo that was never registered in
/// `~/.infigraph/registry.json` (standalone use is the common case), so the
/// registry alone is not enough: with an empty registry the old recovery was
/// a no-op and the crashed repo stayed broken. The supervisor's startup
/// directory is therefore always considered a candidate.
///
/// Only paths that actually contain a `.infigraph/` directory are returned,
/// deduplicated by canonical path so a registered startup dir isn't indexed
/// twice.
pub fn collect_reindex_targets(
    startup_dir: Option<&Path>,
    registry_paths: &[PathBuf],
    groups_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::new();

    let mut push = |path: &Path| {
        if !path.join(".infigraph").exists() {
            return;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if seen.insert(key) {
            targets.push(path.to_path_buf());
        }
    };

    // The repo the MCP server was actually serving comes first.
    if let Some(dir) = startup_dir {
        push(dir);
    }
    for path in registry_paths {
        push(path);
    }
    if let Some(gd) = groups_dir {
        if let Ok(entries) = std::fs::read_dir(gd) {
            for entry in entries.flatten() {
                push(&entry.path());
            }
        }
    }

    targets
}

/// Wipe code graph and document index artifacts under `root/.infigraph/`.
/// Used by SIGSEGV auto-reindex so both stores are rebuilt by `infigraph index`.
pub fn wipe_code_and_docs(root: &Path) {
    let ig = root.join(".infigraph");
    if !ig.exists() {
        return;
    }

    let graph_path = ig.join("graph");
    if graph_path.exists() {
        let _ = std::fs::remove_file(&graph_path);
        let _ = std::fs::remove_dir_all(&graph_path);
    }
    let _ = std::fs::remove_file(ig.join("graph.wal"));

    if let Ok(mut idx) = infigraph_docs::DocIndex::open(root) {
        let _ = idx.clean();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_wipe_code_and_docs_removes_graph_and_docs() {
        let dir = tempfile::tempdir().unwrap();
        let ig = dir.path().join(".infigraph");
        fs::create_dir_all(&ig).unwrap();
        fs::write(ig.join("graph"), b"fake-graph").unwrap();
        fs::write(ig.join("graph.wal"), b"wal").unwrap();
        fs::write(ig.join("docs.kuzu"), b"fake-docs").unwrap();
        fs::write(ig.join("docs_embeddings.bin"), b"emb").unwrap();
        fs::write(ig.join("docs_hnsw_index.usearch"), b"hnsw").unwrap();
        fs::write(ig.join("docs_hnsw_index.meta"), b"meta").unwrap();
        // sessions must survive
        fs::write(ig.join("sessions_keep.txt"), b"keep").unwrap();

        wipe_code_and_docs(dir.path());

        assert!(!ig.join("graph").exists());
        assert!(!ig.join("graph.wal").exists());
        assert!(!ig.join("docs.kuzu").exists());
        assert!(!ig.join("docs_embeddings.bin").exists());
        assert!(!ig.join("docs_hnsw_index.usearch").exists());
        assert!(!ig.join("docs_hnsw_index.meta").exists());
        assert!(
            ig.join("sessions_keep.txt").exists(),
            "non-index files under .infigraph must not be wiped"
        );
    }

    #[test]
    fn test_wipe_code_and_docs_missing_infigraph_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        wipe_code_and_docs(dir.path()); // must not panic
    }

    /// Regression test: recovery used to iterate only registry repos, so with
    /// an empty registry (the standalone default) it recovered nothing — the
    /// crashed repo the MCP server was actually serving stayed broken.
    #[test]
    fn collect_targets_includes_startup_dir_with_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".infigraph")).unwrap();

        let targets = collect_reindex_targets(Some(dir.path()), &[], None);
        assert_eq!(targets, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn collect_targets_skips_dirs_without_infigraph() {
        let dir = tempfile::tempdir().unwrap(); // no .infigraph inside
        let targets = collect_reindex_targets(Some(dir.path()), &[], None);
        assert!(targets.is_empty());
    }

    #[test]
    fn collect_targets_dedups_startup_dir_against_registry() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".infigraph")).unwrap();

        let registry = vec![dir.path().to_path_buf()];
        let targets = collect_reindex_targets(Some(dir.path()), &registry, None);
        assert_eq!(
            targets.len(),
            1,
            "same repo via startup dir and registry must be indexed once"
        );
    }

    #[test]
    fn collect_targets_includes_registry_repos_and_groups() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".infigraph")).unwrap();

        let groups = tempfile::tempdir().unwrap();
        let group = groups.path().join("my-group");
        fs::create_dir_all(group.join(".infigraph")).unwrap();

        let registry = vec![repo.path().to_path_buf()];
        let targets = collect_reindex_targets(None, &registry, Some(groups.path()));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&repo.path().to_path_buf()));
        assert!(targets.contains(&group));
    }
}
