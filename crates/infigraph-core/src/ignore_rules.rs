//! Shared, .gitignore- and .infigraphignore-aware ignore rules used by
//! every directory walker and the file watcher, so a project convention
//! excluded via .gitignore (or .infigraphignore) is honored everywhere
//! consistently, instead of each call site maintaining its own hardcoded
//! directory-name list.

use std::path::Path;
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

/// Directories always excluded, regardless of what any .gitignore or
/// .infigraphignore says. Union of every previously-independent hardcoded
/// list this module replaces (collect_files, the watcher, doc indexing,
/// grep search, security scanning) -- unifying them must not silently
/// reduce protection in a repo whose own .gitignore happens to be sparse.
pub const IGNORE_SAFETY_LIST: &[&str] = &[
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
    "vendor",
    ".idea",
    ".mypy_cache",
    "coverage",
    ".pytest_cache",
];

fn is_safety_excluded(name: &str) -> bool {
    IGNORE_SAFETY_LIST.contains(&name)
}

/// A pre-configured `WalkBuilder` for `root`: respects `.gitignore`,
/// `.infigraphignore`, and the safety list above. Callers may add further
/// configuration (e.g. `.max_depth`) before calling `.build()`.
pub fn walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);

    // Discover and add all .gitignore and .infigraphignore files
    let mut gi_builder = GitignoreBuilder::new(root);
    let mut discovery = WalkBuilder::new(root);
    discovery
        .hidden(false)
        .filter_entry(|entry| !is_safety_excluded(&entry.file_name().to_string_lossy()));

    for result in discovery.build() {
        let Ok(entry) = result else { continue };
        let name = entry.file_name().to_string_lossy();
        if name == ".gitignore" || name == ".infigraphignore" {
            let _ = gi_builder.add(entry.path());
        }
    }

    let gitignore = Arc::new(gi_builder.build().unwrap_or_else(|_| Gitignore::empty()));

    // Apply gitignore rules via filter_entry
    builder.hidden(true).filter_entry(move |entry| {
        // Check safety list first
        if is_safety_excluded(&entry.file_name().to_string_lossy()) {
            return false;
        }
        // Then check gitignore rules
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        !gitignore.matched(entry.path(), is_dir).is_ignore()
    });
    builder
}

/// Point-wise matcher for a single path (e.g. a file-watcher event), where
/// there's no directory tree to walk. Built from the same safety list and
/// the same `.gitignore`/`.infigraphignore` files `walk_builder` would
/// discover -- rebuild when those files may have changed (the watcher
/// rebuilds this on its periodic tick; see `watch_project_with_periodic`).
pub struct IgnoreMatcher {
    #[allow(dead_code)]
    root: std::path::PathBuf,
    gitignore: Gitignore,
}

impl IgnoreMatcher {
    /// Discovers every `.gitignore`/`.infigraphignore` under `root`
    /// (skipping the safety list, same as `walk_builder`, so this never
    /// wastes time descending into e.g. `node_modules/` hunting for nested
    /// ignore files there -- nothing inside is ever relevant since the
    /// whole directory is always excluded), then builds one matcher from
    /// all of them. `.hidden(false)` here (unlike `walk_builder`) because
    /// the ignore files themselves are dot-prefixed and must be visited as
    /// walk results to be found; `.git_ignore(true)` still prunes any
    /// subtree an already-discovered ancestor `.gitignore` excludes, so
    /// this stays proportional to directory count, not full file count.
    pub fn build(root: &Path) -> Self {
        let root = root.to_path_buf();
        let mut gi_builder = GitignoreBuilder::new(&root);

        let mut discovery = WalkBuilder::new(&root);
        discovery
            .hidden(false)
            .filter_entry(|entry| !is_safety_excluded(&entry.file_name().to_string_lossy()));

        for result in discovery.build() {
            let Ok(entry) = result else { continue };
            let name = entry.file_name().to_string_lossy();
            if name == ".gitignore" || name == ".infigraphignore" {
                let _ = gi_builder.add(entry.path());
            }
        }

        let gitignore = gi_builder.build().unwrap_or_else(|_| Gitignore::empty());
        IgnoreMatcher { root, gitignore }
    }

    /// True if `path` should be excluded -- either via the safety list
    /// (checked against every path component, so a nested occurrence like
    /// `foo/node_modules/bar` is still caught) or via a discovered
    /// `.gitignore`/`.infigraphignore` rule.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        if path
            .components()
            .any(|c| is_safety_excluded(&c.as_os_str().to_string_lossy()))
        {
            return true;
        }
        // The Gitignore was built for self.root, and matched() should handle
        // paths that are within the root, whether absolute or relative
        self.gitignore.matched(path, is_dir).is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "scratchpad/\n*.log\n").unwrap();
        fs::create_dir_all(dir.path().join("scratchpad/wt-foo")).unwrap();
        fs::write(dir.path().join("scratchpad/wt-foo/README.md"), "# copy").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("debug.log"), "noisy").unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), "//").unwrap();
        dir
    }

    #[test]
    fn walk_builder_skips_gitignored_scratchpad_and_safety_list() {
        let dir = make_fixture();
        let mut found = Vec::new();
        for result in walk_builder(dir.path()).build() {
            let entry = result.unwrap();
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                found.push(entry.path().to_path_buf());
            }
        }
        assert!(found.iter().any(|p| p.ends_with("src/main.rs")));
        assert!(
            !found
                .iter()
                .any(|p| p.to_string_lossy().contains("scratchpad")),
            "scratchpad/ is gitignored and must not be walked: {found:?}"
        );
        assert!(
            !found
                .iter()
                .any(|p| p.to_string_lossy().contains("node_modules")),
            "node_modules/ is in the safety list and must not be walked: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.ends_with("debug.log")),
            "*.log is gitignored and must not be walked: {found:?}"
        );
    }

    #[test]
    fn ignore_matcher_agrees_with_walk_builder() {
        let dir = make_fixture();
        let matcher = IgnoreMatcher::build(dir.path());

        assert!(!matcher.is_ignored(&dir.path().join("src/main.rs"), false));
        assert!(matcher.is_ignored(&dir.path().join("scratchpad/wt-foo/README.md"), false));
        assert!(matcher.is_ignored(&dir.path().join("scratchpad"), true));
        assert!(matcher.is_ignored(&dir.path().join("debug.log"), false));
        assert!(matcher.is_ignored(&dir.path().join("node_modules/pkg/index.js"), false));
    }

    #[test]
    fn infigraphignore_is_honored_like_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".infigraphignore"), "vendored/\n").unwrap();
        fs::create_dir_all(dir.path().join("vendored")).unwrap();
        fs::write(dir.path().join("vendored/lib.rs"), "// vendored").unwrap();
        fs::write(dir.path().join("real.rs"), "fn f() {}").unwrap();

        let matcher = IgnoreMatcher::build(dir.path());
        assert!(matcher.is_ignored(&dir.path().join("vendored/lib.rs"), false));
        assert!(!matcher.is_ignored(&dir.path().join("real.rs"), false));

        let mut found = Vec::new();
        for result in walk_builder(dir.path()).build() {
            let entry = result.unwrap();
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                found.push(entry.path().to_path_buf());
            }
        }
        assert!(!found
            .iter()
            .any(|p| p.to_string_lossy().contains("vendored")));
        assert!(found.iter().any(|p| p.ends_with("real.rs")));
    }
}
