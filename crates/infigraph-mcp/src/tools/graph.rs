use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::multi::Registry;
use infigraph_languages::bundled_registry;

use super::helpers::{glob_matches, open_prism};

pub(crate) fn tool_query_graph(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let cypher = args
        .get("cypher")
        .and_then(|c| c.as_str())
        .context("missing 'cypher'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query(cypher)?;
    let mut out = String::new();
    for row in &rows {
        out.push_str(&row.join(" | "));
        out.push('\n');
    }
    if out.is_empty() {
        out = "(no results)".to_string();
    }
    Ok(out)
}

pub(crate) fn tool_get_symbols_in_file(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let file = args
        .get("file")
        .and_then(|f| f.as_str())
        .context("missing 'file'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbols = gq.symbols_in_file(file)?;
    let mut out = String::new();
    for s in &symbols {
        out.push_str(&format!(
            "{:>8} {:30} L{}-{}\n",
            s.kind, s.name, s.start_line, s.end_line
        ));
    }
    if out.is_empty() {
        out = format!("No symbols found for '{}'. Run index_project first.", file);
    }
    Ok(out)
}

pub(crate) fn tool_get_stats(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let stats = prism.stats()?;
    Ok(format!("{}", stats))
}

pub(crate) fn tool_get_code_snippet(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let file_path = prism.root().join(&detail.file);
    let snippet = infigraph_core::search::read_lines_from_file(
        &file_path,
        detail.start_line,
        detail.end_line,
    )?;

    let mut out = format!(
        "// {} {} ({}:L{}-{})\n",
        detail.kind, detail.name, detail.file, detail.start_line, detail.end_line
    );
    out.push_str(&snippet);
    Ok(out)
}

pub(crate) fn tool_list_projects(_args: &Value) -> Result<String> {
    let registry = Registry::load()?;

    if registry.repos.is_empty() {
        return Ok("No projects registered. Run index_project to index a codebase.".to_string());
    }

    let mut out = format!("Registered projects ({}):\n", registry.repos.len());
    let mut repos: Vec<_> = registry.repos.values().collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in &repos {
        out.push_str(&format!(
            "  {:30} {:>6} symbols  {}\n",
            entry.name,
            entry.symbol_count,
            entry.path.display()
        ));
    }
    Ok(out)
}

pub(crate) fn tool_delete_project(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let project_path = PathBuf::from(path);

    // Remove the .infigraph directory within the project
    let infigraph_dir = project_path.join(".infigraph");
    if infigraph_dir.exists() {
        std::fs::remove_dir_all(&infigraph_dir).context("failed to remove .infigraph directory")?;
    }

    // Unregister from the global registry
    let mut registry = Registry::load()?;
    let canonical = project_path.canonicalize().unwrap_or(project_path.clone());
    let to_remove: Vec<String> = registry
        .repos
        .iter()
        .filter(|(_, entry)| {
            entry.path == project_path
                || entry.path == canonical
                || entry
                    .path
                    .canonicalize()
                    .map(|p| p == canonical)
                    .unwrap_or(false)
        })
        .map(|(name, _)| name.clone())
        .collect();

    for name in &to_remove {
        registry.repos.remove(name);
    }
    registry.save()?;

    if to_remove.is_empty() {
        Ok(format!(
            "Removed .infigraph directory from {}. (Project was not in the global registry.)",
            path
        ))
    } else {
        Ok(format!(
            "Removed .infigraph directory and unregistered '{}' from global registry.",
            to_remove.join(", ")
        ))
    }
}

pub(crate) fn tool_list_languages(_args: &Value) -> Result<String> {
    let registry = bundled_registry()?;
    let mut out = String::new();
    let mut count = 0;
    for pack in registry.languages() {
        out.push_str(&format!(
            "  {:20} {}\n",
            pack.name,
            pack.extensions.join(", ")
        ));
        count += 1;
    }
    Ok(format!("Supported languages ({}):\n{}", count, out))
}

pub(crate) fn tool_get_graph_schema(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let stats = prism.stats()?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let mut out = String::new();

    out.push_str("=== Node Types ===\n");
    out.push_str(&format!(
        "  Symbol  ({} nodes)  properties: id, name, kind, file, start_line, end_line, docstring\n",
        stats.symbols
    ));
    out.push_str(&format!(
        "  Module  ({} nodes)  properties: id, name, file, language\n",
        stats.modules
    ));

    out.push_str("\n=== Edge Types ===\n");
    out.push_str(&format!(
        "  CALLS     ({} edges)  Symbol -> Symbol\n",
        stats.calls
    ));
    out.push_str(&format!(
        "  INHERITS  ({} edges)  Symbol -> Symbol\n",
        stats.inherits
    ));
    out.push_str(&format!(
        "  CONTAINS  ({} edges)  Module -> Symbol, Symbol -> Symbol\n",
        stats.contains
    ));

    // Show symbol kinds present in the graph
    out.push_str("\n=== Symbol Kinds ===\n");
    let kind_rows =
        gq.raw_query("MATCH (s:Symbol) RETURN s.kind, count(s) ORDER BY count(s) DESC")?;
    for row in &kind_rows {
        out.push_str(&format!("  {:>20}: {}\n", row[0], row[1]));
    }

    Ok(out)
}

pub(crate) fn tool_symbol_context(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    // Find the symbol
    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let mut out = String::new();
    out.push_str(&format!("=== Symbol: {} ===\n", symbol_id));
    out.push_str(&format!("  Name:       {}\n", detail.name));
    out.push_str(&format!("  Kind:       {}\n", detail.kind));
    out.push_str(&format!("  File:       {}\n", detail.file));
    out.push_str(&format!(
        "  Lines:      {}-{}\n",
        detail.start_line, detail.end_line
    ));

    // Docstring
    let doc_rows = gq.raw_query(&format!(
        "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.docstring",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = doc_rows.first() {
        if !row[0].is_empty() {
            out.push_str(&format!("  Docstring:  {}\n", row[0]));
        }
    }

    // Parent (containing scope)
    let parent_rows = gq.raw_query(&format!(
        "MATCH (parent)-[:CONTAINS]->(s:Symbol) WHERE s.id = '{}' RETURN parent.id, parent.name",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = parent_rows.first() {
        out.push_str(&format!("  Parent:     {} ({})\n", row[1], row[0]));
    }

    // Callers
    let callers = gq.callers_of(symbol_id)?;
    out.push_str(&format!("\n  Callers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("    {}\n", c));
        }
    }

    // Callees
    let callees = gq.callees_of(symbol_id)?;
    out.push_str(&format!("\n  Callees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("    {}\n", c));
        }
    }

    Ok(out)
}

pub(crate) fn tool_detect_routes(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let routes = infigraph_core::routes::detect_routes(&gq)?;
    Ok(infigraph_core::routes::format_routes(&routes))
}

pub(crate) fn tool_find_all_references(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;

    let refs = gq.find_all_references(symbol_id)?;
    if refs.is_empty() {
        return Ok(format!("No references found for '{}'", symbol_id));
    }

    let mut out = format!("References to '{}' ({} total):\n\n", symbol_id, refs.len());
    for r in &refs {
        out.push_str(&format!("  {}:{} — in {}\n", r.file, r.line, r.caller_name));
    }
    Ok(out)
}

pub(crate) fn tool_get_api_surface(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut syms = gq.get_api_surface()?;
    if let Some(f) = file_filter {
        syms.retain(|s| s.file.contains(f));
    }

    if syms.is_empty() {
        return Ok("No public symbols found. Ensure project is indexed.".to_string());
    }

    let mut out = format!("API Surface ({} symbols):\n\n", syms.len());
    let mut cur_file = String::new();
    for s in &syms {
        if s.file != cur_file {
            out.push_str(&format!("## {}\n", s.file));
            cur_file = s.file.clone();
        }
        let doc = if s.docstring.is_empty() || s.docstring == "''" {
            String::new()
        } else {
            format!(
                " — {}",
                s.docstring
                    .trim_matches('\'')
                    .chars()
                    .take(80)
                    .collect::<String>()
            )
        };
        out.push_str(&format!(
            "  [{kind}] {name} (L{line}){doc}\n",
            kind = s.kind,
            name = s.name,
            line = s.line,
            doc = doc
        ));
    }
    Ok(out)
}

pub(crate) fn tool_get_file_deps(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .context("missing 'file'")?;

    let deps = gq.get_file_deps(file)?;
    let mut out = format!("File dependencies for '{}':\n\n", file);

    out.push_str(&format!("Imports ({}):\n", deps.imports.len()));
    for f in &deps.imports {
        out.push_str(&format!("  → {}\n", f));
    }
    if deps.imports.is_empty() {
        out.push_str("  (none)\n");
    }

    out.push_str(&format!("\nImported by ({}):\n", deps.imported_by.len()));
    for f in &deps.imported_by {
        out.push_str(&format!("  ← {}\n", f));
    }
    if deps.imported_by.is_empty() {
        out.push_str("  (none)\n");
    }

    Ok(out)
}

pub(crate) fn tool_get_type_hierarchy(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let hier = gq.get_type_hierarchy(symbol_id, depth)?;
    let mut out = format!("Type hierarchy for '{}':\n\n", hier.root_name);

    out.push_str(&format!("Ancestors ({}):\n", hier.ancestors.len()));
    for a in &hier.ancestors {
        out.push_str(&format!("  ↑ {} [{}] ({})\n", a.name, a.kind, a.file));
    }
    if hier.ancestors.is_empty() {
        out.push_str("  (none — root type)\n");
    }

    out.push_str(&format!("\nDescendants ({}):\n", hier.descendants.len()));
    for d in &hier.descendants {
        out.push_str(&format!("  ↓ {} [{}] ({})\n", d.name, d.kind, d.file));
    }
    if hier.descendants.is_empty() {
        out.push_str("  (none — leaf type)\n");
    }

    Ok(out)
}

pub(crate) fn tool_get_test_coverage(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut cov = gq.get_test_coverage()?;

    if let Some(f) = file_filter {
        cov.covered.retain(|s| s.file.contains(f));
        cov.uncovered.retain(|s| s.file.contains(f));
        let total = cov.covered.len() + cov.uncovered.len();
        cov.coverage_pct = (cov.covered.len() * 100).checked_div(total).unwrap_or(0);
        cov.covered_count = cov.covered.len();
        cov.uncovered_count = cov.uncovered.len();
    }

    let mut out = format!(
        "Test Coverage: {}% ({} covered, {} uncovered)\n\n",
        cov.coverage_pct, cov.covered_count, cov.uncovered_count
    );

    if !cov.uncovered.is_empty() {
        out.push_str("Uncovered symbols:\n");
        for s in cov.uncovered.iter().take(50) {
            out.push_str(&format!(
                "  ✗ {} [{}] — {}\n",
                s.symbol_name, s.kind, s.file
            ));
        }
        if cov.uncovered.len() > 50 {
            out.push_str(&format!("  ... and {} more\n", cov.uncovered.len() - 50));
        }
    }

    if !cov.covered.is_empty() {
        out.push_str(&format!("\nCovered symbols ({}):\n", cov.covered.len()));
        for s in cov.covered.iter().take(20) {
            out.push_str(&format!("  ✓ {} [{}]\n", s.symbol_name, s.kind));
        }
        if cov.covered.len() > 20 {
            out.push_str(&format!("  ... and {} more\n", cov.covered.len() - 20));
        }
    }

    Ok(out)
}

pub(crate) fn tool_get_complexity(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let threshold = args.get("threshold").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let file_filter = args.get("file").and_then(|v| v.as_str());

    let base_q = if let Some(f) = file_filter {
        format!(
            "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') AND s.file CONTAINS '{}' RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC",
            f.replace('\'', "\\'")
        )
    } else {
        "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC".to_string()
    };

    let rows = gq.raw_query(&base_q)?;

    if rows.is_empty() {
        return Ok("No function/method symbols found. Run index_project first.".to_string());
    }

    let total: u32 = rows
        .iter()
        .filter_map(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()))
        .sum();
    let count = rows.len();
    let avg = if count > 0 {
        total as f64 / count as f64
    } else {
        0.0
    };

    let hotspots: Vec<_> = rows
        .iter()
        .filter(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) >= threshold)
        .collect();

    let mut out = format!(
        "Complexity Analysis: {} symbols, avg {:.1}, {} hotspots (>= {})\n\n",
        count,
        avg,
        hotspots.len(),
        threshold
    );

    if !hotspots.is_empty() {
        out.push_str(&format!("Hotspots (complexity >= {}):\n", threshold));
        for row in &hotspots {
            let name = row.first().map(|s| s.as_str()).unwrap_or("?");
            let file = row.get(1).map(|s| s.as_str()).unwrap_or("?");
            let line = row.get(2).map(|s| s.as_str()).unwrap_or("?");
            let cplx = row.get(3).map(|s| s.as_str()).unwrap_or("?");
            out.push_str(&format!("  [{cplx:>3}] {name}  ({file}:{line})\n"));
        }
        out.push('\n');
    }

    out.push_str("Top 20 by complexity:\n");
    for row in rows.iter().take(20) {
        let name = row.first().map(|s| s.as_str()).unwrap_or("?");
        let file = row.get(1).map(|s| s.as_str()).unwrap_or("?");
        let line = row.get(2).map(|s| s.as_str()).unwrap_or("?");
        let cplx = row.get(3).map(|s| s.as_str()).unwrap_or("?");
        let flag = if cplx.parse::<u32>().unwrap_or(0) >= threshold {
            " ⚠"
        } else {
            ""
        };
        out.push_str(&format!("  [{cplx:>3}] {name}  ({file}:{line}){flag}\n"));
    }

    Ok(out)
}

pub(crate) fn tool_get_doc_context(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let detail = gq
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found", symbol_id))?;

    let mut out = format!("=== {} {} ===\n", detail.kind, detail.name);
    out.push_str(&format!(
        "File:  {}:{}-{}\n",
        detail.file, detail.start_line, detail.end_line
    ));

    // Docstring
    let doc_rows = gq.raw_query(&format!(
        "MATCH (s:Symbol) WHERE s.id = '{}' RETURN s.docstring, s.complexity",
        symbol_id.replace('\'', "\\'")
    ))?;
    if let Some(row) = doc_rows.first() {
        if !row[0].is_empty() {
            out.push_str(&format!("Doc:   {}\n", row[0]));
        }
        if !row[1].is_empty() && row[1] != "1" {
            out.push_str(&format!("Complexity: {}\n", row[1]));
        }
    }

    // Source snippet (read file directly)
    let file_path = prism.root().join(&detail.file);
    if let Ok(source) = std::fs::read_to_string(&file_path) {
        let lines: Vec<&str> = source.lines().collect();
        let start = (detail.start_line as usize).saturating_sub(1);
        let end = (detail.end_line as usize).min(lines.len());
        if start < end {
            out.push_str("\nSource:\n```\n");
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{:4}  {}\n", start + i + 1, line));
            }
            out.push_str("```\n");
        }
    }

    // Callers
    let callers = gq.callers_of(symbol_id)?;
    out.push_str(&format!("\nCallers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("  (none — possible entry point or dead code)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("  {}\n", c));
        }
    }

    // Callees
    let callees = gq.callees_of(symbol_id)?;
    out.push_str(&format!("\nCallees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("  {}\n", c));
        }
    }

    Ok(out)
}

pub(crate) fn tool_list_files(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let rows = gq.raw_query("MATCH (s:Symbol) RETURN DISTINCT s.file ORDER BY s.file")?;
    if rows.is_empty() {
        return Ok("No files indexed. Run index_project first.".to_string());
    }

    let glob = args.get("glob").and_then(|g| g.as_str()).unwrap_or("");

    let mut files: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.first().map(|s| s.as_str()))
        .filter(|f| glob.is_empty() || glob_matches(glob, f))
        .collect();
    files.dedup();

    Ok(files.join("\n"))
}

pub(crate) fn tool_generate_sequence_diagram(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id' argument")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);
    infigraph_core::sequence::generate_sequence_mermaid(&gq, symbol_id, depth)
}
