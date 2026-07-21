use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;
use infigraph_mcp::tools::analysis::git::parse_diff_hunks;

pub(crate) fn cmd_git_summary(
    root: &Path,
    n_commits: usize,
    author: Option<&str>,
    file: Option<&str>,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    // Get recent commit hashes + metadata
    let n_commits_arg = format!("-{}", n_commits);
    let mut log_cmd_args: Vec<String> = vec![
        "log".to_string(),
        "--format=%H\x1f%an\x1f%ae\x1f%ai\x1f%s".to_string(),
        n_commits_arg,
    ];
    if let Some(author) = author {
        log_cmd_args.push(format!("--author={}", author));
    }
    if let Some(file) = file {
        log_cmd_args.push("--".to_string());
        log_cmd_args.push(file.to_string());
    }

    let log_out = std::process::Command::new("git")
        .args(&log_cmd_args)
        .current_dir(root)
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
        println!("No commits found.");
        return Ok(());
    }

    println!("Git Summary — last {} commits\n", commits.len());

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
        if let Some(file) = file {
            diff_cmd_args.push("--".to_string());
            diff_cmd_args.push(file.to_string());
        }

        let diff_out = std::process::Command::new("git")
            .args(&diff_cmd_args)
            .current_dir(root)
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
        let mut touched: HashSet<String> = HashSet::new();
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

        // Name-only list for changed files
        let parent_ref2 = format!("{}^", hash);
        let files_out = std::process::Command::new("git")
            .args(["diff", "--name-only", &parent_ref2, hash])
            .current_dir(root)
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

        println!("━━ {} {} — {} — {}", short, date_short, author, subject);
        println!("   Files changed: {}", changed_files.len());
        for f in &changed_files {
            println!("     {}", f);
        }
        if !touched.is_empty() {
            let mut sorted: Vec<_> = touched.iter().collect();
            sorted.sort();
            println!("   Symbols touched ({}):", sorted.len());
            for s in sorted {
                println!("     + {}", s);
            }
        } else if !changed_files.is_empty() {
            println!("   Symbols touched: none indexed in changed lines");
        }
        println!();
    }

    Ok(())
}
