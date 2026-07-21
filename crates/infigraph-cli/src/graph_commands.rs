use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_query(root: &Path, cypher: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let rows = backend.raw_query(cypher)?;
    for row in &rows {
        println!("{}", row.join(" | "));
    }
    if rows.is_empty() {
        println!("(no results)");
    }
    Ok(())
}

pub(crate) fn cmd_export(
    root: &Path,
    format: &str,
    output: Option<std::path::PathBuf>,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    match output {
        Some(path) => {
            let file = std::fs::File::create(&path)
                .with_context(|| format!("failed to create output file: {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            export_to_writer(backend, format, &mut writer)?;
            println!("Exported {} to {}", format, path.display());
        }
        None => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            export_to_writer(backend, format, &mut writer)?;
        }
    }

    Ok(())
}

pub(crate) fn cmd_callers(root: &Path, symbol: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let callers = backend.callers_of(symbol)?;
    if callers.is_empty() {
        println!("No callers found for '{}'", symbol);
        return Ok(());
    }

    println!("Callers of '{}' ({}):", symbol, callers.len());
    for caller in &callers {
        println!("  {}", caller);
    }
    Ok(())
}

pub(crate) fn cmd_callees(root: &Path, symbol: &str) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let callees = backend.callees_of(symbol)?;
    if callees.is_empty() {
        println!("No callees found for '{}'", symbol);
        return Ok(());
    }

    println!("Callees of '{}' ({}):", symbol, callees.len());
    for callee in &callees {
        println!("  {}", callee);
    }
    Ok(())
}

pub(crate) fn cmd_dead_code(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let rows = backend.find_uncalled_symbols()?;

    if rows.is_empty() {
        println!("No dead code found (all functions/methods have callers).");
        return Ok(());
    }

    let entry_points = ["main", "__init__", "setUp", "tearDown"];
    let dead: Vec<_> = rows
        .iter()
        .filter(|r| !entry_points.contains(&r.name.as_str()))
        .collect();

    if dead.is_empty() {
        println!("No dead code found (all non-entry-point functions have callers).");
        return Ok(());
    }

    println!("Potentially dead code ({} symbols):", dead.len());
    let mut current_file = "";
    for r in &dead {
        if r.file != current_file {
            current_file = &r.file;
            println!("\n  {}:", current_file);
        }
        println!("    {:>8} {}", r.kind, r.name);
    }

    Ok(())
}

pub(crate) fn cmd_impact(root: &Path, symbol: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let impacted = backend.transitive_impact(symbol, depth)?;

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
    backend: &dyn infigraph_core::graph::GraphBackend,
    format: &str,
    writer: &mut W,
) -> anyhow::Result<()> {
    match format {
        "cypher" => infigraph_core::export::export_cypher(backend, writer),
        "graphml" => infigraph_core::export::export_graphml(backend, writer),
        "json" => infigraph_core::export::export_json(backend, writer),
        _ => anyhow::bail!(
            "unknown export format '{}'. Supported formats: cypher, graphml, json",
            format
        ),
    }
}
