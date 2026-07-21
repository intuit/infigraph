use serde_json::{json, Value};

use super::open_prism;

pub(crate) fn api_git_summary(params: &Value) -> Value {
    let n = params
        .get("n_commits")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;
    match open_prism(params) {
        Ok(prism) => {
            let root = prism.root().to_path_buf();
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error":"not initialized"}),
            };

            let n_arg = format!("-{}", n);
            let log_out = std::process::Command::new("git")
                .args(["log", "--format=%H\x1f%an\x1f%ai\x1f%s", &n_arg])
                .current_dir(&root)
                .output();

            let log_text = match log_out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
                _ => return json!({"error": "git log failed"}),
            };

            let mut commits: Vec<Value> = Vec::new();
            for line in log_text.lines() {
                let parts: Vec<&str> = line.splitn(4, '\x1f').collect();
                if parts.len() < 4 {
                    continue;
                }
                let hash = parts[0];
                let author = parts[1];
                let date = &parts[2][..10.min(parts[2].len())];
                let subject = parts[3];
                let short = &hash[..8.min(hash.len())];

                let parent_ref = format!("{}^", hash);
                let diff_out = std::process::Command::new("git")
                    .args(["diff", "--unified=0", &parent_ref, hash])
                    .current_dir(&root)
                    .output();

                let hunks = match diff_out {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        parse_web_diff_hunks(&text)
                    }
                    _ => vec![],
                };

                let mut touched: Vec<Value> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (file, start, end) in &hunks {
                    if let Ok(syms) = backend.symbols_in_range(file, *start, *end) {
                        for s in syms {
                            if seen.insert(s.id.clone()) {
                                touched.push(json!({"id":s.id,"name":s.name,"kind":s.kind,"file":s.file,"line":s.start_line}));
                            }
                        }
                    }
                }

                let files_ref = format!("{}^", hash);
                let files_out = std::process::Command::new("git")
                    .args(["diff", "--name-only", &files_ref, hash])
                    .current_dir(&root)
                    .output();
                let changed_files: Vec<String> = match files_out {
                    Ok(o) => String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect(),
                    _ => vec![],
                };

                commits.push(json!({
                    "hash": short, "author": author, "date": date,
                    "subject": subject, "files": changed_files, "symbols": touched,
                }));
            }
            json!({"commits": commits})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

fn parse_web_diff_hunks(diff: &str) -> Vec<(String, u32, u32)> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();
    for line in diff.lines() {
        if let Some(stripped) = line.strip_prefix("+++ b/") {
            current_file = stripped.to_string();
        } else if line.starts_with("@@ ") {
            // @@ -old +new,count @@
            if let Some(plus_part) = line.split('+').nth(1) {
                let range = plus_part.split(' ').next().unwrap_or("");
                let (start_str, count_str) = if let Some(comma) = range.find(',') {
                    (&range[..comma], &range[comma + 1..])
                } else {
                    (range, "1")
                };
                let start: u32 = start_str.parse().unwrap_or(1);
                let count: u32 = count_str.parse().unwrap_or(1);
                if !current_file.is_empty() {
                    hunks.push((current_file.clone(), start, start + count));
                }
            }
        }
    }
    hunks
}
