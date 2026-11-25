use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use infigraph_core::embed;
use infigraph_core::graph::{SessionData, SessionStore};

pub fn open_session_store(args: &Value) -> Result<SessionStore> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    SessionStore::open(&PathBuf::from(path))
}

pub fn session_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn session_date_id() -> String {
    let secs = session_epoch();
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < dy {
            break;
        }
        remaining -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let md = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 0usize;
    for (i, &d) in md.iter().enumerate() {
        if remaining < d {
            mo = i;
            break;
        }
        remaining -= d;
    }
    format!("session_{y:04}-{:02}-{:02}", mo + 1, remaining + 1)
}

fn score_session_value(
    decisions: &str,
    constraints: &str,
    assumptions: &str,
    blockers: &str,
) -> f32 {
    let high_value_markers = [
        "invalidates-if",
        "invalidate",
        "if wrong",
        "do not retry",
        "failed because",
        "tried:",
        "blocked",
        "security",
        "compliance",
    ];
    let has_high_value = [decisions, constraints, assumptions, blockers]
        .iter()
        .any(|field| {
            let lower = field.to_lowercase();
            high_value_markers.iter().any(|m| lower.contains(m))
        });

    if has_high_value {
        return 0.9;
    }

    if !constraints.is_empty() || !blockers.is_empty() {
        return 0.85;
    }

    if !decisions.is_empty() || !assumptions.is_empty() {
        return 0.7;
    }

    // Summary-only session — lower initial confidence
    0.5
}

pub fn tool_save_session(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let summary = args
        .get("summary")
        .and_then(|s| s.as_str())
        .context("missing 'summary'")?;
    let pending_tasks = args
        .get("pending_tasks")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let decisions = args.get("decisions").and_then(|s| s.as_str()).unwrap_or("");
    let files_touched = args
        .get("files_touched")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let constraints = args
        .get("constraints")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let assumptions = args
        .get("assumptions")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let blockers = args.get("blockers").and_then(|s| s.as_str()).unwrap_or("");
    let narrative = args.get("narrative").and_then(|s| s.as_str()).unwrap_or("");
    let session_name = args.get("name").and_then(|s| s.as_str()).unwrap_or("");

    let now = session_epoch();
    let session_id = if session_name.is_empty() {
        session_date_id()
    } else {
        format!("named_{}", session_name.to_lowercase().replace(' ', "_"))
    };

    let new_files: Vec<&str> = files_touched
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let session = if let Some(existing) = store.load(&session_id)? {
        let merged_decisions = if decisions.is_empty() {
            existing.decisions.clone()
        } else if existing.decisions.is_empty() {
            decisions.to_string()
        } else {
            format!("{} | {}", existing.decisions, decisions)
        };

        let mut all_files: Vec<String> = existing
            .files_touched
            .split(", ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for f in &new_files {
            if !all_files.iter().any(|x| x == f) {
                all_files.push(f.to_string());
            }
        }

        SessionData {
            id: session_id.clone(),
            name: session_name.to_string(),
            summary: summary.to_string(),
            pending_tasks: pending_tasks.to_string(),
            decisions: merged_decisions,
            files_touched: all_files.join(", "),
            constraints: constraints.to_string(),
            assumptions: assumptions.to_string(),
            blockers: blockers.to_string(),
            created_at: existing.created_at,
            updated_at: now,
            confidence: 0.9_f32.max(existing.confidence),
            last_accessed: now,
        }
    } else {
        SessionData {
            id: session_id.clone(),
            name: session_name.to_string(),
            summary: summary.to_string(),
            pending_tasks: pending_tasks.to_string(),
            decisions: decisions.to_string(),
            files_touched: new_files.join(", "),
            constraints: constraints.to_string(),
            assumptions: assumptions.to_string(),
            blockers: blockers.to_string(),
            created_at: now,
            updated_at: now,
            confidence: score_session_value(decisions, constraints, assumptions, blockers),
            last_accessed: now,
        }
    };

    store.save(&session)?;

    let root = PathBuf::from(path);
    let sessions_dir = root.join(".infigraph").join("sessions");

    if !narrative.is_empty() {
        let md_path = sessions_dir.join(format!("{session_id}.md"));
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&md_path)?;
        let ts_secs = now % 86400;
        let hh = ts_secs / 3600;
        let mm = (ts_secs % 3600) / 60;
        writeln!(f, "\n## Save @ {hh:02}:{mm:02} UTC\n")?;
        writeln!(f, "{narrative}")?;
    }

    let emb_path = sessions_dir.join("embeddings.bin");
    let embed_text =
        format!("{session_name} {summary} {pending_tasks} {decisions} {constraints} {assumptions} {narrative}");
    let embedder = embed::code_embedder();
    let vec = embedder.embed(&embed_text)?;
    let mut emb_store = embed::load_embeddings(&emb_path).unwrap_or_default();
    emb_store.retain(|(id, _)| id != &session_id);
    emb_store.push((session_id.clone(), vec));
    embed::save_embeddings(&emb_path, &emb_store)?;

    // Auto-trigger consolidation when session count > 50
    let session_count = emb_store.len();
    let auto_consolidated = if session_count > 50 {
        let consolidate_args = serde_json::json!({ "path": path, "threshold": 0.7 });
        tool_consolidate_memory(&consolidate_args).ok()
    } else {
        None
    };

    let mut result = if session_name.is_empty() {
        format!("Session saved: {session_id}")
    } else {
        format!("Session saved: {session_id} (name: {session_name})")
    };

    if let Some(consolidation_msg) = auto_consolidated {
        result.push_str(&format!(
            "\n\n**Auto-consolidation triggered ({session_count} sessions):**\n{consolidation_msg}"
        ));
    }

    Ok(result)
}

pub const CLUSTER_GAP_SECS: i64 = 72 * 3600;

/// Sessions whose `updated_at` falls within [`CLUSTER_GAP_SECS`] of the newest session.
/// Unlike a chained walk from newest, this returns every session in the active window
/// (parallel daily/named saves) without an arbitrary cap.
pub fn detect_session_cluster(store: &SessionStore) -> Result<Vec<SessionData>> {
    let sorted = store.list_by_updated()?;
    if sorted.is_empty() {
        return Ok(vec![]);
    }
    let anchor = sorted[0].updated_at;
    Ok(sorted
        .into_iter()
        .filter(|s| anchor - s.updated_at <= CLUSTER_GAP_SECS)
        .collect())
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    format!("{}…", &s[..end])
}

fn decision_count(decisions: &str) -> usize {
    if decisions.is_empty() {
        0
    } else {
        1 + decisions.matches(" | Goal:").count()
    }
}

pub fn date_from_session_id(id: &str) -> &str {
    id.strip_prefix("session_").unwrap_or(id)
}

pub fn format_session_output(
    session: &SessionData,
    idx: usize,
    total: usize,
    path: &str,
) -> String {
    let mut out = String::new();

    if total == 1 {
        out.push_str("## Last Session Context\n\n");
    } else {
        out.push_str(&format!("## Session {} of {}\n\n", idx + 1, total));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let confidence = session.compute_confidence(now);

    if !session.name.is_empty() {
        out.push_str(&format!(
            "**Session:** {} (name: **{}**, confidence: {:.2})\n\n",
            session.id, session.name, confidence
        ));
    } else {
        out.push_str(&format!(
            "**Session:** {} (confidence: {:.2})\n\n",
            session.id, confidence
        ));
    }
    if !session.summary.is_empty() {
        out.push_str(&format!("**Summary:** {}\n\n", session.summary));
    }
    if !session.pending_tasks.is_empty() {
        out.push_str(&format!("**Pending Tasks:** {}\n\n", session.pending_tasks));
    }
    if !session.decisions.is_empty() {
        out.push_str(&format!("**Decisions:** {}\n\n", session.decisions));
    }
    if !session.files_touched.is_empty() {
        out.push_str(&format!("**Files Touched:** {}\n\n", session.files_touched));
    }
    if !session.constraints.is_empty() {
        out.push_str(&format!(
            "**Constraints (do not retry):** {}\n\n",
            session.constraints
        ));
    }
    if !session.assumptions.is_empty() {
        out.push_str(&format!(
            "**Assumptions (do not break):** {}\n\n",
            session.assumptions
        ));
    }
    if !session.blockers.is_empty() {
        out.push_str(&format!(
            "**Blockers (needs human):** {}\n\n",
            session.blockers
        ));
    }

    let narrative_path = PathBuf::from(path)
        .join(".infigraph")
        .join("sessions")
        .join(format!("{}.md", session.id));
    if narrative_path.exists() {
        out.push_str(&format!(
            "**Narrative log:** `{}` (read for full session context)\n\n",
            narrative_path.display()
        ));
    }
    out
}

/// Token-light card for multi-session restore. Omits bulky decisions/files; points to narrative.
pub fn format_session_compact(
    session: &SessionData,
    idx: usize,
    total: usize,
    path: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("### {}. {}\n\n", idx + 1, session.id));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let confidence = session.compute_confidence(now);

    if !session.name.is_empty() {
        out.push_str(&format!(
            "**Name:** {} | **Confidence:** {:.2}\n\n",
            session.name, confidence
        ));
    } else {
        out.push_str(&format!("**Confidence:** {:.2}\n\n", confidence));
    }
    if !session.summary.is_empty() {
        out.push_str(&format!(
            "**Summary:** {}\n\n",
            truncate_chars(&session.summary, 240)
        ));
    }
    if !session.pending_tasks.is_empty() {
        out.push_str(&format!(
            "**Pending:** {}\n\n",
            truncate_chars(&session.pending_tasks, 180)
        ));
    }
    let n = decision_count(&session.decisions);
    if n > 0 {
        out.push_str(&format!(
            "**Decisions:** ({n} — use `name='{}'` or narrative for full)\n\n",
            if session.name.is_empty() {
                session.id.clone()
            } else {
                session.name.clone()
            }
        ));
    }
    if !session.blockers.is_empty() {
        out.push_str(&format!(
            "**Blockers:** {}\n\n",
            truncate_chars(&session.blockers, 120)
        ));
    }

    let narrative_path = PathBuf::from(path)
        .join(".infigraph")
        .join("sessions")
        .join(format!("{}.md", session.id));
    if narrative_path.exists() {
        out.push_str(&format!(
            "**Narrative:** `{}`\n\n",
            narrative_path.display()
        ));
    }
    let _ = total; // idx/total used in header by caller
    out
}

pub fn append_activity_log(out: &mut String, path: &str) {
    let today_date = session_date_id().replace("session_", "");
    let activity_path = PathBuf::from(path)
        .join(".infigraph")
        .join("sessions")
        .join(format!("activity_{today_date}.jsonl"));
    if activity_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&activity_path) {
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let tail = if total > 20 {
                &lines[total - 20..]
            } else {
                &lines[..]
            };
            if !tail.is_empty() {
                out.push_str(&format!(
                    "## Activity Log (today, last {} of {} calls)\n\n",
                    tail.len(),
                    total
                ));
                for line in tail {
                    if let Ok(entry) = serde_json::from_str::<Value>(line) {
                        let tool = entry.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
                        let status = entry.get("status").and_then(|s| s.as_str()).unwrap_or("ok");
                        let marker = if status == "ok" { "" } else { " FAILED" };
                        let args_obj = entry.get("args").cloned().unwrap_or(json!({}));
                        let args_str = serde_json::to_string(&args_obj).unwrap_or_default();
                        let preview = if args_str.len() > 80 {
                            &args_str[..80]
                        } else {
                            &args_str
                        };
                        out.push_str(&format!("- `{tool}`{marker} {preview}\n"));
                    }
                }
                out.push('\n');
            }
        }
    }
}

pub fn append_old_session_hint(sessions_dir: &std::path::Path, out: &mut String) {
    if let Ok(entries) = std::fs::read_dir(sessions_dir) {
        let session_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                s.starts_with("session_") && s.ends_with(".json")
            })
            .collect();
        if session_files.len() > 30 {
            out.push_str(&format!(
                "\n> {} session files found. Consider running `purge_sessions` to clean up old sessions.\n",
                session_files.len()
            ));
        }
    }
}

pub fn tool_get_latest_session(args: &Value) -> Result<String> {
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let explicit_limit = args.get("limit").and_then(|v| v.as_u64());
    let session_name = args.get("name").and_then(|s| s.as_str()).unwrap_or("");
    let detail = args
        .get("detail")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let store = open_session_store(args)?;

    if !session_name.is_empty() {
        return if let Some(session) = store.load_by_name(session_name)? {
            let mut out = format_session_output(&session, 0, 1, path);
            append_activity_log(&mut out, path);
            append_restore_caveat(&mut out, path);
            Ok(out)
        } else {
            Ok(format!("No session found with name '{session_name}'."))
        };
    }

    let sessions = if let Some(limit) = explicit_limit {
        store.list_recent(limit as usize)?
    } else {
        detect_session_cluster(&store)?
    };

    if sessions.is_empty() {
        return Ok("No previous sessions found. This is a fresh start.".to_string());
    }

    let mut out = String::new();
    let total = sessions.len();
    let multi = total > 1 && !detail;

    if multi {
        let newest_date = date_from_session_id(&sessions[0].id);
        let oldest_date = date_from_session_id(&sessions[total - 1].id);
        out.push_str(&format!(
            "## {total} recent sessions ({oldest_date} — {newest_date})\n\n\
             All sessions updated within 72h of the newest save. \
             Compact index below; use `name='<label>'` or `detail=true` for full fields.\n\n"
        ));
        for (idx, session) in sessions.iter().enumerate() {
            out.push_str(&format_session_compact(session, idx, total, path));
        }
    } else {
        for (idx, session) in sessions.iter().enumerate() {
            out.push_str(&format_session_output(session, idx, total, path));
            if idx < total - 1 {
                out.push_str("\n---\n\n");
            }
        }
    }

    append_activity_log(&mut out, path);
    append_old_session_hint(store.sessions_dir(), &mut out);
    append_restore_caveat(&mut out, path);

    Ok(out)
}

/// Restored notes are a snapshot of an earlier session. Say so, and say
/// whether a code graph exists: session files alone don't mean the code
/// tools will work for this path.
pub fn append_restore_caveat(out: &mut String, path: &str) {
    out.push_str(
        "_These are historical notes from earlier sessions. Check pending tasks and blockers \
         against the repo's current instruction files and the user's current request before \
         treating them as live requirements._\n\n",
    );
    let infigraph_dir = PathBuf::from(path).join(".infigraph");
    let indexed = infigraph_dir.join("graph").exists()
        || infigraph_dir.join("graph.cozo").exists()
        || std::env::var("INFIGRAPH_BACKEND").is_ok_and(|b| b == "neo4j");
    if indexed {
        out.push_str("**Code graph:** indexed.\n");
    } else {
        out.push_str(&format!(
            "**Code graph:** not indexed. Session notes exist, but code tools will return \
             PROJECT_NOT_INDEXED until `infigraph index` runs in {path} (or index_project).\n"
        ));
    }
}

pub fn tool_purge_sessions(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let older_than_days = args
        .get("older_than_days")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let now = session_epoch();
    let cutoff = now - (older_than_days as i64 * 86400);

    let all = store.list_all()?;
    let to_purge: Vec<&SessionData> = all.iter().filter(|s| s.created_at < cutoff).collect();

    if to_purge.is_empty() {
        return Ok(format!(
            "No sessions older than {older_than_days} days found."
        ));
    }

    let purged_ids: Vec<String> = to_purge.iter().map(|s| s.id.clone()).collect();

    for id in &purged_ids {
        store.delete(id)?;
    }

    let root = PathBuf::from(path);
    let emb_path = root
        .join(".infigraph")
        .join("sessions")
        .join("embeddings.bin");
    if emb_path.exists() {
        let mut emb_store = embed::load_embeddings(&emb_path).unwrap_or_default();
        let before = emb_store.len();
        emb_store.retain(|(id, _)| !purged_ids.contains(id));
        if emb_store.len() < before {
            embed::save_embeddings(&emb_path, &emb_store)?;
        }
    }

    let mut out = format!(
        "Purged {} session(s) older than {} days:\n",
        to_purge.len(),
        older_than_days
    );
    for s in &to_purge {
        let preview = if s.summary.len() > 60 {
            &s.summary[..60]
        } else {
            &s.summary
        };
        out.push_str(&format!("- {}: {preview}\n", s.id));
    }
    Ok(out)
}

pub fn tool_search_sessions(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let query = args
        .get("query")
        .and_then(|s| s.as_str())
        .context("missing 'query'")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let root = PathBuf::from(path);
    let emb_path = root
        .join(".infigraph")
        .join("sessions")
        .join("embeddings.bin");

    if !emb_path.exists() {
        return Ok(
            "No session embeddings found. Save at least one session with `save_session` first."
                .to_string(),
        );
    }

    let emb_store = embed::load_embeddings(&emb_path)?;
    if emb_store.is_empty() {
        return Ok("No session embeddings found.".to_string());
    }

    let embedder = embed::code_embedder();
    let query_vec = embedder.embed(query)?;
    if query_vec.is_empty() {
        return Ok("Failed to embed query.".to_string());
    }

    let mut scored: Vec<(f32, &str)> = emb_store
        .iter()
        .map(|(id, vec)| (embed::cosine_similarity(&query_vec, vec), id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let mut out = format!("## Session Search: \"{query}\"\n\n");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    for (score, session_id) in &scored {
        if let Some(session) = store.load(session_id)? {
            let confidence = session.compute_confidence(now);

            if session.is_archived(now) {
                continue;
            }

            let _ = store.touch_session(session_id);

            let header = if session.name.is_empty() {
                format!(
                    "### {} (relevance: {:.3}, confidence: {:.2})\n\n",
                    session.id, score, confidence
                )
            } else {
                format!(
                    "### {} — \"{}\" (relevance: {:.3}, confidence: {:.2})\n\n",
                    session.id, session.name, score, confidence
                )
            };
            out.push_str(&header);
            if !session.summary.is_empty() {
                out.push_str(&format!("**Summary:** {}\n\n", session.summary));
            }
            if !session.pending_tasks.is_empty() {
                out.push_str(&format!("**Pending Tasks:** {}\n\n", session.pending_tasks));
            }
            if !session.decisions.is_empty() {
                out.push_str(&format!("**Decisions:** {}\n\n", session.decisions));
            }
            if !session.files_touched.is_empty() {
                out.push_str(&format!("**Files Touched:** {}\n\n", session.files_touched));
            }
            let narrative_path = root
                .join(".infigraph")
                .join("sessions")
                .join(format!("{session_id}.md"));
            if narrative_path.exists() {
                out.push_str(&format!(
                    "**Narrative log:** `{}` (read for full context)\n\n",
                    narrative_path.display()
                ));
            }
            out.push_str("---\n\n");
        }
    }

    Ok(out)
}

pub fn tool_consolidate_memory(args: &Value) -> Result<String> {
    let store = open_session_store(args)?;
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let similarity_threshold = args
        .get("threshold")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.7) as f32;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Purge expired sessions (confidence < 0.1)
    let purged = store.purge_expired(now)?;

    let root = PathBuf::from(path);
    let emb_path = root
        .join(".infigraph")
        .join("sessions")
        .join("embeddings.bin");

    if !emb_path.exists() {
        let msg = if purged.is_empty() {
            "No session embeddings found. Nothing to consolidate.".to_string()
        } else {
            format!(
                "Purged {} expired sessions. No embeddings to consolidate.",
                purged.len()
            )
        };
        return Ok(msg);
    }

    let emb_store = embed::load_embeddings(&emb_path)?;
    if emb_store.len() < 2 {
        return Ok("Fewer than 2 sessions — nothing to consolidate.".to_string());
    }

    // Load all active sessions with embeddings
    let mut sessions_with_emb: Vec<(SessionData, Vec<f32>)> = Vec::new();
    for (id, emb) in &emb_store {
        if let Some(session) = store.load(id)? {
            if !session.is_archived(now) && !id.starts_with("consolidated_") {
                sessions_with_emb.push((session, emb.clone()));
            }
        }
    }

    if sessions_with_emb.len() < 2 {
        return Ok("Fewer than 2 active sessions — nothing to consolidate.".to_string());
    }

    // Union-find clustering by similarity
    let n = sessions_with_emb.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] != i {
            parent[i] = find(parent, parent[i]);
        }
        parent[i]
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let sim = embed::cosine_similarity(&sessions_with_emb[i].1, &sessions_with_emb[j].1);
            if sim >= similarity_threshold {
                let pi = find(&mut parent, i);
                let pj = find(&mut parent, j);
                if pi != pj {
                    parent[pi] = pj;
                }
            }
        }
    }

    // Build clusters
    let mut clusters: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let root_idx = find(&mut parent, i);
        clusters.entry(root_idx).or_default().push(i);
    }

    let mut consolidated_count = 0;
    let mut out = String::from("## Memory Consolidation\n\n");

    for members in clusters.values() {
        if members.len() < 2 {
            continue;
        }

        // Merge sessions in cluster
        let mut merged_summary = String::new();
        let mut merged_decisions = String::new();
        let mut merged_constraints = String::new();
        let mut merged_assumptions = String::new();
        let mut merged_blockers = String::new();
        let mut merged_files: Vec<String> = Vec::new();
        let mut source_ids: Vec<String> = Vec::new();
        let mut earliest_created = i64::MAX;
        let mut latest_updated = 0i64;

        for &idx in members {
            let session = &sessions_with_emb[idx].0;
            source_ids.push(session.id.clone());

            if !session.summary.is_empty() {
                if !merged_summary.is_empty() {
                    merged_summary.push_str(" | ");
                }
                merged_summary.push_str(&session.summary);
            }
            if !session.decisions.is_empty() {
                if !merged_decisions.is_empty() {
                    merged_decisions.push_str(" | ");
                }
                merged_decisions.push_str(&session.decisions);
            }
            if !session.constraints.is_empty() {
                if !merged_constraints.is_empty() {
                    merged_constraints.push_str(" | ");
                }
                merged_constraints.push_str(&session.constraints);
            }
            if !session.assumptions.is_empty() {
                if !merged_assumptions.is_empty() {
                    merged_assumptions.push_str(" | ");
                }
                merged_assumptions.push_str(&session.assumptions);
            }
            if !session.blockers.is_empty() {
                if !merged_blockers.is_empty() {
                    merged_blockers.push_str(" | ");
                }
                merged_blockers.push_str(&session.blockers);
            }
            for f in session.files_touched.split(',').map(|s| s.trim()) {
                if !f.is_empty() && !merged_files.contains(&f.to_string()) {
                    merged_files.push(f.to_string());
                }
            }
            earliest_created = earliest_created.min(session.created_at);
            latest_updated = latest_updated.max(session.updated_at);
        }

        let consolidated_id = format!("consolidated_{}", earliest_created);

        let consolidated = SessionData {
            id: consolidated_id.clone(),
            name: format!("Consolidated ({} sessions)", members.len()),
            summary: merged_summary,
            pending_tasks: String::new(),
            decisions: merged_decisions,
            files_touched: merged_files.join(", "),
            constraints: merged_constraints,
            assumptions: merged_assumptions,
            blockers: merged_blockers,
            created_at: earliest_created,
            updated_at: now,
            confidence: 0.9,
            last_accessed: now,
        };

        store.save(&consolidated)?;

        // Re-embed consolidated session
        let embed_text = format!(
            "{} {} {} {}",
            consolidated.summary,
            consolidated.decisions,
            consolidated.constraints,
            consolidated.assumptions
        );
        let embedder = embed::code_embedder();
        if let Ok(emb_vec) = embedder.embed(&embed_text) {
            let mut all_embs = embed::load_embeddings(&emb_path)?;
            all_embs.push((consolidated_id.clone(), emb_vec));
            embed::save_embeddings(&emb_path, &all_embs)?;
        }

        // Lower confidence of source sessions (superseded, not deleted)
        for &idx in members {
            let mut session = sessions_with_emb[idx].0.clone();
            session.confidence = (session.compute_confidence(now) * 0.5).max(0.3);
            store.save(&session)?;
        }

        out.push_str(&format!(
            "### {} — merged {} sessions\n",
            consolidated_id,
            members.len()
        ));
        out.push_str(&format!("Sources: {}\n\n", source_ids.join(", ")));
        consolidated_count += 1;
    }

    if consolidated_count == 0 {
        out.push_str(
            "No clusters found above similarity threshold. Sessions are already distinct.\n",
        );
    } else {
        out.push_str(&format!(
            "\n**Created {} consolidated session(s).** Source sessions preserved with reduced confidence.\n",
            consolidated_count
        ));
    }

    // Build symbol clusters from co-occurrence data
    match super::memory_context::build_symbol_clusters(path, 3) {
        Ok(clusters) if !clusters.is_empty() => {
            out.push_str(&format!(
                "\n**Symbol clusters:** {} cluster(s) built from co-retrieval patterns.\n",
                clusters.len()
            ));
        }
        Ok(_) => {}
        Err(_) => {}
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use infigraph_core::graph::{SessionData, SessionStore};

    fn make_session(id: &str, created_at: i64, updated_at: i64) -> SessionData {
        SessionData {
            id: id.to_string(),
            name: String::new(),
            summary: format!("work on {id}"),
            pending_tasks: String::new(),
            decisions: String::new(),
            files_touched: String::new(),
            constraints: String::new(),
            assumptions: String::new(),
            blockers: String::new(),
            created_at,
            updated_at,
            confidence: 0.0,
            last_accessed: 0,
        }
    }

    fn store_with_sessions(sessions: &[SessionData]) -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        for s in sessions {
            store.save(s).unwrap();
        }
        (dir, store)
    }

    #[test]
    fn test_cluster_single_session() {
        let (_dir, store) = store_with_sessions(&[make_session("session_2026-06-08", 1000, 1000)]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(cluster.len(), 1);
        assert_eq!(cluster[0].id, "session_2026-06-08");
    }

    #[test]
    fn test_cluster_two_sessions_within_24h() {
        let now = 1_750_000_000i64;
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-06-07", now - 86400, now - 3600),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(cluster.len(), 2, "both sessions within 24h should cluster");
    }

    #[test]
    fn test_cluster_two_sessions_within_72h() {
        let now = 1_750_000_000i64;
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-06-05", now - 200_000, now - 200_000),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(
            cluster.len(),
            2,
            "sessions 55h apart should cluster (< 72h)"
        );
    }

    #[test]
    fn test_cluster_gap_exceeds_72h() {
        let now = 1_750_000_000i64;
        let old = now - (73 * 3600);
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-06-01", old - 86400, old),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(cluster.len(), 1, "73h gap should break cluster");
        assert_eq!(cluster[0].id, "session_2026-06-08");
    }

    #[test]
    fn test_cluster_chained_48h_gaps() {
        let now = 1_750_000_000i64;
        let h48 = 48 * 3600;
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-06-04", now - 2 * h48, now - 2 * h48),
            make_session("session_2026-06-06", now - h48, now - h48),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(
            cluster.len(),
            2,
            "96h-old session falls outside 72h window from newest"
        );
        assert_eq!(cluster[0].id, "session_2026-06-08");
        assert_eq!(cluster[1].id, "session_2026-06-06");
    }

    #[test]
    fn test_cluster_chain_breaks_at_old_session() {
        let now = 1_750_000_000i64;
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-05-01", now - 30 * 86400, now - 30 * 86400),
            make_session("session_2026-06-07", now - 86400, now - 86400),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(
            cluster.len(),
            2,
            "old session 30d ago should not be in cluster"
        );
        assert_eq!(cluster[0].id, "session_2026-06-08");
        assert_eq!(cluster[1].id, "session_2026-06-07");
    }

    #[test]
    fn test_cluster_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        let cluster = detect_session_cluster(&store).unwrap();
        assert!(cluster.is_empty());
    }

    #[test]
    fn test_cluster_many_parallel_same_week() {
        let now = 1_750_000_000i64;
        let (_dir, store) = store_with_sessions(&[
            make_session("session_2026-06-04", now - 4 * 86400, now - 4 * 86400),
            make_session("session_2026-06-05", now - 3 * 86400, now - 3 * 86400),
            make_session("session_2026-06-06", now - 2 * 86400, now - 2 * 86400),
            make_session("session_2026-06-07", now - 86400, now - 86400),
            make_session("session_2026-06-08", now, now),
        ]);
        let cluster = detect_session_cluster(&store).unwrap();
        assert_eq!(
            cluster.len(),
            4,
            "all sessions within 72h of newest (days 0–3), none capped"
        );
    }

    #[test]
    fn test_date_from_session_id() {
        assert_eq!(date_from_session_id("session_2026-06-08"), "2026-06-08");
        assert_eq!(date_from_session_id("weird_id"), "weird_id");
    }

    #[test]
    fn test_multi_session_compact_omits_decisions_body() {
        let now = 1_750_000_000i64;
        let mut s1 = make_session("session_2026-06-07", now - 86400, now - 3600);
        s1.summary = "Worked on clustering".into();
        s1.pending_tasks = "Fix tests".into();
        s1.decisions =
            "Goal: foo. Decision: bar. Why: baz. | Goal: qux. Decision: qax. Why: qaz.".into();
        let mut s2 = make_session("session_2026-06-08", now, now);
        s2.summary = "Token burn fix".into();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let sessions_dir = project.join(".infigraph").join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = SessionStore::open_dir(&sessions_dir).unwrap();
        store.save(&s1).unwrap();
        store.save(&s2).unwrap();
        let out = tool_get_latest_session(&json!({"path": project.to_str().unwrap()})).unwrap();
        assert!(out.contains("2 recent sessions"));
        assert!(out.contains("**Decisions:** (2 — use"));
        assert!(!out.contains("Goal: foo. Decision: bar"));
    }

    #[test]
    fn test_latest_session_marks_notes_historical_and_reports_missing_graph() {
        let now = 1_750_000_000i64;
        let mut s1 = make_session("session_2026-06-07", now, now);
        s1.pending_tasks = "Obtain maintainer approval".into();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let sessions_dir = project.join(".infigraph").join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        SessionStore::open_dir(&sessions_dir)
            .unwrap()
            .save(&s1)
            .unwrap();

        let out = tool_get_latest_session(&json!({"path": project.to_str().unwrap()})).unwrap();
        assert!(out.contains("historical notes"), "{out}");
        assert!(out.contains("**Code graph:** not indexed"), "{out}");

        std::fs::create_dir_all(project.join(".infigraph").join("graph")).unwrap();
        let out = tool_get_latest_session(&json!({"path": project.to_str().unwrap()})).unwrap();
        assert!(out.contains("**Code graph:** indexed."), "{out}");
    }
}
