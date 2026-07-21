use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub use super::session::{session_date_id, session_epoch};

/// Resolve a path argument to a project root containing `.infigraph/`.
/// 1. If the path itself has `.infigraph/`, use it directly.
/// 2. Walk UP from the path looking for `.infigraph/` (handles subdirectory CWD).
/// 3. Check the global registry for a project whose path starts with (or contains) this path.
/// 4. Fall back to the original path (let downstream error).
pub fn resolve_project_path(path: &str) -> String {
    let start = if path == "." {
        std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path))
    };

    // A project .infigraph/ contains a "graph" subdir but NOT registry.json.
    // The global ~/.infigraph/ has registry.json — skip it during walk-up.
    let is_project_infigraph = |p: &std::path::Path| {
        let ig = p.join(".infigraph");
        ig.join("graph").exists() && !ig.join("registry.json").exists()
    };

    // Direct match
    if is_project_infigraph(&start) {
        return start.to_string_lossy().to_string();
    }

    // Walk up (CWD is inside a project subdir)
    let mut current = start.as_path();
    while let Some(parent) = current.parent() {
        if is_project_infigraph(parent) {
            return parent.to_string_lossy().to_string();
        }
        current = parent;
    }

    // Check registry for child projects under this path
    if let Ok(registry) = infigraph_core::multi::Registry::load() {
        for entry in registry.repos.values() {
            if entry.path.starts_with(&start) {
                return entry.path.to_string_lossy().to_string();
            }
        }
    }

    path.to_string()
}

pub fn open_prism(args: &Value) -> Result<Infigraph> {
    let raw_path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let path = resolve_project_path(raw_path);
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&PathBuf::from(&path), registry)?;
    prism.init()?;
    apply_repo_filter(&mut prism, &path);
    Ok(prism)
}

pub fn open_prism_read_only(args: &Value) -> Result<Infigraph> {
    let raw_path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let path = resolve_project_path(raw_path);
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&PathBuf::from(&path), registry)?;
    prism.init_read_only()?;
    apply_repo_filter(&mut prism, &path);
    Ok(prism)
}

/// In Neo4j (remote) mode, scope read queries to the repo matching this path.
/// Uses the same `org/repo` key that the group write path stamps onto `f.repo`
/// (bare repo name = last path component, org from `INFIGRAPH_ORG`). Read and write
/// MUST agree on this key or repo-scoped queries return nothing.
#[cfg(feature = "remote")]
fn apply_repo_filter(prism: &mut Infigraph, raw_path: &str) {
    if std::env::var("INFIGRAPH_BACKEND").as_deref() == Ok("neo4j") {
        let repo_name = std::path::Path::new(raw_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| raw_path.to_string());
        let org = infigraph_core::multi::default_org();
        let key = if org.is_empty() {
            repo_name
        } else {
            format!("{org}/{repo_name}")
        };
        prism.set_repo_filter(&key);
    }
}

#[cfg(not(feature = "remote"))]
fn apply_repo_filter(_prism: &mut Infigraph, _raw_path: &str) {}

pub fn find_infigraph_cli() -> Option<std::path::PathBuf> {
    let bin_name = if cfg!(windows) {
        "infigraph.exe"
    } else {
        "infigraph"
    };

    // Check same directory as this binary first, then parent (target/debug/deps/ → target/debug/)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(bin_name);
            if sibling.exists() {
                return Some(sibling);
            }
            if let Some(parent) = dir.parent() {
                let in_parent = parent.join(bin_name);
                if in_parent.exists() {
                    return Some(in_parent);
                }
            }
        }
    }
    // Fall back to PATH
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(out) = std::process::Command::new(which_cmd)
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

pub fn find_containing_symbol<'a>(
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

pub fn save_analysis(path: &str, tool_name: &str, content: &str) -> Result<String> {
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

pub fn log_activity(tool_name: &str, args: &Value) {
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

pub fn glob_matches(glob: &str, path: &str) -> bool {
    // Simple glob: * matches any sequence, ? matches one char
    let gi = glob.chars().peekable();
    let pi = path.chars().peekable();
    glob_match_inner(&gi.collect::<Vec<_>>(), &pi.collect::<Vec<_>>())
}

pub fn glob_match_inner(glob: &[char], path: &[char]) -> bool {
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
