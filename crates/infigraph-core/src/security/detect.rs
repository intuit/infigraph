use std::path::Path;

use anyhow::Result;

use super::rules::{find_sanitizer_for, Finding, ScanStats, RULES};

/// Scan the project rooted at `root` for security issues.
///
/// Walks all non-vendor files and applies pattern-based rules.
pub fn scan_project(root: &Path) -> Result<ScanStats> {
    let mut stats = ScanStats::default();

    walk_and_scan(root, root, &mut stats)?;
    // Sort findings: Critical first, then High, etc.
    stats.findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    Ok(stats)
}

static IGNORE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "target",
    "build",
    "dist",
    "__pycache__",
    ".tox",
    ".infigraph",
    "vendor",
    ".idea",
    ".mypy_cache",
    "coverage",
    ".pytest_cache",
];

fn walk_and_scan(root: &Path, dir: &Path, stats: &mut ScanStats) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if !IGNORE_DIRS.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_and_scan(root, &path, stats)?;
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                scan_file(&path, &rel, ext, stats)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn scan_file(
    path: &Path,
    rel_path: &str,
    ext: &str,
    stats: &mut ScanStats,
) -> Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // skip binary files
    };

    stats.files_scanned += 1;
    let ext_lower = ext.to_lowercase();
    let all_lines: Vec<&str> = content.lines().collect();

    for (line_idx, line) in all_lines.iter().enumerate() {
        let line_lower = line.to_lowercase();
        let line_no = (line_idx + 1) as u32;

        for rule in RULES {
            if let Some(exts) = rule.extensions {
                if !exts.contains(&ext_lower.as_str()) {
                    continue;
                }
            }

            let matched = if rule.word_boundary {
                super::rules::contains_word(&line_lower, rule.pattern)
            } else {
                line_lower.contains(rule.pattern)
            };
            if !matched {
                continue;
            }

            if let Some(excl) = rule.exclude_if {
                if line_lower.contains(&excl.to_lowercase() as &str) {
                    continue;
                }
            }

            if let Some(required) = rule.require_any {
                if !required
                    .iter()
                    .any(|kw| super::rules::contains_word(&line_lower, kw))
                {
                    continue;
                }
            }

            let col = line_lower.find(rule.pattern).unwrap_or(0) as u32 + 1;
            let category = (rule.category)();

            let sanitizer_hit = find_sanitizer_for(&category, &all_lines, line_idx);
            let suppressed = sanitizer_hit.is_some();

            stats.findings.push(Finding {
                file: rel_path.to_string(),
                line: line_no,
                col,
                severity: rule.severity.clone(),
                category,
                rule_id: rule.id.to_string(),
                message: rule.message.to_string(),
                snippet: line.trim().chars().take(120).collect(),
                suppressed,
                sanitizer_hint: sanitizer_hit,
            });
        }
    }

    Ok(())
}
