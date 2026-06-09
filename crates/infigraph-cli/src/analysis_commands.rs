use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_architecture(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let report = build_architecture_report(&gq)?;
    println!("{}", report);
    Ok(())
}

fn build_architecture_report(gq: &infigraph_core::graph::GraphQuery) -> Result<String> {
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
    let hotspot_rows = gq.raw_query(
        "MATCH (s:Symbol) RETURN s.file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10",
    )?;
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

pub(crate) fn cmd_cluster(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;

    println!("Running Louvain community detection...");
    let stats = infigraph_core::cluster::detect_clusters(&conn)?;
    println!("{}", stats);
    Ok(())
}

pub(crate) fn cmd_detect_changes(root: &Path, base: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let report = build_detect_changes_report(prism.root(), &gq, base, depth)?;
    println!("{}", report);
    Ok(())
}

/// Parse git diff output and map changed lines to symbols in the graph.
fn build_detect_changes_report(
    project_root: &std::path::Path,
    gq: &infigraph_core::graph::GraphQuery,
    base: &str,
    depth: u32,
) -> Result<String> {
    // 1. Get changed files
    let name_output = std::process::Command::new("git")
        .args(["diff", "--name-only", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --name-only")?;

    if !name_output.status.success() {
        let stderr = String::from_utf8_lossy(&name_output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    let changed_files: Vec<String> = String::from_utf8_lossy(&name_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if changed_files.is_empty() {
        return Ok("No changes detected.".to_string());
    }

    // 2. Get unified diff with zero context to extract changed line ranges
    let diff_output = std::process::Command::new("git")
        .args(["diff", "--unified=0", base])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --unified=0")?;

    let diff_text = String::from_utf8_lossy(&diff_output.stdout);
    let hunks = parse_diff_hunks(&diff_text);

    // 3. For each changed file+range, find overlapping symbols
    let mut directly_changed: Vec<(String, String, String, u32, u32)> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (file, start, end) in &hunks {
        let symbols = gq.symbols_in_range(file, *start, *end)?;
        for s in symbols {
            if seen_ids.insert(s.id.clone()) {
                directly_changed.push((s.id, s.name, s.file, s.start_line, s.end_line));
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!("=== Change Detection (base: {}) ===\n\n", base));
    out.push_str(&format!("Changed files: {}\n", changed_files.len()));
    for f in &changed_files {
        out.push_str(&format!("  {}\n", f));
    }

    out.push_str(&format!(
        "\n=== Directly Changed Symbols ({}) ===\n",
        directly_changed.len()
    ));
    if directly_changed.is_empty() {
        out.push_str("  (no indexed symbols overlap with changed lines)\n");
    } else {
        for (id, name, file, start, end) in &directly_changed {
            out.push_str(&format!("  {:30} {} L{}-{}\n", name, file, start, end));
            let _ = id;
        }
    }

    // 4. Compute blast radius via transitive impact for each directly changed symbol
    if !directly_changed.is_empty() && depth > 0 {
        let mut indirectly_affected: Vec<(String, String, String, String)> = Vec::new();
        let mut indirect_ids: HashSet<String> = HashSet::new();

        for (id, _, _, _, _) in &directly_changed {
            if let Ok(impacted) = gq.transitive_impact(id, depth) {
                for row in impacted {
                    if !seen_ids.contains(&row.id) && indirect_ids.insert(row.id.clone()) {
                        indirectly_affected.push((row.id, row.name, row.file, row.kind));
                    }
                }
            }
        }

        out.push_str(&format!(
            "\n=== Blast Radius (depth={}, {} indirectly affected) ===\n",
            depth,
            indirectly_affected.len()
        ));
        if indirectly_affected.is_empty() {
            out.push_str("  (no additional symbols affected)\n");
        } else {
            for (_, name, file, kind) in &indirectly_affected {
                out.push_str(&format!("  {:>8} {:30} {}\n", kind, name, file));
            }
        }
    }

    Ok(out)
}

/// Parse unified diff output (with --unified=0) to extract (file, start_line, end_line) hunks.
fn parse_diff_hunks(diff: &str) -> Vec<(String, u32, u32)> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.to_string();
            continue;
        }

        if line.starts_with("@@") && !current_file.is_empty() {
            if let Some(plus_part) = line.split('+').nth(1) {
                let range_part = plus_part.split(' ').next().unwrap_or("");
                let parts: Vec<&str> = range_part.split(',').collect();
                let start: u32 = parts[0].parse().unwrap_or(0);
                let count: u32 = if parts.len() > 1 {
                    parts[1].parse().unwrap_or(1)
                } else {
                    1
                };
                if start > 0 {
                    let end = if count == 0 { start } else { start + count - 1 };
                    hunks.push((current_file.clone(), start, end));
                }
            }
        }
    }

    hunks
}

pub(crate) fn cmd_security(root: &Path, severity: Option<&str>, category: Option<&str>) -> Result<()> {
    let canonical = root.canonicalize().context("invalid project root")?;
    let mut scan = infigraph_core::security::scan_project(&canonical)?;

    if let Some(sev) = severity {
        let sev_upper = sev.to_uppercase();
        scan.findings
            .retain(|f| f.severity.to_string() == sev_upper);
    }
    if let Some(cat) = category {
        let cat_norm = cat.to_lowercase().replace(' ', "");
        scan.findings
            .retain(|f| f.category.to_string().to_lowercase().replace(' ', "") == cat_norm);
    }

    println!("{}", infigraph_core::security::format_scan_results(&scan));
    Ok(())
}

pub(crate) fn cmd_complexity(root: &Path, threshold: u32, file: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("graph not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    let base_q = if let Some(f) = file {
        format!(
            "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') AND s.file CONTAINS '{}' RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC",
            f.replace('\'', "\\'")
        )
    } else {
        "MATCH (s:Symbol) WHERE (s.kind = 'Function' OR s.kind = 'Method' OR s.kind = 'Test') RETURN s.name, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC".to_string()
    };

    let rows = gq.raw_query(&base_q)?;
    if rows.is_empty() {
        println!("No symbols found. Run 'infigraph index' first.");
        return Ok(());
    }

    let total: u32 = rows
        .iter()
        .filter_map(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()))
        .sum();
    let avg = total as f64 / rows.len() as f64;
    let hotspots: Vec<_> = rows
        .iter()
        .filter(|r| r.get(3).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) >= threshold)
        .collect();

    println!(
        "Complexity: {} symbols, avg {:.1}, {} hotspots (>= {})\n",
        rows.len(),
        avg,
        hotspots.len(),
        threshold
    );

    for row in rows.iter().take(30) {
        let name = row.first().map(|s| s.as_str()).unwrap_or("?");
        let file = row.get(1).map(|s| s.as_str()).unwrap_or("?");
        let line = row.get(2).map(|s| s.as_str()).unwrap_or("?");
        let cplx = row.get(3).map(|s| s.as_str()).unwrap_or("0");
        let flag = if cplx.parse::<u32>().unwrap_or(0) >= threshold {
            " ⚠"
        } else {
            ""
        };
        println!("  [{cplx:>3}] {name}  ({file}:{line}){flag}");
    }
    Ok(())
}

pub(crate) fn cmd_refactor(root: &Path, target: Option<&str>, focus: &str, limit: usize) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;

    let emb_path = root.join(".infigraph").join("embeddings.bin");
    let emb_ref = if emb_path.exists() {
        Some(emb_path.as_path())
    } else {
        None
    };

    let focus = infigraph_core::refactor::Focus::parse(focus);
    let recs = infigraph_core::refactor::analyze(&conn, emb_ref, target, focus, limit)?;
    print!(
        "{}",
        infigraph_core::refactor::format_recommendations(&recs, target)
    );
    Ok(())
}

pub(crate) fn cmd_semantic_diff(root: &Path, old_ref: &str, new_ref: &str) -> Result<()> {
    let canonical = root.canonicalize().context("invalid project root")?;
    let registry = bundled_registry()?;
    let diff = infigraph_core::diff::semantic_diff(&canonical, old_ref, new_ref, &registry)?;
    println!("{}", infigraph_core::diff::format_diff(&diff));
    Ok(())
}

pub(crate) fn cmd_sequence(root: &Path, symbol_id: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("not initialized")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);
    let diagram = infigraph_core::sequence::generate_sequence_mermaid(&gq, symbol_id, depth)?;
    println!("{}", diagram);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_review(
    root: &Path,
    base: &str,
    limit: usize,
    json: bool,
    llm: bool,
    dry_run: bool,
    context: Option<&str>,
    group: Option<&str>,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism
        .store()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let report = if let Some(group_name) = group {
        let multi_reg = infigraph_core::multi::Registry::load()?;
        infigraph_core::review::review_with_group(
            root,
            base,
            limit,
            prism.registry(),
            store,
            group_name,
            &multi_reg,
            bundled_registry,
        )?
    } else {
        infigraph_core::review::review(root, base, limit, prism.registry(), store)?
    };

    if json && !llm {
        println!("{}", infigraph_core::review::format_review_json(&report));
    } else if !llm {
        print!("{}", infigraph_core::review::format_review(&report));
    }

    if llm || dry_run {
        use infigraph_core::review::llm;
        let (prompt, result) =
            llm::review_with_llm(root, &report, store, dry_run, context)?;

        if dry_run {
            println!("{}", prompt);
        } else if let Some(result) = result {
            if json {
                println!("{}", llm::format_llm_review_json(&result));
            } else {
                print!("{}", infigraph_core::review::format_review(&report));
                print!("{}", llm::format_llm_review(&result));
            }
        }
    }

    Ok(())
}

pub(crate) fn cmd_check(
    root: &Path,
    config: Option<&Path>,
    json: bool,
    checks: Option<&str>,
) -> Result<bool> {
    use infigraph_core::check::{self, CheckSelection, CheckStatus};

    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism
        .store()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let config_path = config
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.join(".infigraph").join("check.toml"));
    let cfg = check::load_config(&config_path)?;

    let selection = match checks {
        Some(csv) => CheckSelection::from_csv(csv),
        None => CheckSelection::all(),
    };

    let results = check::run_checks(root, &cfg, store, &selection);

    if json {
        println!("{}", check::format_json(&results));
    } else {
        print!("{}", check::format_table(&results));
    }

    let any_failed = results.iter().any(|r| r.status == CheckStatus::Fail);
    Ok(any_failed)
}

pub(crate) fn cmd_vulns(
    root: &Path,
    severity: Option<&str>,
    ecosystem: Option<&str>,
    json: bool,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism.store().context("graph not initialized")?;

    let deps = infigraph_core::manifest::query_deps(store)?;
    if deps.is_empty() {
        println!("No dependencies found. Run 'infigraph index-manifests' first.");
        return Ok(());
    }

    eprintln!(
        "Scanning {} dependencies against OSV database...",
        deps.len()
    );

    let mut report = infigraph_core::vuln::scan_deps(&deps)?;

    if let Some(sev) = severity {
        infigraph_core::vuln::filter_by_severity(&mut report, sev);
    }
    if let Some(eco) = ecosystem {
        infigraph_core::vuln::filter_by_ecosystem(&mut report, eco);
    }

    if json {
        println!("{}", infigraph_core::vuln::format_json(&report));
    } else {
        print!("{}", infigraph_core::vuln::format_table(&report));
    }

    Ok(())
}

pub(crate) fn cmd_detect_patterns(
    root: &Path,
    pattern: Option<&str>,
    json: bool,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism
        .store()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let report = infigraph_core::patterns::detect_filtered(store, pattern)?;

    if json {
        println!("{}", infigraph_core::patterns::format_json(&report));
    } else {
        print!("{}", infigraph_core::patterns::format_report(&report));
    }

    Ok(())
}

pub(crate) fn cmd_forget(root: &Path) -> Result<()> {
    let abs_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut store = infigraph_core::learned::LearnedStore::load(&abs_root);
    let count = store.len();
    store.clear();
    store.save(&abs_root)?;
    println!("Cleared {} learned patterns", count);
    Ok(())
}

pub(crate) fn cmd_bridges_promote(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let store = prism
        .store()
        .context("graph not initialized -- run 'infigraph index' first")?;
    let conn = store.connection()?;
    let gq = infigraph_core::graph::GraphQuery::new(&conn);

    // Find BRIDGE_TO edges where both endpoints are resolved symbols, promote to CALLS
    let bridge_rows = gq.raw_query(
        "MATCH (a:Symbol)-[r:BRIDGE_TO]->(b:Symbol) RETURN a.id, b.id",
    )?;

    if bridge_rows.is_empty() {
        println!("No BRIDGE_TO edges found to promote.");
        return Ok(());
    }

    let count = bridge_rows.len();
    for row in &bridge_rows {
        let _ = gq.raw_query(&format!(
            "MATCH (a:Symbol {{id: '{}'}})-[r:BRIDGE_TO]->(b:Symbol {{id: '{}'}}) DELETE r CREATE (a)-[:CALLS]->(b)",
            row[0].replace('\'', "\\'"),
            row[1].replace('\'', "\\'"),
        ));
    }
    println!("Promoted {} BRIDGE_TO edges to CALLS edges.", count);
    Ok(())
}
