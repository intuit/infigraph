use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::GraphBackend;

/// Node data for the visualization.
struct VizNode {
    id: String,
    label: String,
    kind: String,
    file: String,
    start_line: String,
    end_line: String,
}

/// Edge data for the visualization.
struct VizEdge {
    from: String,
    to: String,
    rel_type: String,
}

/// Generate a self-contained HTML visualization of the code graph and write it to `output_path`.
///
/// The HTML uses vis.js loaded from a CDN. Nodes are colored by kind, edges by relationship type.
/// Features a left sidebar with search, filters, and file tree; a right panel with node details
/// including callers/callees; and professional dark-themed styling.
pub fn generate_html(backend: &dyn GraphBackend, output_path: &Path) -> Result<String> {
    let nodes = query_nodes(backend)?;
    let edges = query_edges(backend)?;

    let nodes_json = build_nodes_json(&nodes);
    let edges_json = build_edges_json(&edges);

    let html = HTML_TEMPLATE
        .replace("/*__NODES_DATA__*/", &nodes_json)
        .replace("/*__EDGES_DATA__*/", &edges_json);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, html.as_bytes())
        .with_context(|| format!("failed to write {}", output_path.display()))?;

    Ok(output_path.to_string_lossy().to_string())
}

/// Generate a focused subgraph HTML visualization centered on a single symbol.
///
/// Traverses `depth` hops of CALLS/INHERITS/CONTAINS edges in both directions,
/// collecting only the reachable nodes and edges. The root symbol is highlighted.
pub fn generate_symbol_html(
    backend: &dyn GraphBackend,
    symbol_id: &str,
    depth: u32,
    output_path: &Path,
) -> Result<String> {
    let (nodes, edges) = query_symbol_subgraph(backend, symbol_id, depth)?;

    if nodes.is_empty() {
        anyhow::bail!("symbol not found: {symbol_id}");
    }

    let nodes_json = build_nodes_json_with_focus(&nodes, symbol_id);
    let edges_json = build_edges_json(&edges);

    let html = HTML_TEMPLATE
        .replace("/*__NODES_DATA__*/", &nodes_json)
        .replace("/*__EDGES_DATA__*/", &edges_json);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, html.as_bytes())
        .with_context(|| format!("failed to write {}", output_path.display()))?;

    Ok(output_path.to_string_lossy().to_string())
}

fn query_nodes(backend: &dyn GraphBackend) -> Result<Vec<VizNode>> {
    let rows = backend.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line",
    )?;

    let mut nodes = Vec::with_capacity(rows.len());
    for row in &rows {
        if row.len() >= 6 {
            nodes.push(VizNode {
                id: row[0].clone(),
                label: row[1].clone(),
                kind: row[2].clone(),
                file: row[3].clone(),
                start_line: row[4].clone(),
                end_line: row[5].clone(),
            });
        }
    }
    Ok(nodes)
}

fn query_edges(backend: &dyn GraphBackend) -> Result<Vec<VizEdge>> {
    let mut edges = Vec::new();

    // CALLS edges
    let call_rows = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &call_rows {
        if row.len() >= 2 {
            edges.push(VizEdge {
                from: row[0].clone(),
                to: row[1].clone(),
                rel_type: "CALLS".to_string(),
            });
        }
    }

    // INHERITS edges
    let inherit_rows =
        backend.raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &inherit_rows {
        if row.len() >= 2 {
            edges.push(VizEdge {
                from: row[0].clone(),
                to: row[1].clone(),
                rel_type: "INHERITS".to_string(),
            });
        }
    }

    // CONTAINS edges (Module -> Symbol)
    let contains_rows =
        backend.raw_query("MATCH (m:Module)-[:CONTAINS]->(s:Symbol) RETURN m.id, s.id")?;
    for row in &contains_rows {
        if row.len() >= 2 {
            edges.push(VizEdge {
                from: row[0].clone(),
                to: row[1].clone(),
                rel_type: "CONTAINS".to_string(),
            });
        }
    }

    Ok(edges)
}

/// BFS from `symbol_id` up to `depth` hops, following CALLS/INHERITS in both directions
/// and CONTAINS outward. Returns only reachable nodes + edges between them.
fn query_symbol_subgraph(
    backend: &dyn GraphBackend,
    symbol_id: &str,
    depth: u32,
) -> Result<(Vec<VizNode>, Vec<VizEdge>)> {
    use std::collections::{HashSet, VecDeque};

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((symbol_id.to_string(), 0));
    visited.insert(symbol_id.to_string());

    while let Some((id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        let esc = id.replace('\'', "\\'");

        // Outgoing CALLS
        let q = format!("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE a.id = '{esc}' RETURN b.id");
        if let Ok(rows) = backend.raw_query(&q) {
            for row in &rows {
                if let Some(nid) = row.first() {
                    if visited.insert(nid.clone()) {
                        queue.push_back((nid.clone(), hop + 1));
                    }
                }
            }
        }
        // Incoming CALLS (callers)
        let q = format!("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE b.id = '{esc}' RETURN a.id");
        if let Ok(rows) = backend.raw_query(&q) {
            for row in &rows {
                if let Some(nid) = row.first() {
                    if visited.insert(nid.clone()) {
                        queue.push_back((nid.clone(), hop + 1));
                    }
                }
            }
        }
        // INHERITS both directions
        let q = format!("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) WHERE a.id = '{esc}' OR b.id = '{esc}' RETURN a.id, b.id");
        if let Ok(rows) = backend.raw_query(&q) {
            for row in &rows {
                for nid in row {
                    if visited.insert(nid.clone()) {
                        queue.push_back((nid.clone(), hop + 1));
                    }
                }
            }
        }
    }

    // Fetch node details for all visited IDs
    let mut nodes = Vec::new();
    for id in &visited {
        let esc = id.replace('\'', "\\'");
        let q = format!(
            "MATCH (s:Symbol) WHERE s.id = '{esc}' RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line"
        );
        if let Ok(rows) = backend.raw_query(&q) {
            for row in &rows {
                if row.len() >= 6 {
                    nodes.push(VizNode {
                        id: row[0].clone(),
                        label: row[1].clone(),
                        kind: row[2].clone(),
                        file: row[3].clone(),
                        start_line: row[4].clone(),
                        end_line: row[5].clone(),
                    });
                }
            }
        }
    }

    // Fetch only edges between visited nodes
    let mut edges = Vec::new();
    let id_set: HashSet<&str> = visited.iter().map(|s| s.as_str()).collect();

    let call_rows = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &call_rows {
        if row.len() >= 2 && id_set.contains(row[0].as_str()) && id_set.contains(row[1].as_str()) {
            edges.push(VizEdge {
                from: row[0].clone(),
                to: row[1].clone(),
                rel_type: "CALLS".to_string(),
            });
        }
    }
    let inherit_rows =
        backend.raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &inherit_rows {
        if row.len() >= 2 && id_set.contains(row[0].as_str()) && id_set.contains(row[1].as_str()) {
            edges.push(VizEdge {
                from: row[0].clone(),
                to: row[1].clone(),
                rel_type: "INHERITS".to_string(),
            });
        }
    }

    Ok((nodes, edges))
}

/// Like `build_nodes_json` but marks the focus symbol with a larger size and distinct border.
fn build_nodes_json_with_focus(nodes: &[VizNode], focus_id: &str) -> String {
    let entries: Vec<String> = nodes
        .iter()
        .map(|n| {
            let color = match n.kind.as_str() {
                "Function" => "#4A90D9",
                "Class" => "#27AE60",
                "Method" => "#17A2B8",
                "Test" => "#E67E22",
                "Variable" | "Constant" => "#95A5A6",
                "Struct" | "Interface" | "Trait" => "#27AE60",
                "Enum" => "#16A085",
                "Module" => "#F39C12",
                "Section" => "#8E44AD",
                _ => "#BDC3C7",
            };
            if n.id == focus_id {
                format!(
                    "{{id:\"{}\",label:\"{}\",kind:\"{}\",file:\"{}\",startLine:\"{}\",endLine:\"{}\",color:\"{}\",size:30,borderWidth:4,borderColor:\"#FFD700\"}}",
                    json_escape(&n.id), json_escape(&n.label), json_escape(&n.kind),
                    json_escape(&n.file), json_escape(&n.start_line), json_escape(&n.end_line),
                    color,
                )
            } else {
                format!(
                    r#"{{id:"{}",label:"{}",kind:"{}",file:"{}",startLine:"{}",endLine:"{}",color:"{}"}}"#,
                    json_escape(&n.id), json_escape(&n.label), json_escape(&n.kind),
                    json_escape(&n.file), json_escape(&n.start_line), json_escape(&n.end_line),
                    color,
                )
            }
        })
        .collect();
    entries.join(",\n")
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn build_nodes_json(nodes: &[VizNode]) -> String {
    let entries: Vec<String> = nodes
        .iter()
        .map(|n| {
            let color = match n.kind.as_str() {
                "Function" => "#4A90D9",
                "Class" => "#27AE60",
                "Method" => "#17A2B8",
                "Test" => "#E67E22",
                "Variable" | "Constant" => "#95A5A6",
                "Struct" | "Interface" | "Trait" => "#27AE60",
                "Enum" => "#16A085",
                "Module" => "#F39C12",
                "Section" => "#8E44AD",
                _ => "#BDC3C7",
            };
            format!(
                r#"{{id:"{}",label:"{}",kind:"{}",file:"{}",startLine:"{}",endLine:"{}",color:"{}"}}"#,
                json_escape(&n.id),
                json_escape(&n.label),
                json_escape(&n.kind),
                json_escape(&n.file),
                json_escape(&n.start_line),
                json_escape(&n.end_line),
                color,
            )
        })
        .collect();

    entries.join(",\n")
}

fn build_edges_json(edges: &[VizEdge]) -> String {
    let entries: Vec<String> = edges
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let color = match e.rel_type.as_str() {
                "CALLS" => "#3498DB",
                "INHERITS" => "#E74C3C",
                "CONTAINS" => "#7F8C8D",
                _ => "#95A5A6",
            };
            format!(
                r#"{{id:"e{}",from:"{}",to:"{}",relType:"{}",color:"{}"}}"#,
                i,
                json_escape(&e.from),
                json_escape(&e.to),
                json_escape(&e.rel_type),
                color,
            )
        })
        .collect();

    entries.join(",\n")
}

const HTML_TEMPLATE: &str = include_str!("template.html");
