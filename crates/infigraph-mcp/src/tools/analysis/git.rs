use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_languages::bundled_registry;

use super::super::helpers::open_prism;

pub fn tool_detect_changes(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let base = args.get("base").and_then(|b| b.as_str()).unwrap_or("HEAD");
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(3) as u32;

    let backend = prism.backend().context("not initialized")?;

    build_detect_changes_report(prism.root(), backend, base, depth)
}

pub fn build_detect_changes_report(
    project_root: &std::path::Path,
    backend: &dyn infigraph_core::graph::GraphBackend,
    base: &str,
    depth: u32,
) -> Result<String> {
    use std::collections::HashSet;

    // 1. Get changed files
    let name_output = std::process::Command::new("git")
        .args(["diff", "--name-only", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --name-only")?;

    if !name_output.status.success() {
        let stderr = String::from_utf8_lossy(&name_output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    let changed_files: Vec<String> = String::from_utf8_lossy(&name_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if changed_files.is_empty() {
        return Ok("No changes detected.".to_string());
    }

    // 2. Get unified diff with zero context to extract changed line ranges
    let diff_output = std::process::Command::new("git")
        .args(["diff", "--unified=0", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --unified=0")?;

    let diff_text = String::from_utf8_lossy(&diff_output.stdout);
    let hunks = parse_diff_hunks(&diff_text);

    // 3. For each changed file+range, find overlapping symbols
    let mut directly_changed: Vec<(String, String, String, u32, u32)> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (file, start, end) in &hunks {
        let symbols = backend.symbols_in_range(file, *start, *end)?;
        for s in symbols {
            if seen_ids.insert(s.id.clone()) {
                directly_changed.push((s.id, s.name, s.file, s.start_line, s.end_line));
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!("=== Change Detection (base: {}) ===\n\n", base));
    out.push_str(&format!("Changed files: {}\n", changed_files.len()));
    for f in &changed_files {
        out.push_str(&format!("  {}\n", f));
    }

    out.push_str(&format!(
        "\n=== Directly Changed Symbols ({}) ===\n",
        directly_changed.len()
    ));
    if directly_changed.is_empty() {
        out.push_str("  (no indexed symbols overlap with changed lines)\n");
    } else {
        for (_, name, file, start, end) in &directly_changed {
            out.push_str(&format!("  {:30} {} L{}-{}\n", name, file, start, end));
        }
    }

    // 4. Compute blast radius
    if !directly_changed.is_empty() && depth > 0 {
        let mut indirectly_affected: Vec<(String, String, String, String)> = Vec::new();
        let mut indirect_ids: HashSet<String> = HashSet::new();

        for (id, _, _, _, _) in &directly_changed {
            if let Ok(impacted) = backend.transitive_impact(id, depth) {
                for row in impacted {
                    if !seen_ids.contains(&row.id) && indirect_ids.insert(row.id.clone()) {
                        indirectly_affected.push((row.id, row.name, row.file, row.kind));
                    }
                }
            }
        }

        out.push_str(&format!(
            "\n=== Blast Radius (depth={}, {} indirectly affected) ===\n",
            depth,
            indirectly_affected.len()
        ));
        if indirectly_affected.is_empty() {
            out.push_str("  (no additional symbols affected)\n");
        } else {
            for (_, name, file, kind) in &indirectly_affected {
                out.push_str(&format!("  {:>8} {:30} {}\n", kind, name, file));
            }
        }
    }

    Ok(out)
}

/// Parse unified diff output (with --unified=0) to extract (file, start_line, end_line) hunks.
pub fn parse_diff_hunks(diff: &str) -> Vec<(String, u32, u32)> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.to_string();
            continue;
        }

        if line.starts_with("@@") && !current_file.is_empty() {
            if let Some(plus_part) = line.split('+').nth(1) {
                let range_part = plus_part.split(' ').next().unwrap_or("");
                let parts: Vec<&str> = range_part.split(',').collect();
                let start: u32 = parts[0].parse().unwrap_or(0);
                let count: u32 = if parts.len() > 1 {
                    parts[1].parse().unwrap_or(1)
                } else {
                    1
                };
                if start > 0 {
                    let end = if count == 0 { start } else { start + count - 1 };
                    hunks.push((current_file.clone(), start, end));
                }
            }
        }
    }

    hunks
}

pub fn tool_semantic_diff(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let old_ref = args
        .get("old_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let new_ref = args
        .get("new_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;
    let registry = bundled_registry()?;
    let diff = infigraph_core::diff::semantic_diff(&root, old_ref, new_ref, &registry)?;
    Ok(infigraph_core::diff::format_diff(&diff))
}

pub fn tool_git_summary(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let n_commits = args.get("n_commits").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let author_filter = args.get("author").and_then(|v| v.as_str());
    let file_filter = args.get("file").and_then(|v| v.as_str());

    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    // Get recent commit hashes + metadata
    let n_commits_arg = format!("-{}", n_commits);
    let mut log_cmd_args: Vec<String> = vec![
        "log".to_string(),
        "--format=%H\x1f%an\x1f%ae\x1f%ai\x1f%s".to_string(),
        n_commits_arg,
    ];
    if let Some(author) = author_filter {
        log_cmd_args.push(format!("--author={}", author));
    }
    if let Some(file) = file_filter {
        log_cmd_args.push("--".to_string());
        log_cmd_args.push(file.to_string());
    }

    let log_out = std::process::Command::new("git")
        .args(&log_cmd_args)
        .current_dir(&root)
        .output()
        .context("failed to run git log")?;

    if !log_out.status.success() {
        let stderr = String::from_utf8_lossy(&log_out.stderr);
        anyhow::bail!("git log failed: {}", stderr.trim());
    }

    let log_text = String::from_utf8_lossy(&log_out.stdout);
    let commits: Vec<(&str, &str, &str, &str, &str)> = log_text
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(5, '\x1f').collect();
            if parts.len() == 5 {
                Some((parts[0], parts[1], parts[2], parts[3], parts[4]))
            } else {
                None
            }
        })
        .collect();

    if commits.is_empty() {
        return Ok("No commits found.".to_string());
    }

    let mut out = format!("Git Summary — last {} commits\n\n", commits.len());

    for (hash, author, _email, date, subject) in &commits {
        let short = &hash[..8.min(hash.len())];

        // Get files changed in this commit
        let parent_ref = format!("{}^", hash);
        let mut diff_cmd_args: Vec<String> = vec![
            "diff".to_string(),
            "--unified=0".to_string(),
            parent_ref,
            hash.to_string(),
        ];
        if let Some(file) = file_filter {
            diff_cmd_args.push("--".to_string());
            diff_cmd_args.push(file.to_string());
        }

        let diff_out = std::process::Command::new("git")
            .args(&diff_cmd_args)
            .current_dir(&root)
            .output();

        let diff_text_owned;
        let hunks = match diff_out {
            Ok(o) if o.status.success() => {
                diff_text_owned = String::from_utf8_lossy(&o.stdout).to_string();
                parse_diff_hunks(&diff_text_owned)
            }
            _ => vec![],
        };

        // Collect touched symbols
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (file, start, end) in &hunks {
            if let Ok(syms) = backend.symbols_in_range(file, *start, *end) {
                for s in syms {
                    touched.insert(format!(
                        "{} {} ({}:{})",
                        s.kind, s.name, s.file, s.start_line
                    ));
                }
            }
        }

        // Name-only list for files that had changes but no indexed symbols
        let parent_ref2 = format!("{}^", hash);
        let files_out = std::process::Command::new("git")
            .args(["diff", "--name-only", &parent_ref2, hash])
            .current_dir(&root)
            .output();
        let changed_files: Vec<String> = match files_out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            Err(_) => vec![],
        };

        // Date: just the date part (drop time zone)
        let date_short = date.get(..10).unwrap_or(date);

        out.push_str(&format!(
            "━━ {} {} — {} — {}\n",
            short, date_short, author, subject
        ));
        out.push_str(&format!("   Files changed: {}\n", changed_files.len()));
        for f in &changed_files {
            out.push_str(&format!("     {}\n", f));
        }
        if !touched.is_empty() {
            let mut sorted: Vec<_> = touched.iter().collect();
            sorted.sort();
            out.push_str(&format!("   Symbols touched ({}):\n", sorted.len()));
            for s in sorted {
                out.push_str(&format!("     + {}\n", s));
            }
        } else if !changed_files.is_empty() {
            out.push_str("   Symbols touched: none indexed in changed lines\n");
        }
        out.push('\n');
    }

    Ok(out)
}
