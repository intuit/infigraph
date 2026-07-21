use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;
use infigraph_core::graph::{GraphBackend, SessionStore};

use super::helpers::open_prism_read_only;

#[derive(Clone)]
struct ScoredItem {
    id: String,
    source_type: SourceType,
    score: f32,
    rendered_text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceType {
    Code = 0,
    Skeleton = 1,
    Session = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Depth {
    L1,
    L2,
    L3,
}

fn select_depth(query: &str, has_anchor: bool) -> Depth {
    let q = query.to_lowercase();
    let l3_keywords = [
        "refactor",
        "architecture",
        "design",
        "impact",
        "migrate",
        "migration",
        "restructure",
        "rewrite",
        "overview",
        "all ",
        "entire",
        "whole",
    ];
    let l2_keywords = [
        "caller",
        "callee",
        "uses",
        "used by",
        "related",
        "depends",
        "dependency",
        "calls",
        "called by",
        "how is",
        "how does",
        "where is",
        "who calls",
    ];

    for kw in &l3_keywords {
        if q.contains(kw) {
            return Depth::L3;
        }
    }
    for kw in &l2_keywords {
        if q.contains(kw) {
            return Depth::L2;
        }
    }
    if has_anchor {
        Depth::L1
    } else {
        Depth::L2
    }
}

pub fn tool_memory_context(args: &Value) -> Result<String> {
    let start = std::time::Instant::now();
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query'")?;
    let anchor_file = args.get("file").and_then(|f| f.as_str());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let sources_str = args
        .get("sources")
        .and_then(|s| s.as_str())
        .unwrap_or("code,sessions,skeleton");
    let want_code = sources_str.contains("code");
    let want_sessions = sources_str.contains("sessions");
    let want_skeleton = sources_str.contains("skeleton");

    let depth = match args.get("depth").and_then(|d| d.as_str()) {
        Some("L1" | "l1") => Depth::L1,
        Some("L2" | "l2") => Depth::L2,
        Some("L3" | "l3") => Depth::L3,
        _ => select_depth(query, anchor_file.is_some()),
    };

    let mut items: Vec<ScoredItem> = Vec::new();
    let mut always_include: Vec<ScoredItem> = Vec::new();

    if want_code {
        items.extend(gather_code(args, query, anchor_file, limit, depth)?);
    }

    if want_sessions {
        let (session_items, constraint_items) = gather_sessions(path, query)?;
        items.extend(session_items);
        always_include.extend(constraint_items);
    }

    let skeleton_item = match anchor_file {
        Some(f) if want_skeleton => gather_skeleton(args, f)?,
        _ => None,
    };

    if let Some(f) = anchor_file {
        if want_code {
            apply_anchor_boost(&mut items, args, f)?;
        }
    }

    // Apply cluster boost from prior co-occurrence data
    if want_code {
        apply_cluster_boost(&mut items, path);
    }

    let ranked = rank_and_assemble(items, skeleton_item, always_include);

    // Record co-occurrence for symbol clustering (Phase 5)
    let code_ids: Vec<String> = ranked
        .iter()
        .filter(|i| i.source_type == SourceType::Code)
        .map(|i| i.id.clone())
        .collect();
    record_cooccurrence(path, &code_ids);

    let output = render_output(query, &ranked)?;

    // Instrumentation: append usage log
    let code_count = ranked
        .iter()
        .filter(|i| i.source_type == SourceType::Code)
        .count();
    let session_count = ranked
        .iter()
        .filter(|i| i.source_type == SourceType::Session)
        .count();
    let depth_str = match depth {
        Depth::L1 => "L1",
        Depth::L2 => "L2",
        Depth::L3 => "L3",
    };
    let elapsed_ms = start.elapsed().as_millis();
    let tokens = output.len().div_ceil(4);
    let log_path = PathBuf::from(path)
        .join(".infigraph")
        .join("memory_context_log.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            f,
            r#"{{"ts":{},"query":"{}","depth":"{}","code":{},"sessions":{},"tokens":{},"ms":{}}}"#,
            ts,
            query.replace('"', "'"),
            depth_str,
            code_count,
            session_count,
            tokens,
            elapsed_ms
        );
    }

    Ok(output)
}

fn gather_code(
    args: &Value,
    query: &str,
    anchor_file: Option<&str>,
    limit: usize,
    mut depth: Depth,
) -> Result<Vec<ScoredItem>> {
    let prism = open_prism_read_only(args)?;
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let backend = prism
        .backend()
        .context("not indexed — run index_project first")?;

    // L1 requires anchor file; escalate to L2 if missing
    if depth == Depth::L1 && anchor_file.is_none() {
        depth = Depth::L2;
    }

    // --- L1: anchor file symbols + recent edits + active session ---
    if depth == Depth::L1 {
        let anchor = anchor_file.unwrap();
        let mut items = gather_file_symbols(backend, &prism, anchor, 0.8)?;
        let seen_ids: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();

        // Recent edits: git diff --name-only for modified files in same dir
        if let Ok(output) = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .current_dir(prism.root())
            .output()
        {
            if output.status.success() {
                let diff_files = String::from_utf8_lossy(&output.stdout);
                for diff_file in diff_files.lines().take(3) {
                    let diff_file = diff_file.trim();
                    if diff_file == anchor || diff_file.is_empty() {
                        continue;
                    }
                    for sym in backend.symbols_in_file(diff_file).unwrap_or_default() {
                        if !seen_ids.contains(&sym.id) {
                            items.push(ScoredItem {
                                id: sym.id,
                                source_type: SourceType::Code,
                                score: 0.6,
                                rendered_text: format!(
                                    "#### {}::{} (recent edit)\nKind: {} | Lines: {}-{}\n\n",
                                    diff_file, sym.name, sym.kind, sym.start_line, sym.end_line
                                ),
                            });
                        }
                    }
                }
            }
        }

        // Auto-escalate: L1 < 3 results → L2
        if items.len() < 3 {
            depth = Depth::L2;
        } else {
            return Ok(items);
        }
    }

    // --- L2: anchor symbols + callers/callees + file deps ---
    if depth == Depth::L2 {
        let mut items = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        if let Some(anchor) = anchor_file {
            let anchor_items = gather_file_symbols(backend, &prism, anchor, 0.8)?;
            for item in &anchor_items {
                seen_ids.insert(item.id.clone());
            }

            // Collect related symbol IDs via callers/callees
            let mut related_ids: HashSet<String> = HashSet::new();
            for item in &anchor_items {
                for caller in backend.callers_of(&item.id).unwrap_or_default() {
                    if !seen_ids.contains(&caller) {
                        related_ids.insert(caller);
                    }
                }
                for callee in backend.callees_of(&item.id).unwrap_or_default() {
                    if !seen_ids.contains(&callee) {
                        related_ids.insert(callee);
                    }
                }
            }

            // Collect related files via file deps
            if let Ok(deps) = backend.get_file_deps(anchor) {
                for imp_file in deps.imports.iter().chain(deps.imported_by.iter()) {
                    let file_syms = backend.symbols_in_file(imp_file).unwrap_or_default();
                    for sym in &file_syms {
                        if !seen_ids.contains(&sym.id) {
                            related_ids.insert(sym.id.clone());
                        }
                    }
                }
            }

            items.extend(anchor_items);

            // Render related symbols
            let related_rendered = render_symbol_ids(backend, &prism, &related_ids, 0.5)?;
            items.extend(related_rendered);
        } else {
            // No anchor: use BM25-only quick search for L2
            items = gather_l3_hybrid(query, backend, &prism, path, limit)?;
        }

        // Auto-escalate: L2 < 5 results → L3
        if items.len() < 5 && anchor_file.is_some() {
            let l3_items = gather_l3_hybrid(query, backend, &prism, path, limit)?;
            let existing_ids: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
            for item in l3_items {
                if !existing_ids.contains(&item.id) {
                    items.push(item);
                }
            }
        }

        items.truncate(limit);
        return Ok(items);
    }

    // --- L3: full hybrid search (current behavior) ---
    gather_l3_hybrid(query, backend, &prism, path, limit)
}

fn gather_file_symbols(
    backend: &dyn GraphBackend,
    prism: &infigraph_core::Infigraph,
    file: &str,
    base_score: f32,
) -> Result<Vec<ScoredItem>> {
    let file_symbols = backend.symbols_in_file(file).unwrap_or_default();
    let mut items = Vec::new();

    for sym in file_symbols {
        let start_line = sym.start_line;
        let end_line = sym.end_line;
        let mut text = format!(
            "#### {}::{} (anchor file)\nKind: {} | Lines: {}-{}\n",
            file, sym.name, sym.kind, start_line, end_line
        );

        let file_path = if std::path::Path::new(file).is_absolute() {
            std::path::PathBuf::from(file)
        } else {
            prism.root().join(file)
        };
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (start_line as usize).saturating_sub(1);
            let end = (end_line as usize).min(lines.len());
            if start < end {
                text.push_str("```\n");
                for (i, line) in lines[start..end].iter().enumerate() {
                    text.push_str(&format!("{:4}  {}\n", start + i + 1, line));
                }
                text.push_str("```\n");
            }
        }
        text.push('\n');

        items.push(ScoredItem {
            id: sym.id,
            source_type: SourceType::Code,
            score: base_score,
            rendered_text: text,
        });
    }

    Ok(items)
}

fn render_symbol_ids(
    backend: &dyn GraphBackend,
    prism: &infigraph_core::Infigraph,
    ids: &HashSet<String>,
    base_score: f32,
) -> Result<Vec<ScoredItem>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let id_list: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut items = Vec::new();

    for id in &id_list {
        let detail = match backend.find_symbol_by_id(id) {
            Ok(Some(d)) => d,
            _ => continue,
        };

        let mut text = format!(
            "#### {}::{} (related, score: {:.3})\nKind: {} | Lines: {}-{}\n",
            detail.file, detail.name, base_score, detail.kind, detail.start_line, detail.end_line
        );

        let file_path = if std::path::Path::new(&detail.file).is_absolute() {
            std::path::PathBuf::from(&detail.file)
        } else {
            prism.root().join(&detail.file)
        };
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (detail.start_line as usize).saturating_sub(1);
            let end = (detail.end_line as usize).min(lines.len());
            if start < end {
                text.push_str("```\n");
                for (i, line) in lines[start..end].iter().enumerate() {
                    text.push_str(&format!("{:4}  {}\n", start + i + 1, line));
                }
                text.push_str("```\n");
            }
        }
        text.push('\n');

        items.push(ScoredItem {
            id: id.to_string(),
            source_type: SourceType::Code,
            score: base_score,
            rendered_text: text,
        });
    }

    Ok(items)
}

fn gather_l3_hybrid(
    query: &str,
    backend: &dyn GraphBackend,
    prism: &infigraph_core::Infigraph,
    path: &str,
    limit: usize,
) -> Result<Vec<ScoredItem>> {
    let rows = backend.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring, s.start_line, s.end_line",
    )?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let docs: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let id = row[0].clone();
            let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                format!("{} {}: {}", row[2], row[1], row[4])
            } else {
                format!("{} {}", row[2], row[1])
            };
            (id, text)
        })
        .collect();

    let bm25_index = infigraph_core::search::BM25Index::build(docs.clone());
    let embedder = embed::best_embedder();
    let emb_path = PathBuf::from(path)
        .join(".infigraph")
        .join("embeddings.bin");
    let symbol_embeddings: Vec<(String, Vec<f32>)> = if emb_path.exists() {
        let all: HashMap<String, Vec<f32>> = embed::load_embeddings_cached(&emb_path)?
            .into_iter()
            .collect();
        docs.iter()
            .filter_map(|(id, text)| {
                all.get(id)
                    .cloned()
                    .or_else(|| embedder.embed(text).ok())
                    .map(|emb| (id.clone(), emb))
            })
            .collect()
    } else {
        docs.iter()
            .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
            .collect()
    };

    let oversample = limit * 2;
    let tg_dir = PathBuf::from(path).join(".infigraph");
    let hnsw_path = tg_dir.join("hnsw_index.usearch");
    let raw = infigraph_core::search::compute_raw_scores(
        query,
        &bm25_index,
        embedder.as_ref(),
        &symbol_embeddings,
        oversample,
        Some(&hnsw_path),
        Some(&emb_path),
    )?;

    let keyword_results = infigraph_core::search::combine_scores(&raw, 0.3, limit);
    let semantic_results = infigraph_core::search::combine_scores(&raw, 0.85, limit);

    let mut merged: HashMap<String, infigraph_core::search::SearchResult> = HashMap::new();
    for r in keyword_results.into_iter().chain(semantic_results) {
        merged
            .entry(r.symbol_id.clone())
            .and_modify(|existing| {
                if r.score > existing.score {
                    *existing = r.clone();
                }
            })
            .or_insert(r);
    }

    let mut results: Vec<infigraph_core::search::SearchResult> = merged.into_values().collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);

    let row_map: HashMap<&str, &Vec<String>> =
        rows.iter().map(|row| (row[0].as_str(), row)).collect();

    let mut scored_items = Vec::new();
    for r in &results {
        let row = match row_map.get(r.symbol_id.as_str()) {
            Some(row) => row,
            None => continue,
        };

        let start_line: usize = row.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
        let end_line: usize = row.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
        let file = &row[3];
        let kind = &row[2];
        let name = &row[1];
        let docstring = row.get(4).filter(|s| !s.is_empty());

        let mut text = format!("#### {}::{} (score: {:.3})\n", file, name, r.score);
        text.push_str(&format!(
            "Kind: {} | File: {}:{}-{}\n",
            kind, file, start_line, end_line
        ));
        if let Some(doc) = docstring {
            text.push_str(&format!("Doc: {}\n", doc));
        }

        let file_path = if std::path::Path::new(file).is_absolute() {
            std::path::PathBuf::from(file)
        } else {
            prism.root().join(file)
        };
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = start_line.saturating_sub(1);
            let end = end_line.min(lines.len());
            if start < end {
                text.push_str("```\n");
                for (i, line) in lines[start..end].iter().enumerate() {
                    text.push_str(&format!("{:4}  {}\n", start + i + 1, line));
                }
                text.push_str("```\n");
            }
        }

        let callers = backend.callers_of(&r.symbol_id).unwrap_or_default();
        let callees = backend.callees_of(&r.symbol_id).unwrap_or_default();
        if !callers.is_empty() {
            text.push_str(&format!(
                "Callers: {}\n",
                callers
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !callees.is_empty() {
            text.push_str(&format!(
                "Callees: {}\n",
                callees
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        text.push('\n');

        scored_items.push(ScoredItem {
            id: r.symbol_id.clone(),
            source_type: SourceType::Code,
            score: r.score,
            rendered_text: text,
        });
    }

    Ok(scored_items)
}

fn gather_sessions(path: &str, query: &str) -> Result<(Vec<ScoredItem>, Vec<ScoredItem>)> {
    let root = PathBuf::from(path);
    let store = SessionStore::open(&root)?;
    let emb_path = root
        .join(".infigraph")
        .join("sessions")
        .join("embeddings.bin");

    let mut session_items = Vec::new();
    let mut constraint_items = Vec::new();

    if !emb_path.exists() {
        return Ok((session_items, constraint_items));
    }

    let emb_store = embed::load_embeddings(&emb_path)?;
    if emb_store.is_empty() {
        return Ok((session_items, constraint_items));
    }

    let embedder = embed::code_embedder();
    let query_vec = embedder.embed(query)?;
    if query_vec.is_empty() {
        return Ok((session_items, constraint_items));
    }

    let mut scored: Vec<(f32, &str)> = emb_store
        .iter()
        .map(|(id, vec)| (embed::cosine_similarity(&query_vec, vec), id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(5);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    for (score, session_id) in &scored {
        if let Some(session) = store.load(session_id)? {
            let confidence = session.compute_confidence(now);

            // Skip archived sessions (confidence below threshold)
            if session.is_archived(now) {
                continue;
            }

            // Score = relevance * session_weight * confidence
            let weighted_score = score * 0.7 * confidence;

            // Touch session to update last_accessed
            let _ = store.touch_session(session_id);

            let created = format_epoch(session.created_at);
            let updated = format_epoch(session.updated_at);
            let name_label = if session.name.is_empty() {
                String::new()
            } else {
                format!(" — \"{}\"", session.name)
            };

            let mut text = format!(
                "#### {}{} (relevance: {:.3}, confidence: {:.2})\n",
                session.id, name_label, weighted_score, confidence
            );
            text.push_str(&format!("Created: {} | Updated: {}\n", created, updated));

            if !session.summary.is_empty() {
                text.push_str(&format!("**Summary:** {}\n", session.summary));
            }
            if !session.pending_tasks.is_empty() {
                text.push_str(&format!("**Pending:** {}\n", session.pending_tasks));
            }
            if !session.decisions.is_empty() {
                text.push_str(&format!("**Decisions:** {}\n", session.decisions));
            }
            text.push('\n');

            session_items.push(ScoredItem {
                id: session_id.to_string(),
                source_type: SourceType::Session,
                score: weighted_score,
                rendered_text: text,
            });

            if !session.constraints.is_empty() || !session.blockers.is_empty() {
                let mut ctext = String::new();
                if !session.constraints.is_empty() {
                    ctext.push_str(&format!(
                        "**Constraint ({}):** {}\n",
                        session.id, session.constraints
                    ));
                }
                if !session.blockers.is_empty() {
                    ctext.push_str(&format!(
                        "**Blocker ({}):** {}\n",
                        session.id, session.blockers
                    ));
                }
                constraint_items.push(ScoredItem {
                    id: format!("{}_constraints", session_id),
                    source_type: SourceType::Session,
                    score: 1.0,
                    rendered_text: ctext,
                });
            }
        }
    }

    Ok((session_items, constraint_items))
}

fn gather_skeleton(args: &Value, file: &str) -> Result<Option<ScoredItem>> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not indexed")?;

    match backend.skeleton(file) {
        Ok(skeleton_text) if !skeleton_text.is_empty() => {
            let text = format!("#### Skeleton: {}\n{}\n", file, skeleton_text);
            Ok(Some(ScoredItem {
                id: format!("skeleton_{}", file),
                source_type: SourceType::Skeleton,
                score: 0.9,
                rendered_text: text,
            }))
        }
        _ => Ok(None),
    }
}

fn apply_anchor_boost(items: &mut [ScoredItem], args: &Value, anchor_file: &str) -> Result<()> {
    let prism = open_prism_read_only(args)?;
    let backend = match prism.backend() {
        Some(b) => b,
        None => return Ok(()),
    };

    let file_symbols = backend.symbols_in_file(anchor_file).unwrap_or_default();
    let anchor_ids: std::collections::HashSet<String> =
        file_symbols.iter().map(|s| s.id.clone()).collect();

    let mut related_ids: std::collections::HashSet<String> = anchor_ids.clone();
    for sym in &file_symbols {
        for caller in backend.callers_of(&sym.id).unwrap_or_default() {
            related_ids.insert(caller);
        }
        for callee in backend.callees_of(&sym.id).unwrap_or_default() {
            related_ids.insert(callee);
        }
    }

    for item in items.iter_mut() {
        if item.source_type == SourceType::Code && related_ids.contains(&item.id) {
            item.score = (item.score + 0.15).min(1.0);
        }
    }

    Ok(())
}

fn rank_and_assemble(
    mut items: Vec<ScoredItem>,
    skeleton: Option<ScoredItem>,
    always_include: Vec<ScoredItem>,
) -> Vec<ScoredItem> {
    // Sort by score descending, then source_type, then id (deterministic)
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.source_type.cmp(&b.source_type))
            .then(a.id.cmp(&b.id))
    });

    let mut result: Vec<ScoredItem> = Vec::new();

    // Always-include first (constraints/blockers)
    result.extend(always_include);

    // Skeleton
    if let Some(skel) = skeleton {
        result.push(skel);
    }

    // All ranked items
    result.extend(items);

    result
}

fn render_output(query: &str, items: &[ScoredItem]) -> Result<String> {
    let mut out = format!("## Memory Context: \"{}\"\n\n", query);

    let code_items: Vec<&ScoredItem> = items
        .iter()
        .filter(|i| i.source_type == SourceType::Code)
        .collect();
    let skeleton_items: Vec<&ScoredItem> = items
        .iter()
        .filter(|i| i.source_type == SourceType::Skeleton)
        .collect();
    let session_items: Vec<&ScoredItem> = items
        .iter()
        .filter(|i| i.source_type == SourceType::Session)
        .collect();

    if !code_items.is_empty() {
        out.push_str(&format!("### Code ({} results)\n\n", code_items.len()));
        for item in &code_items {
            out.push_str(&item.rendered_text);
        }
    }

    if !skeleton_items.is_empty() {
        out.push_str("### Skeleton\n\n");
        for item in &skeleton_items {
            out.push_str(&item.rendered_text);
        }
    }

    if !session_items.is_empty() {
        out.push_str(&format!(
            "### Sessions ({} results)\n\n",
            session_items.len()
        ));
        for item in &session_items {
            out.push_str(&item.rendered_text);
        }
    }

    Ok(out)
}

/// Co-occurrence matrix: tracks how often symbol pairs appear together in memory_context results.
/// Stored as JSON: { "sym_a\tsym_b": count, ... } where sym_a < sym_b lexicographically.
fn cooccurrence_path(path: &str) -> PathBuf {
    PathBuf::from(path)
        .join(".infigraph")
        .join("symbol_cooccurrence.json")
}

fn cluster_path(path: &str) -> PathBuf {
    PathBuf::from(path)
        .join(".infigraph")
        .join("symbol_clusters.json")
}

fn record_cooccurrence(path: &str, symbol_ids: &[String]) {
    if symbol_ids.len() < 2 {
        return;
    }
    let co_path = cooccurrence_path(path);
    let mut counts: HashMap<String, u32> = if co_path.exists() {
        std::fs::read_to_string(&co_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    let ids: Vec<&str> = symbol_ids.iter().map(|s| s.as_str()).take(10).collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let (a, b) = if ids[i] < ids[j] {
                (ids[i], ids[j])
            } else {
                (ids[j], ids[i])
            };
            let key = format!("{}\t{}", a, b);
            *counts.entry(key).or_insert(0) += 1;
        }
    }

    if let Ok(json) = serde_json::to_string(&counts) {
        let _ = std::fs::write(&co_path, json);
    }
}

/// Build clusters from co-occurrence data using union-find.
/// Pairs with count >= min_cooccurrence get merged.
pub fn build_symbol_clusters(
    path: &str,
    min_cooccurrence: u32,
) -> Result<HashMap<String, Vec<String>>> {
    let co_path = cooccurrence_path(path);
    if !co_path.exists() {
        return Ok(HashMap::new());
    }

    let counts: HashMap<String, u32> = serde_json::from_str(&std::fs::read_to_string(&co_path)?)?;

    let mut all_ids: Vec<String> = Vec::new();
    let mut id_index: HashMap<String, usize> = HashMap::new();

    for (key, &count) in &counts {
        if count < min_cooccurrence {
            continue;
        }
        let parts: Vec<&str> = key.split('\t').collect();
        if parts.len() != 2 {
            continue;
        }
        for &part in &parts {
            if !id_index.contains_key(part) {
                id_index.insert(part.to_string(), all_ids.len());
                all_ids.push(part.to_string());
            }
        }
    }

    if all_ids.len() < 2 {
        return Ok(HashMap::new());
    }

    let n = all_ids.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] != i {
            parent[i] = find(parent, parent[i]);
        }
        parent[i]
    }

    for (key, &count) in &counts {
        if count < min_cooccurrence {
            continue;
        }
        let parts: Vec<&str> = key.split('\t').collect();
        if parts.len() != 2 {
            continue;
        }
        if let (Some(&a), Some(&b)) = (id_index.get(parts[0]), id_index.get(parts[1])) {
            let pa = find(&mut parent, a);
            let pb = find(&mut parent, b);
            if pa != pb {
                parent[pa] = pb;
            }
        }
    }

    let mut raw_clusters: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, id) in all_ids.iter().enumerate().take(n) {
        let root = find(&mut parent, i);
        raw_clusters.entry(root).or_default().push(id.clone());
    }

    // Only keep clusters with 2+ members, key by first member
    let mut clusters: HashMap<String, Vec<String>> = HashMap::new();
    for (_root, members) in raw_clusters {
        if members.len() >= 2 {
            let key = members[0].clone();
            clusters.insert(key, members);
        }
    }

    // Persist clusters
    let cl_path = cluster_path(path);
    if let Ok(json) = serde_json::to_string(&clusters) {
        let _ = std::fs::write(&cl_path, json);
    }

    Ok(clusters)
}

/// Load persisted clusters. Returns map: symbol_id → all peers in its cluster.
fn load_cluster_peers(path: &str) -> HashMap<String, Vec<String>> {
    let cl_path = cluster_path(path);
    if !cl_path.exists() {
        return HashMap::new();
    }

    let clusters: HashMap<String, Vec<String>> = std::fs::read_to_string(&cl_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut peer_map: HashMap<String, Vec<String>> = HashMap::new();
    for members in clusters.values() {
        for member in members {
            peer_map.insert(member.clone(), members.clone());
        }
    }
    peer_map
}

/// Boost scores for symbols that are cluster peers of already-matched symbols.
fn apply_cluster_boost(items: &mut [ScoredItem], path: &str) {
    let peer_map = load_cluster_peers(path);
    if peer_map.is_empty() {
        return;
    }

    let matched_ids: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();

    let mut peer_ids: HashSet<String> = HashSet::new();
    for id in &matched_ids {
        if let Some(peers) = peer_map.get(id) {
            for peer in peers {
                if !matched_ids.contains(peer) {
                    peer_ids.insert(peer.clone());
                }
            }
        }
    }

    // Boost existing items that are peers
    for item in items.iter_mut() {
        if peer_ids.contains(&item.id) {
            item.score = (item.score + 0.1).min(1.0);
        }
    }
}

fn format_epoch(epoch: i64) -> String {
    let days = epoch / 86400;
    let y = (days * 400 / 146097) + 1970;
    let rem = days - ((y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400);
    let (y, rem) = if rem < 0 {
        let y = y - 1;
        let r = days - ((y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400);
        (y, r)
    } else {
        (y, rem)
    };
    let is_leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = if is_leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0;
    let mut d = rem;
    for (i, &days_in_month) in months.iter().enumerate() {
        if d < days_in_month {
            m = i + 1;
            break;
        }
        d -= days_in_month;
    }
    if m == 0 {
        m = 12;
    }
    format!("{:04}-{:02}-{:02}", y, m, d + 1)
}
