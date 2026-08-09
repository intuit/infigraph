use std::path::Path;

use kuzu::Connection;

use crate::graph::parquet_loader;

/// How many bad-record-drop-and-retry cycles a bulk COPY gets before giving
/// up and falling back to the slow per-row UNWIND path for whatever remains.
/// Bounded so a batch with many genuinely-bad rows still terminates instead
/// of looping indefinitely.
const MAX_BAD_RECORD_RETRIES: usize = 20;

/// Absolute floor of free space required before attempting any bulk write,
/// regardless of the projected write size -- Kuzu needs headroom for WAL and
/// temp buffers even for a small COPY.
const MIN_DISK_HEADROOM_BYTES: u64 = 200 * 1024 * 1024;

/// How many bytes of free space to require per byte of projected write size
/// (source data read, or an existing on-disk estimate). Conservative
/// multiplier for WAL/temp-buffer write amplification, not a precise model.
const DISK_HEADROOM_FACTOR: u64 = 3;

/// Check that the volume holding `dir` has enough free space to safely
/// attempt a write of roughly `projected_write_bytes`. Returns `Err` with a
/// human-readable shortfall description if not -- callers should abort the
/// write entirely rather than let Kuzu run out of disk mid-transaction,
/// which crashes the whole process with an uncaught C++ exception instead of
/// surfacing a `Result` (observed on sittir: SCIP enrichment's COPY ran the
/// volume out of space and crashed with `TransactionManagerException`).
///
/// If free space can't be determined (e.g. `dir` doesn't exist, or the
/// platform call fails), the check passes -- an unrelated I/O error here
/// shouldn't block a write; let the write itself surface any real problem.
pub(crate) fn check_disk_headroom(dir: &Path, projected_write_bytes: u64) -> Result<(), String> {
    let needed = projected_write_bytes
        .saturating_mul(DISK_HEADROOM_FACTOR)
        .max(MIN_DISK_HEADROOM_BYTES);
    match fs2::available_space(dir) {
        Ok(avail) if avail >= needed => Ok(()),
        Ok(avail) => Err(format!(
            "only {} MB free on the volume holding {}, need ~{} MB headroom for a ~{} MB write",
            avail / (1024 * 1024),
            dir.display(),
            needed / (1024 * 1024),
            projected_write_bytes / (1024 * 1024),
        )),
        Err(_) => Ok(()),
    }
}

/// Escape single quotes and control characters for Kuzu string literals.
pub(crate) fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', " ")
        .replace('\r', "")
        .replace('\t', " ")
}

/// Convert a path to forward-slash form (needed on Windows for Kuzu COPY FROM).
pub(crate) fn fwd_slash_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Lookup stem for a Module file id (a path): final path segment with its file
/// extension stripped, lower-cased. "pkg/helpers.py" -> "helpers".
pub(crate) fn file_stem(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    stem.to_lowercase()
}

/// Lookup stem for an import target module name: the last dotted/slashed
/// segment, lower-cased. Python "src.lib" -> "lib"; "pkg/mod" -> "mod";
/// bare "helpers" -> "helpers". Matches `file_stem` so an import resolves to
/// the imported file's Module node (whose stem is computed via `file_stem`).
pub(crate) fn import_stem(module_name: &str) -> String {
    module_name
        .rsplit(['.', '/', '\\'])
        .next()
        .unwrap_or(module_name)
        .to_lowercase()
}

/// Resolve an import's bare module name to a Module file path when the
/// basename stem has more than one candidate (e.g. both `app/service/constants.py`
/// and `app/test/e2e/constants.py` exist). Picks the candidate whose path,
/// normalized to `/`-separated lowercase segments, ends with `module_name`'s
/// own dotted/slashed segments -- so `app.service.constants` matches
/// `app/service/constants.py` but not `app/test/e2e/constants.py`. Returns
/// `None` (skip the edge) rather than guessing when no candidate matches or
/// more than one does -- a wrong IMPORTS edge is worse than a missing one.
pub(crate) fn resolve_import_candidate<'a>(
    module_name: &str,
    candidates: &[&'a str],
) -> Option<&'a str> {
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    let wanted: Vec<String> = module_name
        .split(['.', '/', '\\'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    if wanted.len() < 2 {
        // Bare single-segment import name (e.g. "constants") carries no
        // disambiguating path info -- can't tell candidates apart.
        return None;
    }
    let mut matches = candidates.iter().filter(|file| {
        // Strip the extension from the basename only (matches `file_stem`),
        // not the whole path -- a dot in a directory name (e.g. "app/v1.2/x.py")
        // must not be mistaken for the extension separator.
        let (dir, base) = file.rsplit_once(['/', '\\']).unwrap_or(("", file));
        let base_no_ext = base.rsplit_once('.').map(|(b, _)| b).unwrap_or(base);
        let path_no_ext = if dir.is_empty() {
            base_no_ext.to_string()
        } else {
            format!("{dir}/{base_no_ext}")
        };
        let segments: Vec<String> = path_no_ext
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();
        segments.ends_with(&wanted)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}

/// Batch-insert edges via UNWIND in chunks of 500.
pub(crate) fn unwind_edges_from_pairs(
    conn: &Connection,
    pairs: &[(&str, &str)],
    rel_type: &str,
    src_label: &str,
    dst_label: &str,
) {
    const CHUNK: usize = 500;
    for chunk in pairs.chunks(CHUNK) {
        let pair_list: Vec<String> = chunk
            .iter()
            .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
            .collect();
        let _ = conn.query(&format!(
            "UNWIND [{}] AS p MATCH (a:{src_label}), (b:{dst_label}) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{rel_type}]->(b)",
            pair_list.join(", ")
        ));
    }
}

/// Extract the offending value from a Kuzu COPY-failure error message, if it
/// looks like a bad-primary-key error recoverable by dropping just that
/// record and retrying -- a duplicate id within the batch (or one that
/// already exists in the graph), or an edge endpoint id that doesn't match
/// any existing row. Returns `None` for errors that don't name a specific
/// value, so callers know to fall back rather than retry blindly.
pub(crate) fn extract_bad_copy_value(err: &str) -> Option<&str> {
    for marker in [
        "duplicated primary key value ",
        "Unable to find primary key value ",
    ] {
        if let Some(idx) = err.find(marker) {
            let rest = &err[idx + marker.len()..];
            let end = rest.find(", which").unwrap_or(rest.len());
            let val = rest[..end].trim_end_matches('.').trim();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Bulk-COPY an edge table from `pairs`, retrying with the offending
/// endpoint's pairs dropped whenever the failure is a recoverable
/// bad-primary-key error (a duplicate edge, or an endpoint id that doesn't
/// exist in `src_label`/`dst_label`). Falls back to per-row UNWIND only once
/// retries are exhausted (`MAX_BAD_RECORD_RETRIES`) or the error isn't
/// recognized as recoverable -- avoids paying the slow UNWIND path for an
/// entire batch over a handful of bad rows.
pub(crate) fn copy_edges_with_bad_record_retry(
    conn: &Connection,
    table: &str,
    mut pairs: Vec<(String, String)>,
    src_label: &str,
    dst_label: &str,
    edge_pq: &Path,
) {
    for attempt in 0..MAX_BAD_RECORD_RETRIES {
        if pairs.is_empty() {
            return;
        }
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        if parquet_loader::write_edge_parquet(edge_pq, &refs).is_err() {
            break;
        }
        match conn.query(&format!("COPY {table} FROM '{}'", fwd_slash_path(edge_pq))) {
            Ok(_) => {
                let _ = std::fs::remove_file(edge_pq);
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(bad) = extract_bad_copy_value(&msg) {
                    let before = pairs.len();
                    pairs.retain(|(a, b)| a != bad && b != bad);
                    if pairs.len() < before {
                        eprintln!(
                            "warn: COPY {table} dropped {} bad-PK record(s) (attempt {}/{MAX_BAD_RECORD_RETRIES}), retrying",
                            before - pairs.len(),
                            attempt + 1
                        );
                        continue;
                    }
                }
                eprintln!("warn: COPY {table} via parquet failed ({e}), falling back to UNWIND");
                break;
            }
        }
    }
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    unwind_edges_from_pairs(conn, &refs, table, src_label, dst_label);
    let _ = std::fs::remove_file(edge_pq);
}

pub fn classify_file(file: &str) -> &'static str {
    let fl = file.to_ascii_lowercase();
    if fl.ends_with("-lock.yaml")
        || fl.ends_with(".lock")
        || fl.contains("pnpm-lock")
        || fl.contains("package-lock")
        || fl.contains("yarn.lock")
    {
        return "config";
    }
    if fl.ends_with(".md") || fl.contains("/docs/") || fl.contains("/doc/") {
        return "docs";
    }
    if fl.ends_with(".yaml") || fl.ends_with(".yml") || fl.ends_with(".json") {
        if fl.contains("test")
            || fl.contains("eval")
            || fl.contains("golden")
            || fl.contains("dataset")
            || fl.contains("fixture")
        {
            return "test";
        }
        return "config";
    }
    if fl.contains("/test/")
        || fl.contains("/tests/")
        || fl.contains("/__tests__/")
        || fl.contains("/__mocks__/")
        || fl.starts_with("test_")
        || fl.contains("/test_")
        || fl.contains(".test.")
        || fl.contains(".spec.")
        || fl.contains("/e2e/")
        || fl.starts_with("e2e/")
        || fl.contains("/fixtures/")
        || fl.starts_with("fixtures/")
        || fl.contains("/testdata/")
        || fl.starts_with("testdata/")
        || fl.starts_with("__tests__/")
        || fl.starts_with("__mocks__/")
    {
        return "test";
    }
    "impl"
}

#[cfg(test)]
mod tests {
    use super::{
        check_disk_headroom, classify_file, extract_bad_copy_value, resolve_import_candidate,
    };

    #[test]
    fn disk_headroom_passes_for_tiny_projected_write_on_real_dir() {
        // Real syscall against a real directory -- any dev/CI machine has
        // far more than 200MB free on the temp volume, and a 1-byte
        // projected write floors to the minimum headroom, not scaled up.
        assert!(check_disk_headroom(&std::env::temp_dir(), 1).is_ok());
    }

    #[test]
    fn disk_headroom_does_not_block_on_unreadable_path() {
        // Can't determine free space for a path that doesn't exist -- must
        // not block the write over an unrelated lookup failure.
        let bogus = std::path::Path::new("/does/not/exist/anywhere/infigraph-test");
        assert!(check_disk_headroom(bogus, 1024 * 1024 * 1024 * 1024).is_ok());
    }

    #[test]
    fn extracts_duplicated_primary_key_value() {
        // Real Kuzu COPY-failure text observed on sittir's SCIP enrichment.
        let err = "Found duplicated primary key value packages/codegen/src/compiler/link.ts::\"'\"0, which violates the uniqueness constraint of the primary key column.";
        assert_eq!(
            extract_bad_copy_value(err),
            Some("packages/codegen/src/compiler/link.ts::\"'\"0")
        );
    }

    #[test]
    fn extracts_unable_to_find_primary_key_value() {
        let err = "Unable to find primary key value packages/codegen/src/compiler/collect-slots.ts::scip-typescript npm @sittir/codegen 0.1.0 src/compiler/`collect-slots.ts`/findNestedSeparator().(rule).";
        assert_eq!(
            extract_bad_copy_value(err),
            Some(
                "packages/codegen/src/compiler/collect-slots.ts::scip-typescript npm @sittir/codegen 0.1.0 src/compiler/`collect-slots.ts`/findNestedSeparator().(rule)"
            )
        );
    }

    #[test]
    fn returns_none_for_unrecognized_errors() {
        assert_eq!(
            extract_bad_copy_value("Invalid transaction type to rollback."),
            None
        );
        assert_eq!(extract_bad_copy_value("No space left on device"), None);
    }

    #[test]
    fn impl_files() {
        assert_eq!(classify_file("src/main.rs"), "impl");
        assert_eq!(classify_file("lib/parser.py"), "impl");
        assert_eq!(classify_file("cmd/server/handler.go"), "impl");
    }

    #[test]
    fn test_files() {
        assert_eq!(classify_file("src/tests/unit.rs"), "test");
        assert_eq!(classify_file("test_parser.py"), "test");
        assert_eq!(classify_file("src/__tests__/App.test.tsx"), "test");
        assert_eq!(classify_file("src/handler.spec.ts"), "test");
        assert_eq!(classify_file("e2e/login.test.js"), "test");
        assert_eq!(classify_file("src/__mocks__/db.ts"), "test");
        assert_eq!(classify_file("testdata/input.go"), "test");
        assert_eq!(classify_file("src/fixtures/sample.py"), "test");
    }

    #[test]
    fn test_yaml_json_files() {
        assert_eq!(classify_file("tests/golden/expected.json"), "test");
        assert_eq!(classify_file("eval/dataset.yaml"), "test");
        assert_eq!(classify_file("fixtures/data.yml"), "test");
        assert_eq!(classify_file("config/settings.yaml"), "config");
        assert_eq!(classify_file("package.json"), "config");
    }

    #[test]
    fn config_files() {
        assert_eq!(classify_file("Cargo.lock"), "config");
        assert_eq!(classify_file("pnpm-lock.yaml"), "config");
        assert_eq!(classify_file("package-lock.json"), "config");
        assert_eq!(classify_file("yarn.lock"), "config");
        assert_eq!(classify_file("docker-compose.yml"), "config");
    }

    #[test]
    fn docs_files() {
        assert_eq!(classify_file("README.md"), "docs");
        assert_eq!(classify_file("docs/api.md"), "docs");
        assert_eq!(classify_file("doc/architecture.md"), "docs");
    }

    #[test]
    fn resolve_import_candidate_single_candidate_always_resolves() {
        assert_eq!(
            resolve_import_candidate("constants", &["app/service/constants.py"]),
            Some("app/service/constants.py")
        );
    }

    #[test]
    fn resolve_import_candidate_disambiguates_by_dotted_path() {
        let candidates = ["app/service/constants.py", "app/test/e2e/constants.py"];
        assert_eq!(
            resolve_import_candidate("app.service.constants", &candidates),
            Some("app/service/constants.py")
        );
        assert_eq!(
            resolve_import_candidate("app.test.e2e.constants", &candidates),
            Some("app/test/e2e/constants.py")
        );
    }

    #[test]
    fn resolve_import_candidate_skips_ambiguous_bare_name() {
        let candidates = ["app/service/constants.py", "app/test/e2e/constants.py"];
        assert_eq!(resolve_import_candidate("constants", &candidates), None);
    }

    #[test]
    fn resolve_import_candidate_skips_when_no_suffix_matches() {
        let candidates = ["app/service/constants.py", "app/test/e2e/constants.py"];
        assert_eq!(
            resolve_import_candidate("other.pkg.constants", &candidates),
            None
        );
    }
}
