use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::{open_prism, save_analysis};

pub(crate) fn tool_detect_dead_code(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN s.name, s.kind, s.file ORDER BY s.file, s.name",
    )?;

    let entry_points = ["main", "__init__", "setUp", "tearDown"];
    let dead: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| !entry_points.contains(&row[0].as_str()))
        .collect();

    if dead.is_empty() {
        return Ok("No dead code found.".to_string());
    }

    let mut out = format!("Potentially dead code ({} symbols):\n", dead.len());
    for row in &dead {
        out.push_str(&format!("  {} {} ({})\n", row[1], row[0], row[2]));
    }

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    match save_analysis(path, "dead_code", &out) {
        Ok(receipt) => Ok(receipt),
        Err(_) => Ok(out),
    }
}

pub(crate) fn tool_trace_callers(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let callers = gq.callers_of(symbol_id)?;
    if callers.is_empty() {
        return Ok(format!("No callers found for '{}'", symbol_id));
    }
    Ok(callers.join("\n"))
}

pub(crate) fn tool_trace_callees(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let callees = gq.callees_of(symbol_id)?;
    if callees.is_empty() {
        return Ok(format!("No callees found for '{}'", symbol_id));
    }
    Ok(callees.join("\n"))
}

pub(crate) fn tool_transitive_impact(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(5) as u32;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let impacted = gq.transitive_impact(symbol_id, depth)?;
    if impacted.is_empty() {
        return Ok(format!("No symbols affected by changes to '{}'", symbol_id));
    }

    let mut out = String::new();
    for row in &impacted {
        out.push_str(&format!("{} {} ({})\n", row.kind, row.name, row.file));
    }
    Ok(out)
}

pub(crate) fn tool_get_architecture(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    build_architecture_report(&gq)
}

pub(crate) fn build_architecture_report(gq: &infigraph_core::graph::GraphQuery) -> Result<String> {
    let mut out = String::new();

    // 1. Language breakdown
    out.push_str("=== Language Breakdown ===\n");
    let lang_rows =
        gq.raw_query("MATCH (m:Module) RETURN m.language, count(m) ORDER BY count(m) DESC")?;
    if lang_rows.is_empty() {
        out.push_str("  (no modules indexed)\n");
    } else {
        for row in &lang_rows {
            out.push_str(&format!("  {:>20}: {} files\n", row[0], row[1]));
        }
    }

    // 2. Total symbols by kind
    out.push_str("\n=== Symbols by Kind ===\n");
    let kind_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.kind, count(s) ORDER BY count(s) DESC")?;
    if kind_rows.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for row in &kind_rows {
            out.push_str(&format!("  {:>20}: {}\n", row[0], row[1]));
        }
    }

    // 3. Hotspots: files with most symbols
    out.push_str("\n=== Hotspot Files (most symbols) ===\n");
    let hotspot_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10")?;
    if hotspot_rows.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for (i, row) in hotspot_rows.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:60} {} symbols\n",
                i + 1,
                row[0],
                row[1]
            ));
        }
    }

    // 4. Hub functions: most-called
    out.push_str("\n=== Hub Functions (most callers) ===\n");
    let hub_rows = gq.raw_query(
        "MATCH ()-[r:CALLS]->(s:Symbol) RETURN s.name, s.file, count(r) AS calls ORDER BY calls DESC LIMIT 10",
    )?;
    if hub_rows.is_empty() {
        out.push_str("  (no call edges found)\n");
    } else {
        for (i, row) in hub_rows.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:30} {:40} {} callers\n",
                i + 1,
                row[0],
                row[1],
                row[2]
            ));
        }
    }

    // 5. Entry points: functions that call others but are not called themselves
    out.push_str("\n=== Entry Points (call others, never called) ===\n");
    let entry_rows = gq.raw_query(
        "MATCH (s:Symbol)-[:CALLS]->() WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN DISTINCT s.name, s.kind, s.file ORDER BY s.file, s.name LIMIT 20",
    )?;
    if entry_rows.is_empty() {
        out.push_str("  (none found)\n");
    } else {
        for row in &entry_rows {
            out.push_str(&format!("  {:>8} {:30} {}\n", row[1], row[0], row[2]));
        }
    }

    Ok(out)
}

pub(crate) fn tool_detect_clusters(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;

    let stats = infigraph_core::cluster::detect_clusters(&conn)?;
    Ok(format!("{}", stats))
}
