use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_query(root: &Path, cypher: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(cypher)?;
    for row in &rows {
        println!("{}", row.join(" | "));
    }
    if rows.is_empty() {
        println!("(no results)");
    }
    Ok(())
}

pub(crate) fn cmd_export(root: &Path, format: &str, output: Option<std::path::PathBuf>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    match output {
        Some(path) => {
            let file = std::fs::File::create(&path)
                .with_context(|| format!("failed to create output file: {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            export_to_writer(&gq, format, &mut writer)?;
            println!("Exported {} to {}", format, path.display());
        }
        None => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            export_to_writer(&gq, format, &mut writer)?;
        }
    }

    Ok(())
}

pub(crate) fn cmd_dead_code(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN s.name, s.kind, s.file ORDER BY s.file, s.name",
    )?;

    if rows.is_empty() {
        println!("No dead code found (all functions/methods have callers).");
        return Ok(());
    }

    let entry_points = ["main", "__init__", "setUp", "tearDown"];
    let dead: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| !entry_points.contains(&row[0].as_str()))
        .collect();

    if dead.is_empty() {
        println!("No dead code found (all non-entry-point functions have callers).");
        return Ok(());
    }

    println!("Potentially dead code ({} symbols):", dead.len());
    let mut current_file = "";
    for row in &dead {
        if row[2] != current_file {
            current_file = &row[2];
            println!("\n  {}:", current_file);
        }
        println!("    {:>8} {}", row[1], row[0]);
    }

    Ok(())
}

pub(crate) fn cmd_impact(root: &Path, symbol: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let impacted = gq.transitive_impact(symbol, depth)?;

    if impacted.is_empty() {
        println!("No symbols affected by changes to '{}'", symbol);
        return Ok(());
    }

    println!(
        "Symbols affected by changes to '{}' (depth={}):",
        symbol, depth
    );
    for row in &impacted {
        println!("  {:>8} {:30} {}", row.kind, row.name, row.file);
    }

    Ok(())
}

fn export_to_writer<W: std::io::Write>(
    gq: &infigraph_core::graph::GraphQuery,
    format: &str,
    writer: &mut W,
) -> anyhow::Result<()> {
    match format {
        "cypher" => infigraph_core::export::export_cypher(gq, writer),
        "graphml" => infigraph_core::export::export_graphml(gq, writer),
        "json" => infigraph_core::export::export_json(gq, writer),
        _ => anyhow::bail!(
            "unknown export format '{}'. Supported formats: cypher, graphml, json",
            format
        ),
    }
}
