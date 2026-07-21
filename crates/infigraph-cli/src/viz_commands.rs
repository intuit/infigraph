use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_visualize(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    let output_path = prism.root().join(".infigraph").join("graph.html");
    let path = infigraph_core::viz::generate_html(backend, &output_path)?;
    println!("Graph visualization written to: {}", path);
    Ok(())
}

pub(crate) fn cmd_visualize_symbol(root: &Path, symbol_id: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

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
    println!("Symbol subgraph written to: {}", path);
    Ok(())
}

pub(crate) fn cmd_routes(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    let routes = infigraph_core::routes::detect_routes(backend)?;
    println!("{}", infigraph_core::routes::format_routes(&routes));
    Ok(())
}
