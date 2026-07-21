use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;

use crate::graph::GraphBackend;

/// Generate a Mermaid sequenceDiagram from the call graph starting at `entry_symbol_id`.
///
/// Participants = unique files. Messages = CALLS edges. BFS bounded by `depth`.
/// Skips self-calls (same file calling same file) unless they cross functions.
pub fn generate_sequence_mermaid(
    backend: &dyn GraphBackend,
    entry_symbol_id: &str,
    depth: u32,
) -> Result<String> {
    // BFS over outgoing CALLS edges only (directed: entry → callees)
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    // edges in BFS order: (caller_id, callee_id)
    let mut edges: Vec<(String, String)> = Vec::new();

    queue.push_back((entry_symbol_id.to_string(), 0));
    visited.insert(entry_symbol_id.to_string());

    while let Some((id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        let esc = id.replace('\'', "\\'");
        let q = format!("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE a.id = '{esc}' RETURN b.id");
        if let Ok(rows) = backend.raw_query(&q) {
            for row in &rows {
                if let Some(callee_id) = row.first() {
                    edges.push((id.clone(), callee_id.clone()));
                    if visited.insert(callee_id.clone()) {
                        queue.push_back((callee_id.clone(), hop + 1));
                    }
                }
            }
        }
    }

    if edges.is_empty() {
        return Ok(format!(
            "sequenceDiagram\n    note over {}: no outgoing calls found\n",
            participant_name(entry_symbol_id)
        ));
    }

    // Fetch name + file for all visited symbols
    let mut sym_info: HashMap<String, (String, String)> = HashMap::new(); // id -> (name, file)
    for id in &visited {
        let esc = id.replace('\'', "\\'");
        let q = format!("MATCH (s:Symbol) WHERE s.id = '{esc}' RETURN s.name, s.file");
        if let Ok(rows) = backend.raw_query(&q) {
            if let Some(row) = rows.first() {
                if row.len() >= 2 {
                    sym_info.insert(id.clone(), (row[0].clone(), row[1].clone()));
                }
            }
        }
    }

    // Collect unique participants (files), preserving encounter order
    let mut participants: Vec<String> = Vec::new();
    let mut seen_parts: HashSet<String> = HashSet::new();

    // Entry symbol's file first
    if let Some((_, file)) = sym_info.get(entry_symbol_id) {
        let p = file_to_participant(file);
        if seen_parts.insert(p.clone()) {
            participants.push(p);
        }
    }
    for (caller, callee) in &edges {
        for id in [caller, callee] {
            if let Some((_, file)) = sym_info.get(id) {
                let p = file_to_participant(file);
                if seen_parts.insert(p.clone()) {
                    participants.push(p);
                }
            }
        }
    }

    let mut out = String::from("sequenceDiagram\n");
    for p in &participants {
        out.push_str(&format!("    participant {p}\n"));
    }
    out.push('\n');

    for (caller_id, callee_id) in &edges {
        let caller_file = sym_info
            .get(caller_id)
            .map(|(_, f)| f.as_str())
            .unwrap_or(caller_id);
        let callee_file = sym_info
            .get(callee_id)
            .map(|(_, f)| f.as_str())
            .unwrap_or(callee_id);
        let caller_part = file_to_participant(caller_file);
        let callee_part = file_to_participant(callee_file);
        let callee_name = sym_info
            .get(callee_id)
            .map(|(n, _)| n.as_str())
            .unwrap_or(callee_id);
        out.push_str(&format!(
            "    {caller_part}->>{callee_part}: {callee_name}()\n"
        ));
    }

    Ok(out)
}

/// Shorten a file path to a readable participant label.
/// `crates/infigraph-core/src/graph/store.rs` → `store`
fn file_to_participant(file: &str) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    // Mermaid participant names can't have spaces or dots
    stem.replace([' ', '.', '-'], "_")
}

fn participant_name(symbol_id: &str) -> String {
    file_to_participant(symbol_id.split("::").next().unwrap_or(symbol_id))
}
