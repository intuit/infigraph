use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_architecture(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let arch = backend.get_architecture_stats()?;

    let mut out = String::new();

    out.push_str("=== Language Breakdown ===\n");
    if arch.languages.is_empty() {
        out.push_str("  (no modules indexed)\n");
    } else {
        for l in &arch.languages {
            out.push_str(&format!("  {:>20}: {} files\n", l.language, l.count));
        }
    }

    out.push_str("\n=== Symbols by Kind ===\n");
    if arch.kind_counts.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for k in &arch.kind_counts {
            out.push_str(&format!("  {:>20}: {}\n", k.kind, k.count));
        }
    }

    out.push_str("\n=== Hotspot Files (most symbols) ===\n");
    if arch.hotspot_files.is_empty() {
        out.push_str("  (no symbols indexed)\n");
    } else {
        for (i, h) in arch.hotspot_files.iter().enumerate() {
            out.push_str(&format!(
                "  {:>2}. {:60} {} symbols\n",
                i + 1,
                h.file,
                h.count
            ));
        }
    }

    out.push_str("\n=== Hub Functions (most callers) ===\n");
    if arch.hub_functions.is_empty() {
        out.push_str("  (no call edges found)\n");
    } else {
        for (i, h) in arch.hub_functions.iter().enumerate() {
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
    if arch.entry_points.is_empty() {
        out.push_str("  (none found)\n");
    } else {
        for e in &arch.entry_points {
            out.push_str(&format!("  {:>8} {:30} {}\n", e.kind, e.name, e.file));
        }
    }

    println!("{}", out);
    Ok(())
}

pub(crate) fn cmd_cluster(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    println!("Running Louvain community detection...");
    let stats = infigraph_core::cluster::detect_clusters(backend)?;
    println!("{}", stats);
    Ok(())
}

pub(crate) fn cmd_detect_changes(root: &Path, base: &str, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let report = infigraph_mcp::tools::analysis::git::build_detect_changes_report(
        prism.root(),
        backend,
        base,
        depth,
    )?;
    println!("{}", report);
    Ok(())
}

pub(crate) fn cmd_security(
    root: &Path,
    severity: Option<&str>,
    category: Option<&str>,
) -> Result<()> {
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

    let backend = prism.backend().context("graph not initialized")?;
    let rows = backend.get_complexity_ranking(file)?;
    if rows.is_empty() {
        println!("No symbols found. Run 'infigraph index' first.");
        return Ok(());
    }

    let total: u32 = rows.iter().map(|r| r.complexity).sum();
    let avg = total as f64 / rows.len() as f64;
    let hotspot_count = rows.iter().filter(|r| r.complexity >= threshold).count();

    println!(
        "Complexity: {} symbols, avg {:.1}, {} hotspots (>= {})\n",
        rows.len(),
        avg,
        hotspot_count,
        threshold
    );

    for r in rows.iter().take(30) {
        let flag = if r.complexity >= threshold {
            " ⚠"
        } else {
            ""
        };
        println!(
            "  [{:>3}] {}  ({}:{}){}",
            r.complexity, r.name, r.file, r.start_line, flag
        );
    }
    Ok(())
}

pub(crate) fn cmd_refactor(
    root: &Path,
    target: Option<&str>,
    focus: &str,
    limit: usize,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("not initialized")?;

    let emb_path = root.join(".infigraph").join("embeddings.bin");
    let emb_ref = if emb_path.exists() {
        Some(emb_path.as_path())
    } else {
        None
    };

    let focus = infigraph_core::refactor::Focus::parse(focus);
    let recs = infigraph_core::refactor::analyze(backend, emb_ref, target, focus, limit)?;
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
    let backend = prism.backend().context("not initialized")?;
    let diagram = infigraph_core::sequence::generate_sequence_mermaid(backend, symbol_id, depth)?;
    println!("{}", diagram);
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
    let backend = prism
        .backend()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let report = if let Some(group_name) = group {
        let multi_reg = infigraph_core::multi::Registry::load()?;
        infigraph_core::review::review_with_group(
            root,
            base,
            limit,
            prism.registry(),
            backend,
            group_name,
            &multi_reg,
            bundled_registry,
        )?
    } else {
        infigraph_core::review::review(root, base, limit, prism.registry(), backend)?
    };

    if json && !llm {
        println!("{}", infigraph_core::review::format_review_json(&report));
    } else if !llm {
        print!("{}", infigraph_core::review::format_review(&report));
    }

    if llm || dry_run {
        use infigraph_core::review::llm;
        let (prompt, result) = llm::review_with_llm(root, &report, backend, dry_run, context)?;

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
    let backend = prism
        .backend()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let config_path = config
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.join(".infigraph").join("check.toml"));
    let cfg = check::load_config(&config_path)?;

    let selection = match checks {
        Some(csv) => CheckSelection::from_csv(csv),
        None => CheckSelection::all(),
    };

    let results = check::run_checks(root, &cfg, backend, &selection);

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
    let backend = prism.backend().context("graph not initialized")?;

    let deps = infigraph_core::manifest::query_deps(backend)?;
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

pub(crate) fn cmd_detect_patterns(root: &Path, pattern: Option<&str>, json: bool) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let backend = prism
        .backend()
        .context("graph not initialized -- run 'infigraph index' first")?;

    let report = infigraph_core::patterns::detect_filtered(backend, pattern)?;

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

pub(crate) fn cmd_bridges(root: &Path, kind: Option<&str>) -> Result<()> {
    let canonical = root.canonicalize().context("invalid project root")?;
    let result = infigraph_core::bridges::detect_bridges(&canonical)?;

    let bridges: Vec<_> = match kind {
        Some(k) => {
            let k_upper = k.to_uppercase();
            result
                .bridges
                .iter()
                .filter(|b| b.kind.as_str() == k_upper)
                .collect()
        }
        None => result.bridges.iter().collect(),
    };

    if bridges.is_empty() {
        let filter_note = kind.map(|k| format!(" (filter: {k})")).unwrap_or_default();
        println!("No cross-language bridges detected{}.", filter_note);
        return Ok(());
    }

    println!("Cross-language bridges: {} total", result.bridges.len());

    // Group by file
    let mut by_file: std::collections::HashMap<&str, Vec<_>> = std::collections::HashMap::new();
    for b in &bridges {
        by_file.entry(&b.file).or_default().push(b);
    }
    let mut files: Vec<&str> = by_file.keys().copied().collect();
    files.sort_unstable();

    for file in files {
        let file_bridges = &by_file[file];
        println!("\n  {}:", file);
        let mut sorted = file_bridges.to_vec();
        sorted.sort_by_key(|b| b.line);
        for b in sorted {
            let target = b.target_language.as_deref().unwrap_or("unknown");
            println!(
                "    L{} [{}] {} -> {} | {}",
                b.line,
                b.kind.as_str(),
                b.foreign_symbol,
                target,
                b.detail
            );
        }
    }

    Ok(())
}

pub(crate) fn cmd_clones(root: &Path, threshold: f64, limit: usize) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;

    let threshold_f32 = threshold as f32;
    let kinds: Vec<&str> = vec!["Function", "Method"];
    let syms = backend.symbols_with_docstring(Some(&kinds))?;

    if syms.len() < 2 {
        println!("Not enough symbols to compare. Run 'infigraph index' first.");
        return Ok(());
    }

    let embedder = infigraph_core::embed::best_embedder();
    let emb_path = root.join(".infigraph").join("embeddings.bin");

    let cached: std::collections::HashMap<String, Vec<f32>> = if emb_path.exists() {
        infigraph_core::embed::load_embeddings_cached(&emb_path)?
            .into_iter()
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    let symbol_vecs: Vec<(String, String, String, Vec<f32>)> = syms
        .iter()
        .map(|s| {
            let text = if !s.docstring.is_empty() {
                format!("{} {}: {}", s.kind, s.name, s.docstring)
            } else {
                format!("{} {}", s.kind, s.name)
            };
            let emb = cached
                .get(&s.id)
                .cloned()
                .unwrap_or_else(|| embedder.embed(&text).unwrap_or_default());
            (s.id.clone(), s.name.clone(), s.file.clone(), emb)
        })
        .filter(|(_, _, _, emb)| !emb.is_empty())
        .collect();

    let n = symbol_vecs.len();
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();

    for i in 0..n {
        for j in (i + 1)..n {
            if symbol_vecs[i].2 == symbol_vecs[j].2 {
                continue;
            }
            let sim =
                infigraph_core::embed::cosine_similarity(&symbol_vecs[i].3, &symbol_vecs[j].3);
            if sim >= threshold_f32 {
                pairs.push((sim, i, j));
            }
        }
    }

    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(limit);

    if pairs.is_empty() {
        println!(
            "No clones found above threshold {:.2} across {} symbols.",
            threshold, n
        );
        return Ok(());
    }

    for (score, i, j) in &pairs {
        let _ = backend.upsert_similar_edge(&symbol_vecs[*i].0, &symbol_vecs[*j].0, *score);
    }

    println!(
        "Clone detection: {} pairs found (threshold={:.2}, symbols={})\n",
        pairs.len(),
        threshold,
        n
    );

    for (score, i, j) in &pairs {
        let (id_a, name_a, file_a, _) = &symbol_vecs[*i];
        let (id_b, name_b, file_b, _) = &symbol_vecs[*j];
        println!(
            "  {:.3}  {} ({}) <-> {} ({})\n         {} vs {}",
            score, name_a, id_a, name_b, id_b, file_a, file_b
        );
    }

    Ok(())
}

pub(crate) fn cmd_concerns(root: &Path, kind: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let matches = infigraph_core::concerns::detect_cross_cutting(backend)?;

    let filtered: Vec<_> = if let Some(k) = kind {
        let k_lower = k.to_lowercase();
        matches
            .iter()
            .filter(|m| m.kind.to_lowercase() == k_lower)
            .cloned()
            .collect()
    } else {
        matches
    };

    println!("{}", infigraph_core::concerns::format_concerns(&filtered));
    Ok(())
}

pub(crate) fn cmd_config_bindings(
    root: &Path,
    kind: Option<&str>,
    profile: Option<&str>,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let bindings = infigraph_core::config::detect_config_bindings(backend)?;
    let canonical = root.canonicalize().context("invalid project root")?;
    let config_files = infigraph_core::config::detect_config_files(&canonical);

    let filtered: Vec<_> = bindings
        .iter()
        .filter(|b| {
            kind.as_ref()
                .is_none_or(|k| b.kind.to_lowercase() == k.to_lowercase())
                && profile
                    .as_ref()
                    .is_none_or(|p| b.profile.to_lowercase() == p.to_lowercase())
        })
        .cloned()
        .collect();

    println!(
        "{}",
        infigraph_core::config::format_config_bindings(&filtered, &config_files)
    );
    Ok(())
}

pub(crate) fn cmd_reflection(root: &Path, mechanism: Option<&str>) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let canonical = root.canonicalize().context("invalid project root")?;
    let sites = infigraph_core::reflection::detect_reflection_sites(backend, &canonical)?;

    let filtered: Vec<_> = if let Some(m) = mechanism {
        let m_lower = m.to_lowercase();
        sites
            .iter()
            .filter(|s| s.mechanism.to_lowercase() == m_lower)
            .cloned()
            .collect()
    } else {
        sites
    };

    println!(
        "{}",
        infigraph_core::reflection::format_reflection_sites(&filtered)
    );
    Ok(())
}

pub(crate) fn cmd_taint(
    root: &Path,
    category: Option<&str>,
    show_sanitized: bool,
    inter: bool,
    depth: u32,
) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let canonical = root.canonicalize().context("invalid project root")?;

    if inter {
        let flows = infigraph_core::taint::interprocedural::detect_interprocedural_taint(
            backend, &canonical, depth,
        )?;

        let filtered: Vec<_> = if let Some(c) = category {
            let c_lower = c.to_lowercase();
            flows
                .iter()
                .filter(|f| f.sink_category.to_lowercase() == c_lower)
                .cloned()
                .collect()
        } else {
            flows
        };

        println!(
            "{}",
            infigraph_core::taint::interprocedural::format_interprocedural_flows(&filtered)
        );
    } else {
        let flows = infigraph_core::taint::detect_taint_flows(backend, &canonical)?;

        let filtered: Vec<_> = flows
            .iter()
            .filter(|f| {
                category
                    .as_ref()
                    .is_none_or(|c| f.sink_category.to_lowercase() == c.to_lowercase())
                    && (show_sanitized || !f.sanitized)
            })
            .cloned()
            .collect();

        println!("{}", infigraph_core::taint::format_taint_flows(&filtered));
    }

    Ok(())
}

pub(crate) fn cmd_dynamic_urls(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let canonical = root.canonicalize().context("invalid project root")?;
    let urls = infigraph_core::taint::dynamic_urls::detect_dynamic_urls(backend, &canonical)?;

    println!(
        "{}",
        infigraph_core::taint::dynamic_urls::format_dynamic_urls(&urls)
    );
    Ok(())
}

pub(crate) fn cmd_path_traversal(root: &Path, depth: u32) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    let backend = prism.backend().context("graph not initialized")?;
    let canonical = root.canonicalize().context("invalid project root")?;
    let flows =
        infigraph_core::taint::path_traversal::detect_path_traversal(backend, &canonical, depth)?;

    println!(
        "{}",
        infigraph_core::taint::path_traversal::format_path_traversal(&flows)
    );
    Ok(())
}

pub(crate) fn cmd_bridges_promote(root: &Path) -> Result<()> {
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;
    let backend = prism
        .backend()
        .context("graph not initialized -- run 'infigraph index' first")?;

    // Find BRIDGE_TO edges where both endpoints are resolved symbols, promote to CALLS
    let bridge_rows =
        backend.raw_query("MATCH (a:Symbol)-[r:BRIDGE_TO]->(b:Symbol) RETURN a.id, b.id")?;

    if bridge_rows.is_empty() {
        println!("No BRIDGE_TO edges found to promote.");
        return Ok(());
    }

    let count = bridge_rows.len();
    for row in &bridge_rows {
        let _ = backend.raw_query(&format!(
            "MATCH (a:Symbol {{id: '{}'}})-[r:BRIDGE_TO]->(b:Symbol {{id: '{}'}}) DELETE r CREATE (a)-[:CALLS]->(b)",
            row[0].replace('\'', "\\'"),
            row[1].replace('\'', "\\'"),
        ));
    }
    println!("Promoted {} BRIDGE_TO edges to CALLS edges.", count);
    Ok(())
}
