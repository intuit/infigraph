use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::open_prism;

pub fn tool_export_graph(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let format = args
        .get("format")
        .and_then(|f| f.as_str())
        .context("missing 'format' argument")?;

    let backend = prism.backend().context("not initialized")?;

    let mut buf = Vec::new();
    match format {
        "cypher" => infigraph_core::export::export_cypher(backend, &mut buf)?,
        "graphml" => infigraph_core::export::export_graphml(backend, &mut buf)?,
        "json" => infigraph_core::export::export_json(backend, &mut buf)?,
        _ => anyhow::bail!(
            "unknown export format '{}'. Supported: cypher, graphml, json",
            format
        ),
    }

    String::from_utf8(buf).context("export produced invalid UTF-8")
}

pub fn tool_visualize(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let output_path = prism.root().join(".infigraph").join("graph.html");
    let path = infigraph_core::viz::generate_html(backend, &output_path)?;
    Ok(format!("Graph visualization written to: {}", path))
}

pub fn tool_visualize_symbol(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as u32;

    let backend = prism.backend().context("not initialized")?;

    let safe_name: String = symbol_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let output_path = prism
        .root()
        .join(".infigraph")
        .join(format!("symbol-{safe_name}.html"));
    let path = infigraph_core::viz::generate_symbol_html(backend, symbol_id, depth, &output_path)?;
    Ok(format!("Symbol subgraph visualization written to: {path}"))
}
