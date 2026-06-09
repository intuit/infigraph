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
