use anyhow::{Context, Result};
use infigraph_core::graph::GraphBackend;
use serde_json::Value;

use super::super::helpers::{open_prism, save_analysis};

pub fn tool_detect_dead_code(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let rows = backend.find_uncalled_symbols()?;

    let entry_points = ["main", "__init__", "setUp", "tearDown"];
    let dead: Vec<_> = rows
        .iter()
        .filter(|row| !entry_points.contains(&row.name.as_str()))
        .collect();

    if dead.is_empty() {
        return Ok("No dead code found.".to_string());
    }

    let mut out = format!("Potentially dead code ({} symbols):\n", dead.len());
    for row in &dead {
        out.push_str(&format!("  {} {} ({})\n", row.kind, row.name, row.file));
    }

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    match save_analysis(path, "dead_code", &out) {
        Ok(receipt) => Ok(receipt),
        Err(_) => Ok(out),
    }
}

pub fn tool_trace_callers(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let backend = prism.backend().context("not initialized")?;
    let callers = backend.callers_of(symbol_id)?;
    if callers.is_empty() {
        return Ok(format!("No callers found for '{}'", symbol_id));
    }
    Ok(callers.join("\n"))
}

pub fn tool_trace_callees(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let backend = prism.backend().context("not initialized")?;
    let callees = backend.callees_of(symbol_id)?;
    if callees.is_empty() {
        return Ok(format!("No callees found for '{}'", symbol_id));
    }
    Ok(callees.join("\n"))
}

pub fn tool_transitive_impact(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(5) as u32;

    let backend = prism.backend().context("not initialized")?;
    let impacted = backend.transitive_impact(symbol_id, depth)?;
    if impacted.is_empty() {
        return Ok(format!("No symbols affected by changes to '{}'", symbol_id));
    }

    let mut out = String::new();
    for row in &impacted {
        out.push_str(&format!("{} {} ({})\n", row.kind, row.name, row.file));
    }
    Ok(out)
}

pub fn tool_get_architecture(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    build_architecture_report(backend)
}

pub fn build_architecture_report(backend: &dyn GraphBackend) -> Result<String> {
    let stats = backend.get_architecture_stats()?;
    let mut out = String::new();

    out.push_str("=== Language Breakdown ===\n");
    if stats.languages.is_empty() {
        out.push_str("  (no modules indexed)\n");
    } else {
        for l in &stats.languages {
            out.push_str(&format!("  {:>20}: {} files\n", l.language, l.count));
        }
    }

    out.push_str("\n=== Symbols by Kind ===\n");
    if stats.kind_counts.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for k in &stats.kind_counts {
            out.push_str(&format!("  {:>20}: {}\n", k.kind, k.count));
        }
    }

    out.push_str("\n=== Hotspot Files (most symbols) ===\n");
    if stats.hotspot_files.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for (i, h) in stats.hotspot_files.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:60} {} symbols\n",
                i + 1,
                h.file,
                h.count
            ));
        }
    }

    out.push_str("\n=== Hub Functions (most callers) ===\n");
    if stats.hub_functions.is_empty() {
        out.push_str("  (no call edges found)\n");
    } else {
        for (i, h) in stats.hub_functions.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:30} {:40} {} callers\n",
                i + 1,
                h.name,
                h.file,
                h.calls
            ));
        }
    }

    out.push_str("\n=== Entry Points (call others, never called) ===\n");
    if stats.entry_points.is_empty() {
        out.push_str("  (none found)\n");
    } else {
        for row in &stats.entry_points {
            out.push_str(&format!("  {:>8} {:30} {}\n", row.kind, row.name, row.file));
        }
    }

    Ok(out)
}

pub fn tool_detect_clusters(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let stats = infigraph_core::cluster::detect_clusters(backend)?;
    Ok(format!("{}", stats))
}
