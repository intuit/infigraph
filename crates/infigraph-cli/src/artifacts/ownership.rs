//! Tracks the content hash of files written by the `overwrite` strategy so
//! uninstall can tell an untouched managed file from one the user hand-edited
//! after install, and preserve the latter instead of silently deleting it.
//!
//! The other four strategies don't need this: `json_deep_merge`,
//! `json_key_path`, and `toml_section` remove only the specific key/section
//! they own, and `marker_delimited` removes only the content between its own
//! markers -- a user's edits elsewhere in the same file are untouched by
//! construction. `overwrite` has no such boundary; it owns the whole file,
//! so it's the one strategy where "did the user touch this since install"
//! has to be checked explicitly before deleting.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

fn manifest_path(home: &Path) -> std::path::PathBuf {
    home.join(".infigraph").join("installed-files.json")
}

fn load_manifest(home: &Path) -> Result<BTreeMap<String, String>> {
    let path = manifest_path(home);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(BTreeMap::new());
    };
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_manifest(home: &Path, manifest: &BTreeMap<String, String>) -> Result<()> {
    let path = manifest_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn hash_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

/// Records the hash of content just written to `target_path`, keyed by its
/// absolute path, so a later uninstall can verify it's unchanged.
pub(crate) fn record_written(home: &Path, target_path: &Path, content: &[u8]) -> Result<()> {
    let mut manifest = load_manifest(home)?;
    manifest.insert(
        target_path.to_string_lossy().into_owned(),
        hash_hex(content),
    );
    save_manifest(home, &manifest)
}

/// Returns true if `target_path` is safe to delete on uninstall: either it
/// was never recorded (an install that predates this tracking, or content
/// written outside the artifact engine), or its current on-disk content
/// still matches what was written at install time. Clears the manifest
/// entry either way, so a later reinstall starts fresh.
pub(crate) fn verify_unchanged_and_clear(home: &Path, target_path: &Path) -> Result<bool> {
    let mut manifest = load_manifest(home)?;
    let key = target_path.to_string_lossy().into_owned();
    let Some(recorded_hash) = manifest.remove(&key) else {
        return Ok(true);
    };
    save_manifest(home, &manifest)?;

    match std::fs::read(target_path) {
        Ok(on_disk) => Ok(hash_hex(&on_disk) == recorded_hash),
        Err(_) => Ok(true), // already gone -- nothing to preserve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecorded_path_is_treated_as_safe_to_delete() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("some/file.sh");
        assert!(verify_unchanged_and_clear(home.path(), &target).unwrap());
    }

    #[test]
    fn unchanged_content_is_safe_to_delete() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("hooks/script.sh");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"original").unwrap();
        record_written(home.path(), &target, b"original").unwrap();

        assert!(verify_unchanged_and_clear(home.path(), &target).unwrap());
    }

    #[test]
    fn modified_content_is_not_safe_to_delete() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("hooks/script.sh");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"original").unwrap();
        record_written(home.path(), &target, b"original").unwrap();

        std::fs::write(&target, b"user edited this").unwrap();

        assert!(!verify_unchanged_and_clear(home.path(), &target).unwrap());
    }

    #[test]
    fn verify_clears_the_manifest_entry_regardless_of_verdict() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("hooks/script.sh");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"original").unwrap();
        record_written(home.path(), &target, b"original").unwrap();

        verify_unchanged_and_clear(home.path(), &target).unwrap();

        assert!(load_manifest(home.path()).unwrap().is_empty());
    }
}
