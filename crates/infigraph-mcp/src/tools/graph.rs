use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;
use infigraph_core::graph::SessionStore;
use infigraph_core::multi::Registry;
use infigraph_languages::bundled_registry;

use super::helpers::{glob_matches, open_prism_read_only};

pub fn tool_query_graph(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let cypher = args
        .get("cypher")
        .and_then(|c| c.as_str())
        .context("missing 'cypher'")?;

    let backend = prism.backend().context("not initialized")?;

    let rows = backend.raw_query(cypher)?;
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

pub fn tool_get_symbols_in_file(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let file = args
        .get("file")
        .and_then(|f| f.as_str())
        .context("missing 'file'")?;

    let backend = prism.backend().context("not initialized")?;

    let symbols = backend.symbols_in_file(file)?;
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

pub fn tool_get_stats(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let stats = prism.stats()?;
    Ok(format!("{}", stats))
}

pub fn tool_get_code_snippet(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let backend = prism.backend().context("not initialized")?;

    let detail = backend
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found in graph", symbol_id))?;

    let file_path = if std::path::Path::new(&detail.file).is_absolute() {
        PathBuf::from(&detail.file)
    } else {
        prism.root().join(&detail.file)
    };
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

pub fn tool_list_projects(_args: &Value) -> Result<String> {
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

pub fn tool_delete_project(args: &Value) -> Result<String> {
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

pub fn tool_list_languages(_args: &Value) -> Result<String> {
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

pub fn tool_get_graph_schema(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let stats = prism.stats()?;
    let backend = prism.backend().context("not initialized")?;

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
    out.push_str("  HAS_STATEMENT         Symbol -> Statement\n");

    out.push_str("\n=== Symbol Kinds ===\n");
    let arch = backend.get_architecture_stats()?;
    for k in &arch.kind_counts {
        out.push_str(&format!("  {:>20}: {}\n", k.kind, k.count));
    }

    Ok(out)
}

pub fn tool_symbol_context(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let backend = prism.backend().context("not initialized")?;

    let detail = backend
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

    if let Ok(Some(meta)) = backend.symbol_metadata(symbol_id) {
        if !meta.docstring.is_empty() {
            out.push_str(&format!("  Docstring:  {}\n", meta.docstring));
        }
        if let (Some(pid), Some(pname)) = (&meta.parent_id, &meta.parent_name) {
            out.push_str(&format!("  Parent:     {} ({})\n", pname, pid));
        }
    }

    let callers = backend.callers_of(symbol_id)?;
    out.push_str(&format!("\n  Callers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("    {}\n", c));
        }
    }

    let callees = backend.callees_of(symbol_id)?;
    out.push_str(&format!("\n  Callees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("    {}\n", c));
        }
    }

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    auto_inject_session_context(path, &detail.name, &mut out);

    Ok(out)
}

pub fn tool_detect_routes(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let routes = infigraph_core::routes::detect_routes(backend)?;
    Ok(infigraph_core::routes::format_routes(&routes))
}

pub fn tool_find_all_references(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;

    let refs = backend.find_all_references(symbol_id)?;
    if refs.is_empty() {
        return Ok(format!("No references found for '{}'", symbol_id));
    }

    let mut out = format!("References to '{}' ({} total):\n\n", symbol_id, refs.len());
    for r in &refs {
        out.push_str(&format!("  {}:{} — in {}\n", r.file, r.line, r.caller_name));
    }
    Ok(out)
}

pub fn tool_get_api_surface(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut syms = backend.get_api_surface()?;
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

pub fn tool_get_file_deps(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .context("missing 'file'")?;

    let deps = backend.get_file_deps(file)?;
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

pub fn tool_get_type_hierarchy(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let hier = backend.get_type_hierarchy(symbol_id, depth)?;
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

pub fn tool_get_test_coverage(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let mut cov = backend.get_test_coverage()?;

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

pub fn tool_get_complexity(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let threshold = args.get("threshold").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let file_filter = args.get("file").and_then(|v| v.as_str());

    let rows = backend.get_complexity_ranking(file_filter)?;

    if rows.is_empty() {
        return Ok("No function/method symbols found. Run index_project first.".to_string());
    }

    let total: u32 = rows.iter().map(|r| r.complexity).sum();
    let count = rows.len();
    let avg = if count > 0 {
        total as f64 / count as f64
    } else {
        0.0
    };

    let hotspots: Vec<_> = rows.iter().filter(|r| r.complexity >= threshold).collect();

    let mut out = format!(
        "Complexity Analysis: {} symbols, avg {:.1}, {} hotspots (>= {})\n\n",
        count,
        avg,
        hotspots.len(),
        threshold
    );

    if !hotspots.is_empty() {
        out.push_str(&format!("Hotspots (complexity >= {}):\n", threshold));
        for r in &hotspots {
            out.push_str(&format!(
                "  [{:>3}] {}  ({}:{})\n",
                r.complexity, r.name, r.file, r.start_line
            ));
        }
        out.push('\n');
    }

    out.push_str("Top 20 by complexity:\n");
    for r in rows.iter().take(20) {
        let flag = if r.complexity >= threshold {
            " ⚠"
        } else {
            ""
        };
        out.push_str(&format!(
            "  [{:>3}] {}  ({}:{}){flag}\n",
            r.complexity, r.name, r.file, r.start_line
        ));
    }

    Ok(out)
}

pub fn tool_get_skeleton(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .context("missing 'file'")?;

    backend.skeleton(file)
}

pub fn tool_get_doc_context(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .context("missing 'symbol_id'")?;

    let backend = prism.backend().context("not initialized")?;

    let detail = backend
        .find_symbol_by_id(symbol_id)?
        .context(format!("symbol '{}' not found", symbol_id))?;

    let mut out = format!("=== {} {} ===\n", detail.kind, detail.name);
    out.push_str(&format!(
        "File:  {}:{}-{}\n",
        detail.file, detail.start_line, detail.end_line
    ));

    if let Ok(Some(meta)) = backend.symbol_metadata(symbol_id) {
        if !meta.docstring.is_empty() {
            out.push_str(&format!("Doc:   {}\n", meta.docstring));
        }
        if meta.complexity > 1 {
            out.push_str(&format!("Complexity: {}\n", meta.complexity));
        }
    }

    let file_path = if std::path::Path::new(&detail.file).is_absolute() {
        PathBuf::from(&detail.file)
    } else {
        prism.root().join(&detail.file)
    };
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

    let callers = backend.callers_of(symbol_id)?;
    out.push_str(&format!("\nCallers ({}):\n", callers.len()));
    if callers.is_empty() {
        out.push_str("  (none — possible entry point or dead code)\n");
    } else {
        for c in &callers {
            out.push_str(&format!("  {}\n", c));
        }
    }

    let callees = backend.callees_of(symbol_id)?;
    out.push_str(&format!("\nCallees ({}):\n", callees.len()));
    if callees.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for c in &callees {
            out.push_str(&format!("  {}\n", c));
        }
    }

    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    auto_inject_session_context(path, &detail.name, &mut out);

    Ok(out)
}

fn auto_inject_session_context(path: &str, symbol_name: &str, main_output: &mut String) {
    let root = PathBuf::from(path);
    let emb_path = root
        .join(".infigraph")
        .join("sessions")
        .join("embeddings.bin");

    if !emb_path.exists() {
        return;
    }

    let emb_store = match embed::load_embeddings(&emb_path) {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };

    let embedder = embed::code_embedder();
    let query_vec = match embedder.embed(symbol_name) {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let store = match SessionStore::open(&root) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut scored: Vec<(f32, String)> = emb_store
        .iter()
        .map(|(id, vec)| (embed::cosine_similarity(&query_vec, vec), id.clone()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let max_inject_len = main_output.len() / 5; // 20% budget
    let mut injected_len = 0;

    for (score, session_id) in &scored {
        if *score < 0.7 {
            break;
        }

        let session = match store.load(session_id) {
            Ok(Some(s)) => s,
            _ => continue,
        };

        let confidence = session.compute_confidence(now);
        if confidence < 0.5 {
            continue;
        }

        let mut snippet = String::new();
        if !session.decisions.is_empty() {
            snippet.push_str(&format!("Decisions: {}\n", session.decisions));
        }
        if !session.constraints.is_empty() {
            snippet.push_str(&format!("Constraints: {}\n", session.constraints));
        }
        if !session.summary.is_empty() && snippet.is_empty() {
            snippet.push_str(&format!("Summary: {}\n", session.summary));
        }

        if snippet.is_empty() {
            continue;
        }

        if injected_len + snippet.len() > max_inject_len {
            break;
        }

        if injected_len == 0 {
            main_output.push_str("\n**Prior context:**\n");
        }

        let label = if session.name.is_empty() {
            session.id.clone()
        } else {
            format!("{} ({})", session.name, session.id)
        };
        main_output.push_str(&format!(
            "  [{}] (confidence: {:.2}): {}\n",
            label,
            confidence,
            snippet.trim()
        ));
        injected_len += snippet.len();

        let _ = store.touch_session(session_id);
    }
}

pub fn tool_list_files(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let all_files = backend.list_indexed_files()?;
    if all_files.is_empty() {
        return Ok("No files indexed. Run index_project first.".to_string());
    }

    let glob = args.get("glob").and_then(|g| g.as_str()).unwrap_or("");

    let files: Vec<&str> = all_files
        .iter()
        .map(|s| s.as_str())
        .filter(|f| glob.is_empty() || glob_matches(glob, f))
        .collect();

    Ok(files.join("\n"))
}

pub fn tool_generate_test_context(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let backend = prism.backend().context("not initialized")?;

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let test_type = args.get("test_type").and_then(|v| v.as_str());

    let ctx = backend.generate_test_context(file_filter, limit, test_type)?;

    let mut out = format!(
        "## Test Generation Context\n\nFramework: {}\n",
        ctx.framework
    );

    if let Some(ref ex) = ctx.example_test {
        out.push_str("\n### Example Test (style reference)\n");
        out.push_str(&format!(
            "  {} — {}:{}-{}\n",
            ex.name, ex.file, ex.start_line, ex.end_line
        ));
        let file_path = if std::path::Path::new(&ex.file).is_absolute() {
            PathBuf::from(&ex.file)
        } else {
            prism.root().join(&ex.file)
        };
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (ex.start_line as usize).saturating_sub(1);
            let end = (ex.end_line as usize).min(lines.len());
            if start < end {
                out.push_str("```\n");
                for (i, line) in lines[start..end].iter().enumerate() {
                    out.push_str(&format!("{:4}  {}\n", start + i + 1, line));
                }
                out.push_str("```\n");
            }
        }
    }

    out.push_str(&format!(
        "\n### Targets ({} uncovered symbols, priority-ranked)\n\n",
        ctx.targets.len()
    ));

    for (i, t) in ctx.targets.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}** [{}] — {}:{}-{} (priority: {})\n",
            i + 1,
            t.name,
            t.kind,
            t.file,
            t.start_line,
            t.end_line,
            t.priority_score
        ));
        if !t.visibility.is_empty() {
            out.push_str(&format!("   visibility: {}\n", t.visibility));
        }
        if !t.parameters.is_empty() {
            out.push_str(&format!("   params: {}\n", t.parameters));
        }
        if !t.return_type.is_empty() {
            out.push_str(&format!("   returns: {}\n", t.return_type));
        }
        if t.complexity > 1 {
            out.push_str(&format!("   complexity: {}\n", t.complexity));
        }
        if !t.callers.is_empty() {
            out.push_str(&format!("   callers: {}\n", t.callers.join(", ")));
        }
        if !t.callees.is_empty() {
            out.push_str(&format!("   callees: {}\n", t.callees.join(", ")));
        }
        if !t.branches.is_empty() {
            out.push_str(&format!("   branches ({}):\n", t.branches.len()));
            for b in &t.branches {
                let indent = "   ".repeat(b.depth as usize + 2);
                if b.condition.is_empty() {
                    out.push_str(&format!("{}L{}: {}\n", indent, b.line, b.kind));
                } else {
                    out.push_str(&format!(
                        "{}L{}: {} ({})\n",
                        indent, b.line, b.kind, b.condition
                    ));
                }
            }
        }

        let file_path = if std::path::Path::new(&t.file).is_absolute() {
            PathBuf::from(&t.file)
        } else {
            prism.root().join(&t.file)
        };
        if let Ok(source) = std::fs::read_to_string(&file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = (t.start_line as usize).saturating_sub(1);
            let end = (t.end_line as usize).min(lines.len());
            if start < end {
                out.push_str("   ```\n");
                for (i, line) in lines[start..end].iter().enumerate() {
                    out.push_str(&format!("   {:4}  {}\n", start + i + 1, line));
                }
                out.push_str("   ```\n");
            }
        }
        out.push('\n');
    }

    if !ctx.templates.is_empty() {
        out.push_str("\n### Framework Templates\n\n");
        for tpl in &ctx.templates {
            out.push_str(&format!("#### {} test\n", tpl.test_type));
            out.push_str(&format!("**Conventions:** {}\n\n", tpl.conventions));
            out.push_str("**Scaffold:**\n```\n");
            out.push_str(&tpl.scaffold);
            out.push_str("\n```\n\n");
        }
    }

    Ok(out)
}

pub fn tool_generate_sequence_diagram(args: &Value) -> Result<String> {
    let prism = open_prism_read_only(args)?;
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .context("missing 'symbol_id' argument")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
    let backend = prism.backend().context("not initialized")?;
    infigraph_core::sequence::generate_sequence_mermaid(backend, symbol_id, depth)
}
