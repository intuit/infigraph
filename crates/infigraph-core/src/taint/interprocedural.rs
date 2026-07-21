use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::graph::GraphBackend;

use super::sinks::TAINT_SINKS;
use super::sources::TAINT_SOURCES;
use super::{FuncInfo, SourceCache};

#[derive(Debug, Clone, Serialize)]
pub struct InterProcTaintFlow {
    pub source_symbol: String,
    pub sink_symbol: String,
    pub source_kind: String,
    pub sink_kind: String,
    pub sink_category: String,
    pub call_chain: Vec<String>,
    pub depth: u32,
}

pub fn detect_interprocedural_taint(
    backend: &dyn GraphBackend,
    root: &Path,
    max_depth: u32,
) -> Result<Vec<InterProcTaintFlow>> {
    let source_functions = find_source_functions(backend, root)?;
    let sink_functions = find_sink_functions(backend, root)?;

    // Step 3: BFS from source functions through CALLS edges to sink functions
    let mut flows = Vec::new();

    for (src_sym, src_kind) in &source_functions {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, Vec<String>, u32)> = VecDeque::new();

        visited.insert(src_sym.clone());
        queue.push_back((src_sym.clone(), vec![src_sym.clone()], 0));

        while let Some((current, chain, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }

            // Check if current function is a sink
            if let Some((sink_kind, sink_cat)) = sink_functions.get(&current) {
                if current != *src_sym {
                    flows.push(InterProcTaintFlow {
                        source_symbol: src_sym.clone(),
                        sink_symbol: current.clone(),
                        source_kind: src_kind.clone(),
                        sink_kind: sink_kind.clone(),
                        sink_category: sink_cat.clone(),
                        call_chain: chain.clone(),
                        depth,
                    });
                }
            }

            // Traverse callees
            if let Ok(callees) = backend.callees_of(&current) {
                for callee in callees {
                    if !visited.contains(&callee) {
                        visited.insert(callee.clone());
                        let mut new_chain = chain.clone();
                        new_chain.push(callee.clone());
                        queue.push_back((callee, new_chain, depth + 1));
                    }
                }
            }
        }
    }

    Ok(flows)
}

pub fn detect_interprocedural_taint_with_cache(
    backend: &dyn GraphBackend,
    functions: &[FuncInfo],
    cache: &SourceCache,
    max_depth: u32,
) -> Result<Vec<InterProcTaintFlow>> {
    let adj = load_call_adjacency(backend)?;

    let (source_functions, sink_functions) = find_sources_and_sinks_from_cache(functions, cache);

    let mut flows = Vec::new();
    for (src_sym, src_kind) in &source_functions {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, Vec<String>, u32)> = VecDeque::new();
        visited.insert(src_sym.clone());
        queue.push_back((src_sym.clone(), vec![src_sym.clone()], 0));

        while let Some((current, chain, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }
            if let Some((sink_kind, sink_cat)) = sink_functions.get(&current) {
                if current != *src_sym {
                    flows.push(InterProcTaintFlow {
                        source_symbol: src_sym.clone(),
                        sink_symbol: current.clone(),
                        source_kind: src_kind.clone(),
                        sink_kind: sink_kind.clone(),
                        sink_category: sink_cat.clone(),
                        call_chain: chain.clone(),
                        depth,
                    });
                }
            }
            if let Some(callees) = adj.get(&current) {
                for callee in callees {
                    if !visited.contains(callee) {
                        visited.insert(callee.clone());
                        let mut new_chain = chain.clone();
                        new_chain.push(callee.clone());
                        queue.push_back((callee.clone(), new_chain, depth + 1));
                    }
                }
            }
        }
    }
    Ok(flows)
}

fn load_call_adjacency(backend: &dyn GraphBackend) -> Result<HashMap<String, Vec<String>>> {
    let result = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for row in result {
        if row.len() >= 2 {
            adj.entry(row[0].to_string())
                .or_default()
                .push(row[1].to_string());
        }
    }
    Ok(adj)
}

#[allow(clippy::type_complexity)]
fn find_sources_and_sinks_from_cache(
    functions: &[FuncInfo],
    cache: &SourceCache,
) -> (Vec<(String, String)>, HashMap<String, (String, String)>) {
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut sinks: HashMap<String, (String, String)> = HashMap::new();

    for func in functions {
        let lines = match cache.get(&func.file) {
            Some(l) => l,
            None => continue,
        };
        let start_idx = (func.start_line as usize).saturating_sub(1);
        let end_idx = (func.end_line as usize).min(lines.len());
        if start_idx >= end_idx {
            continue;
        }

        let mut found_source = false;
        let mut found_sink = false;

        for line in &lines[start_idx..end_idx] {
            let lower = line.to_lowercase();

            if !found_source {
                for src in super::sources::TAINT_SOURCES {
                    for &pat in src.patterns {
                        if lower.contains(&pat.to_lowercase()) {
                            sources.push((func.id.clone(), src.kind.to_string()));
                            found_source = true;
                            break;
                        }
                    }
                    if found_source {
                        break;
                    }
                }
            }

            if !found_sink {
                for sink in TAINT_SINKS {
                    for &pat in sink.patterns {
                        if lower.contains(&pat.to_lowercase()) {
                            sinks.insert(
                                func.id.clone(),
                                (sink.kind.to_string(), sink.category.to_string()),
                            );
                            found_sink = true;
                            break;
                        }
                    }
                    if found_sink {
                        break;
                    }
                }
            }

            if found_source && found_sink {
                break;
            }
        }
    }

    sources.dedup_by(|a, b| a.0 == b.0);
    (sources, sinks)
}

fn find_source_functions(backend: &dyn GraphBackend, root: &Path) -> Result<Vec<(String, String)>> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND s.file IS NOT NULL RETURN s.id, s.file, s.start_line, s.end_line")?;

    let mut sources = Vec::new();
    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();

    for row in result {
        if row.len() < 4 {
            continue;
        }
        let id = row[0].to_string();
        let file = row[1].to_string();
        let start: usize = row[2].to_string().parse().unwrap_or(0);
        let end: usize = row[3].to_string().parse().unwrap_or(0);
        if start == 0 || end <= start {
            continue;
        }

        let lines = file_cache.entry(file.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(&file))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        });

        let start_idx = start.saturating_sub(1);
        let end_idx = end.min(lines.len());
        if start_idx >= end_idx {
            continue;
        }

        for line in &lines[start_idx..end_idx] {
            let lower = line.to_lowercase();
            for src in TAINT_SOURCES {
                for &pat in src.patterns {
                    if lower.contains(&pat.to_lowercase()) {
                        sources.push((id.clone(), src.kind.to_string()));
                        break;
                    }
                }
                if sources.last().map(|(s, _)| s == &id).unwrap_or(false) {
                    break;
                }
            }
            if sources.last().map(|(s, _)| s == &id).unwrap_or(false) {
                break;
            }
        }
    }

    sources.dedup_by(|a, b| a.0 == b.0);
    Ok(sources)
}

fn find_sink_functions(
    backend: &dyn GraphBackend,
    root: &Path,
) -> Result<HashMap<String, (String, String)>> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND s.file IS NOT NULL RETURN s.id, s.file, s.start_line, s.end_line")?;

    let mut sinks: HashMap<String, (String, String)> = HashMap::new();
    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();

    for row in result {
        if row.len() < 4 {
            continue;
        }
        let id = row[0].to_string();
        let file = row[1].to_string();
        let start: usize = row[2].to_string().parse().unwrap_or(0);
        let end: usize = row[3].to_string().parse().unwrap_or(0);
        if start == 0 || end <= start {
            continue;
        }

        let lines = file_cache.entry(file.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(&file))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        });

        let start_idx = start.saturating_sub(1);
        let end_idx = end.min(lines.len());
        if start_idx >= end_idx {
            continue;
        }

        'outer: for line in &lines[start_idx..end_idx] {
            let lower = line.to_lowercase();
            for sink in TAINT_SINKS {
                for &pat in sink.patterns {
                    if lower.contains(&pat.to_lowercase()) {
                        sinks.insert(
                            id.clone(),
                            (sink.kind.to_string(), sink.category.to_string()),
                        );
                        break 'outer;
                    }
                }
            }
        }
    }

    Ok(sinks)
}

pub fn format_interprocedural_flows(flows: &[InterProcTaintFlow]) -> String {
    if flows.is_empty() {
        return "No inter-procedural taint flows detected.".to_string();
    }

    let mut out = format!("Inter-procedural taint flows: {} total\n\n", flows.len());

    let mut by_category: std::collections::BTreeMap<&str, Vec<&InterProcTaintFlow>> =
        std::collections::BTreeMap::new();
    for f in flows {
        by_category.entry(&f.sink_category).or_default().push(f);
    }

    for (cat, items) in &by_category {
        out.push_str(&format!("## {} ({} flows)\n", cat, items.len()));
        for f in items {
            out.push_str(&format!(
                "  {} ({}) -> {} ({}) [depth: {}]\n",
                f.source_symbol, f.source_kind, f.sink_symbol, f.sink_kind, f.depth
            ));
            out.push_str("    Chain: ");
            out.push_str(&f.call_chain.join(" -> "));
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_empty() {
        let result = format_interprocedural_flows(&[]);
        assert!(result.contains("No inter-procedural"));
    }

    #[test]
    fn test_format_with_flows() {
        let flows = vec![InterProcTaintFlow {
            source_symbol: "app.py::handle_request".to_string(),
            sink_symbol: "db.py::run_query".to_string(),
            source_kind: "HttpParam".to_string(),
            sink_kind: "SqlQuery".to_string(),
            sink_category: "SqlInjection".to_string(),
            call_chain: vec![
                "app.py::handle_request".to_string(),
                "db.py::run_query".to_string(),
            ],
            depth: 1,
        }];
        let result = format_interprocedural_flows(&flows);
        assert!(result.contains("SqlInjection"));
        assert!(result.contains("handle_request"));
        assert!(result.contains("run_query"));
        assert!(result.contains("depth: 1"));
    }
}
