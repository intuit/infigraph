use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::multi::{self, Registry};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use super::watch::auto_start_watch;

pub(crate) fn tool_group_list(_args: &Value) -> Result<String> {
    let registry = Registry::load()?;

    if registry.groups.is_empty() {
        return Ok("No groups defined. Use group_create to create one.".to_string());
    }

    let mut out = format!("Groups ({}):\n", registry.groups.len());
    let mut groups: Vec<_> = registry.groups.values().collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    for group in &groups {
        out.push_str(&format!(
            "  {:30} {} repos, {} contracts\n",
            group.name,
            group.repos.len(),
            group.contracts.len()
        ));
        for repo_name in &group.repos {
            if let Some(entry) = registry.repos.get(repo_name) {
                out.push_str(&format!(
                    "    - {:25} ({} symbols)\n",
                    repo_name, entry.symbol_count
                ));
            } else {
                out.push_str(&format!("    - {:25} (not registered)\n", repo_name));
            }
        }
    }
    Ok(out)
}

pub(crate) fn tool_group_create(args: &Value) -> Result<String> {
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .context("missing 'name' argument")?;

    let mut registry = Registry::load()?;
    registry.create_group(name)?;

    Ok(format!("Group '{}' created.", name))
}

pub(crate) fn tool_group_add(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let repo_name = args
        .get("repo_name")
        .and_then(|r| r.as_str())
        .context("missing 'repo_name' argument")?;

    // If the repo isn't registered yet and a path is given, register it first
    let mut registry = Registry::load()?;
    if !registry.repos.contains_key(repo_name) {
        if let Some(path_str) = args.get("path").and_then(|p| p.as_str()) {
            let lang_registry = bundled_registry()?;
            let mut prism = Infigraph::open(&PathBuf::from(path_str), lang_registry)?;
            prism.init()?;
            registry.register_repo(repo_name, &PathBuf::from(path_str), &prism)?;
        }
    }

    registry.group_add(group_name, repo_name)?;

    Ok(format!(
        "Added repo '{}' to group '{}'.",
        repo_name, group_name
    ))
}

pub(crate) fn tool_group_query(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let cypher = args
        .get("cypher")
        .and_then(|c| c.as_str())
        .context("missing 'cypher' argument")?;

    let registry = Registry::load()?;
    let results = registry.group_query(group_name, cypher, bundled_registry)?;

    if results.is_empty() {
        return Ok(format!(
            "No results across repos in group '{}'.",
            group_name
        ));
    }

    let mut out = String::new();
    for (repo_name, rows) in &results {
        out.push_str(&format!("=== {} ({} rows) ===\n", repo_name, rows.len()));
        for row in rows {
            out.push_str(&format!("  {}\n", row.join(" | ")));
        }
    }
    Ok(out)
}

pub(crate) fn tool_group_sync(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let mut registry = Registry::load()?;
    let count = multi::sync_group_contracts(&mut registry, group_name, bundled_registry)?;

    Ok(format!(
        "Extracted {} contracts from group '{}'.",
        count, group_name
    ))
}

pub(crate) fn tool_group_contracts(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;

    if group.contracts.is_empty() {
        return Ok(format!(
            "No contracts in group '{}'. Run group_sync first.",
            group_name
        ));
    }

    let mut out = format!(
        "Contracts in group '{}' ({}):\n",
        group_name,
        group.contracts.len()
    );
    for c in &group.contracts {
        out.push_str(&format!(
            "  {:?} {} {} {}  (symbol: {}, file: {})\n",
            c.kind, c.service, c.method, c.path, c.symbol_id, c.file
        ));
    }
    Ok(out)
}

pub(crate) fn tool_group_deps(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let deps = infigraph_core::multi::detect_cross_service_deps(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )?;

    if deps.is_empty() {
        return Ok(format!(
            "No cross-service dependencies found in group '{}'. Run group_sync first.",
            group_name
        ));
    }

    let mut out = format!(
        "Cross-service dependencies in group '{}' ({}):\n",
        group_name,
        deps.len()
    );
    for d in &deps {
        out.push_str(&format!(
            "  {} ({}) → {} {} {} [{}]\n",
            d.caller_service,
            d.caller_symbol,
            d.target_service,
            d.target_method,
            d.target_path,
            d.caller_file
        ));
    }
    Ok(out)
}

pub(crate) fn tool_group_index(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let full = args.get("full").and_then(|f| f.as_bool()).unwrap_or(false);

    let mut registry = Registry::load()?;
    let results = infigraph_core::multi::index_group(
        &mut registry,
        group_name,
        full,
        infigraph_languages::bundled_registry,
    )?;

    let mut out = format!(
        "Indexed {} repos in group '{}':\n",
        results.len(),
        group_name
    );
    for (repo, indexed, total) in &results {
        out.push_str(&format!("  {}: {}/{} files\n", repo, indexed, total));
    }
    let group = registry.groups.get(group_name);
    if let Some(g) = group {
        for repo_name in &g.repos {
            if let Some(entry) = registry.repos.get(repo_name) {
                let p = entry.path.to_string_lossy();
                if let Some(msg) = auto_start_watch(&p) {
                    out.push_str(&format!("{}\n", msg));
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn tool_group_link(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    let registry = Registry::load()?;
    let count = infigraph_core::multi::link_cross_service_calls(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )?;

    Ok(format!(
        "Linked {} cross-service CALLS_SERVICE edges in group '{}'.",
        count, group_name
    ))
}
