use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::embed;

use super::docs::auto_start_doc_watch_opportunistic as auto_start_doc_watch;
use super::helpers::{find_infigraph_cli, open_prism};
use super::watch::auto_start_watch_opportunistic as auto_start_watch;

#[cfg(feature = "remote")]
fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

pub fn tool_index_project(args: &Value) -> Result<String> {
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let full = args.get("full").and_then(|f| f.as_bool()).unwrap_or(false);

    if let Some(cli) = find_infigraph_cli() {
        let mut cmd = std::process::Command::new(&cli);
        cmd.arg("index").current_dir(path);
        if full {
            cmd.arg("--full");
        }

        let output = cmd
            .output()
            .with_context(|| format!("Failed to run {}", cli.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        if !output.status.success() {
            return Err(anyhow::anyhow!("infigraph index failed:\n{}", combined));
        }
        let mut out = combined;

        // Register in global registry so watchers auto-start on next MCP init
        if let Ok(prism) = open_prism(args) {
            let project_name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());
            let mut registry = infigraph_core::multi::Registry::load().unwrap_or_default();
            let _ = registry.register_repo(&project_name, &std::path::PathBuf::from(path), &prism);
        }

        if let Some(msg) = auto_start_watch(path) {
            out.push_str(&format!("\n{}", msg));
        }
        auto_start_doc_watch(path);
        return Ok(out);
    }

    // Fallback: run inline if CLI not found
    let prism = open_prism(args)?;
    let result = prism.index()?;

    let mut out = if result.indexed_files == 0 {
        format!(
            "All {} files up-to-date, nothing to reindex\n",
            result.total_files
        )
    } else {
        format!(
            "Indexed {} files ({} up-to-date, {} total)\n",
            result.indexed_files,
            result.total_files - result.indexed_files,
            result.total_files
        )
    };
    let mut by_lang: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for ext in &result.extractions {
        let entry = by_lang.entry(&ext.language).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += ext.symbols.len();
    }
    for (lang, (files, symbols)) in &by_lang {
        out.push_str(&format!(
            "  {}: {} files, {} symbols\n",
            lang, files, symbols
        ));
    }
    if result.resolve_stats.total_calls > 0 {
        out.push_str(&format!("{}\n", result.resolve_stats));
    }
    if let Some(backend) = prism.backend() {
        let root = std::path::PathBuf::from(path);
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        #[allow(unused_mut)]
        let mut embed_done = false;
        #[cfg(feature = "remote")]
        if is_remote_mode() {
            if let Ok(pg) = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached() {
                match embed::update_embeddings_remote(backend, pg, &changed) {
                    Ok(n) => out.push_str(&format!("Saved {} embeddings to pgvector\n", n)),
                    Err(e) => {
                        out.push_str(&format!("warning: remote embedding update failed: {e}\n"))
                    }
                }
                embed_done = true;
            }
        }
        if !embed_done {
            match embed::update_embeddings(backend, &root, &changed) {
                Ok(n) => out.push_str(&format!("Saved {} embeddings\n", n)),
                Err(e) => out.push_str(&format!("warning: embedding update failed: {e}\n")),
            }
        }
    }
    let stats = prism.stats()?;
    out.push_str(&format!("\n{}", stats));

    // Register in global registry so watchers auto-start on next MCP init
    {
        let project_name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        let mut registry = infigraph_core::multi::Registry::load().unwrap_or_default();
        let _ = registry.register_repo(&project_name, &std::path::PathBuf::from(path), &prism);
    }

    if let Some(msg) = auto_start_watch(path) {
        out.push_str(&format!("\n{}", msg));
    }
    auto_start_doc_watch(path);
    if let Err(e) = infigraph_core::claude_md::ensure_project_claude_md(std::path::Path::new(path))
    {
        out.push_str(&format!(
            "\nwarning: failed to update project CLAUDE.md: {e}"
        ));
    }
    Ok(out)
}

pub fn tool_get_dependencies(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;
    let eco_filter = args.get("ecosystem").and_then(|v| v.as_str());

    let mut deps = infigraph_core::manifest::query_deps(backend)?;
    if let Some(eco) = eco_filter {
        deps.retain(|d| d.ecosystem == eco);
    }

    if deps.is_empty() {
        return Ok("No dependencies found. Run index_manifests first.".to_string());
    }

    let mut out = format!("Dependencies ({}):\n\n", deps.len());
    let mut cur_eco = String::new();
    for d in &deps {
        if d.ecosystem != cur_eco {
            out.push_str(&format!("## {} \n", d.ecosystem));
            cur_eco = d.ecosystem.clone();
        }
        let dev_tag = if d.is_dev { " [dev]" } else { "" };
        out.push_str(&format!("  {}@{}{}\n", d.name, d.version, dev_tag));
    }
    Ok(out)
}

pub fn tool_scip_import(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let index_rel = args
        .get("index")
        .and_then(|v| v.as_str())
        .unwrap_or("index.scip");
    let index_path = if std::path::Path::new(index_rel).is_absolute() {
        std::path::PathBuf::from(index_rel)
    } else {
        root.join(index_rel)
    };

    let stats = backend.import_scip_index(&index_path, Some(&root))?;
    let mut out = format!(
        "SCIP import complete:\n  files processed: {}\n  symbols added: {}\n  symbols enriched: {}\n  relations added: {}\n  references added: {}\n  corrections learned: {}",
        stats.files_processed,
        stats.symbols_added,
        stats.symbols_enriched,
        stats.relations_added,
        stats.references_added,
        stats.corrections_learned,
    );
    let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    if let Some(msg) = auto_start_watch(path) {
        out.push_str(&format!("\n{}", msg));
    }
    Ok(out)
}
