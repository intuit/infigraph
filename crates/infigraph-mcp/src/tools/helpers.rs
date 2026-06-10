use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use super::session::{session_date_id, session_epoch};

pub(crate) fn open_prism(args: &Value) -> Result<Infigraph> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&PathBuf::from(path), registry)?;
    prism.init()?;
    Ok(prism)
}

pub(crate) fn find_infigraph_cli() -> Option<std::path::PathBuf> {
    // Check same directory as this binary first
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent()?.join("infigraph");
        if sibling.exists() {
            return Some(sibling);
        }
    }
    // Fall back to PATH
    if let Ok(out) = std::process::Command::new("which")
        .arg("infigraph")
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}

pub(crate) fn find_containing_symbol<'a>(
    intervals: &'a [(&str, usize, usize, &str)],
    file: &str,
    line: usize,
) -> Option<&'a str> {
    intervals.iter().find_map(|(f, start, end, id)| {
        if *f == file && *start <= line && line <= *end {
            Some(*id)
        } else {
            None
        }
    })
}

pub(crate) fn save_analysis(path: &str, tool_name: &str, content: &str) -> Result<String> {
    let root = PathBuf::from(path);
    let dir = root.join(".infigraph").join("sessions").join("analysis");
    std::fs::create_dir_all(&dir)?;

    let date = session_date_id().replace("session_", "");
    let filename = format!("{tool_name}_{date}.md");
    let filepath = dir.join(&filename);
    std::fs::write(&filepath, content)?;

    let lines = content.lines().count();
    let summary: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
    Ok(format!(
        "Saved to {}\n({} lines, {} bytes)\n\n{}",
        filepath.display(),
        lines,
        content.len(),
        summary
    ))
}

pub(crate) fn log_activity(tool_name: &str, args: &Value) {
    if matches!(
        tool_name,
        "get_latest_session"
            | "save_session"
            | "search_sessions"
            | "purge_sessions"
            | "list_projects"
    ) {
        return;
    }
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
    if path.is_empty() {
        return;
    }
    let sessions_dir = PathBuf::from(path).join(".infigraph").join("sessions");
    if std::fs::create_dir_all(&sessions_dir).is_err() {
        return;
    }
    let date = session_date_id().replace("session_", "");
    let log_path = sessions_dir.join(format!("activity_{date}.jsonl"));
    let ts = session_epoch();
    let mut key_args = serde_json::Map::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "path" {
                continue;
            }
            if let Some(s) = v.as_str() {
                let truncated = if s.len() > 120 { &s[..120] } else { s };
                key_args.insert(k.clone(), json!(truncated));
            }
        }
    }
    let entry = json!({"ts": ts, "tool": tool_name, "args": key_args});
    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub(crate) fn glob_matches(glob: &str, path: &str) -> bool {
    // Simple glob: * matches any sequence, ? matches one char
    let gi = glob.chars().peekable();
    let pi = path.chars().peekable();
    glob_match_inner(&gi.collect::<Vec<_>>(), &pi.collect::<Vec<_>>())
}

fn glob_match_inner(glob: &[char], path: &[char]) -> bool {
    match (glob.first(), path.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            // ** matches path separators too; * stops at /
            let greedy = glob.first() == Some(&'*') && glob.get(1) == Some(&'*');
            if greedy {
                // try consuming 0..=n chars including /
                for i in 0..=path.len() {
                    if glob_match_inner(&glob[2..], &path[i..]) {
                        return true;
                    }
                }
                false
            } else {
                for i in 0..=path.len() {
                    if path.get(i) == Some(&'/') && i > 0 {
                        break;
                    }
                    if glob_match_inner(&glob[1..], &path[i..]) {
                        return true;
                    }
                }
                false
            }
        }
        (Some('?'), Some(_)) => glob_match_inner(&glob[1..], &path[1..]),
        (Some(g), Some(p)) if g.eq_ignore_ascii_case(p) => glob_match_inner(&glob[1..], &path[1..]),
        _ => false,
    }
}
