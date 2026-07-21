use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::graph::GraphBackend;

use super::interprocedural::detect_interprocedural_taint;

#[derive(Debug, Clone, Serialize)]
pub struct PathTraversalFlow {
    pub kind: &'static str,
    pub source_symbol: String,
    pub sink_symbol: String,
    pub source_kind: String,
    pub depth: u32,
    pub call_chain: Vec<String>,
    pub sanitized: bool,
}

static PATH_TRAVERSAL_SOURCE_KINDS: &[&str] = &["HttpParam", "HttpBody", "HttpHeader", "UserInput"];

static PATH_TRAVERSAL_SINK_CATEGORIES: &[&str] = &["PathTraversal"];

static PATH_TRAVERSAL_SANITIZERS: &[&str] = &[
    "realpath(",
    "abspath(",
    "canonicalize(",
    "path.resolve(",
    "secure_filename(",
    "os.path.basename(",
    "filepath.Clean(",
    "os.path.normpath(",
    "Path.normalize(",
];

pub fn detect_path_traversal(
    backend: &dyn GraphBackend,
    root: &Path,
    max_depth: u32,
) -> Result<Vec<PathTraversalFlow>> {
    let mut results = Vec::new();

    let intra_flows = super::detect_taint_flows(backend, root)?;
    for flow in &intra_flows {
        if flow.sink_category == "PathTraversal"
            && PATH_TRAVERSAL_SOURCE_KINDS.contains(&flow.source_kind.as_str())
        {
            results.push(PathTraversalFlow {
                kind: "intra-procedural",
                source_symbol: flow.symbol_id.clone(),
                sink_symbol: flow.symbol_id.clone(),
                source_kind: flow.source_kind.clone(),
                depth: 0,
                call_chain: vec![flow.symbol_id.clone()],
                sanitized: flow.sanitized,
            });
        }
    }

    // Inter-procedural: trace across function boundaries
    let inter_flows = detect_interprocedural_taint(backend, root, max_depth)?;
    for flow in &inter_flows {
        if PATH_TRAVERSAL_SINK_CATEGORIES.contains(&flow.sink_category.as_str())
            && PATH_TRAVERSAL_SOURCE_KINDS.contains(&flow.source_kind.as_str())
        {
            let sanitized = check_chain_sanitized(backend, root, &flow.call_chain);
            results.push(PathTraversalFlow {
                kind: "inter-procedural",
                source_symbol: flow.source_symbol.clone(),
                sink_symbol: flow.sink_symbol.clone(),
                source_kind: flow.source_kind.clone(),
                depth: flow.depth,
                call_chain: flow.call_chain.clone(),
                sanitized,
            });
        }
    }

    Ok(results)
}

fn check_chain_sanitized(backend: &dyn GraphBackend, root: &Path, chain: &[String]) -> bool {
    for symbol_id in chain {
        let result = backend.raw_query(&format!(
            "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.file, s.start_line, s.end_line",
            crate::escape_str(symbol_id)
        ));

        if let Ok(result) = result {
            for row in result {
                if row.len() < 3 {
                    continue;
                }
                let file = row[0].to_string();
                let start: usize = row[1].to_string().parse().unwrap_or(0);
                let end: usize = row[2].to_string().parse().unwrap_or(0);

                if let Ok(content) = std::fs::read_to_string(root.join(&file)) {
                    let lines: Vec<&str> = content.lines().collect();
                    let start_idx = start.saturating_sub(1);
                    let end_idx = end.min(lines.len());

                    for line in &lines[start_idx..end_idx] {
                        let lower = line.to_lowercase();
                        for &pat in PATH_TRAVERSAL_SANITIZERS {
                            if lower.contains(&pat.to_lowercase()) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    false
}

pub fn format_path_traversal(flows: &[PathTraversalFlow]) -> String {
    if flows.is_empty() {
        return "No multi-layer path traversal vulnerabilities detected.".to_string();
    }

    let active: Vec<_> = flows.iter().filter(|f| !f.sanitized).collect();
    let sanitized = flows.len() - active.len();

    let mut out = format!(
        "Path traversal analysis: {} total ({} active, {} sanitized)\n\n",
        flows.len(),
        active.len(),
        sanitized
    );

    if !active.is_empty() {
        let intra: Vec<_> = active
            .iter()
            .filter(|f| f.kind == "intra-procedural")
            .collect();
        let inter: Vec<_> = active
            .iter()
            .filter(|f| f.kind == "inter-procedural")
            .collect();

        if !intra.is_empty() {
            out.push_str(&format!("## Intra-procedural ({} flows)\n", intra.len()));
            for f in &intra {
                out.push_str(&format!(
                    "  {} ({}) — same function\n",
                    f.source_symbol, f.source_kind
                ));
            }
            out.push('\n');
        }

        if !inter.is_empty() {
            out.push_str(&format!("## Inter-procedural ({} flows)\n", inter.len()));
            for f in &inter {
                out.push_str(&format!(
                    "  {} -> {} ({}, depth: {})\n    Chain: {}\n",
                    f.source_symbol,
                    f.sink_symbol,
                    f.source_kind,
                    f.depth,
                    f.call_chain.join(" -> ")
                ));
            }
            out.push('\n');
        }
    }

    if sanitized > 0 {
        out.push_str(&format!(
            "\n--- {} flows sanitized (path normalization detected) ---\n",
            sanitized
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_empty() {
        let result = format_path_traversal(&[]);
        assert!(result.contains("No multi-layer"));
    }

    #[test]
    fn test_format_with_flows() {
        let flows = vec![
            PathTraversalFlow {
                kind: "intra-procedural",
                source_symbol: "app.py::download".to_string(),
                sink_symbol: "app.py::download".to_string(),
                source_kind: "HttpParam".to_string(),
                depth: 0,
                call_chain: vec!["app.py::download".to_string()],
                sanitized: false,
            },
            PathTraversalFlow {
                kind: "inter-procedural",
                source_symbol: "api.py::get_file".to_string(),
                sink_symbol: "storage.py::read_file".to_string(),
                source_kind: "HttpParam".to_string(),
                depth: 1,
                call_chain: vec![
                    "api.py::get_file".to_string(),
                    "storage.py::read_file".to_string(),
                ],
                sanitized: false,
            },
        ];
        let result = format_path_traversal(&flows);
        assert!(result.contains("Intra-procedural"));
        assert!(result.contains("Inter-procedural"));
        assert!(result.contains("2 active"));
    }

    #[test]
    fn test_sanitized_flow() {
        let flows = vec![PathTraversalFlow {
            kind: "intra-procedural",
            source_symbol: "app.py::download".to_string(),
            sink_symbol: "app.py::download".to_string(),
            source_kind: "HttpParam".to_string(),
            depth: 0,
            call_chain: vec!["app.py::download".to_string()],
            sanitized: true,
        }];
        let result = format_path_traversal(&flows);
        assert!(result.contains("0 active"));
        assert!(result.contains("1 sanitized"));
    }
}
