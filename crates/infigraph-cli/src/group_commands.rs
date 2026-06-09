use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use crate::GroupAction;

pub(crate) fn cmd_group(root: &Path, action: GroupAction) -> Result<()> {
    use infigraph_core::multi::Registry;

    let mut registry = Registry::load().unwrap_or_default();

    match action {
        GroupAction::Create { name } => {
            registry.create_group(&name)?;
            registry.save()?;
            println!("Created group '{}'", name);
        }
        GroupAction::Add { group, repo } => {
            let reg = bundled_registry()?;
            let mut prism = Infigraph::open(root, reg)?;
            prism.init()?;
            registry.register_repo(&repo, root, &prism)?;
            registry.group_add(&group, &repo)?;
            registry.save()?;
            println!("Added repo '{}' to group '{}'", repo, group);
        }
        GroupAction::Remove { group, repo } => {
            registry.group_remove(&group, &repo)?;
            registry.save()?;
            println!("Removed repo '{}' from group '{}'", repo, group);
        }
        GroupAction::List => {
            if registry.groups.is_empty() {
                println!("No groups defined.");
            } else {
                for (name, group) in &registry.groups {
                    println!("{}:", name);
                    for r in &group.repos {
                        println!("  - {}", r);
                    }
                }
            }
        }
        GroupAction::Index { group, full } => {
            let g = registry
                .groups
                .get(&group)
                .context(format!("group '{}' not found", group))?
                .clone();
            println!("Indexing {} repos in group '{}'...", g.repos.len(), group);
            for repo_name in &g.repos {
                let entry = registry
                    .repos
                    .get(repo_name)
                    .context(format!("repo '{}' not in registry", repo_name))?
                    .clone();
                println!("\n--- {} ({}) ---", repo_name, entry.path.display());
                if full {
                    let tg_dir = entry.path.join(".infigraph");
                    if tg_dir.exists() {
                        let sess_dir = tg_dir.join("sessions");
                        let sess_bak = entry.path.join(".infigraph-sessions-backup");
                        let had = sess_dir.exists();
                        if had {
                            let _ = std::fs::rename(&sess_dir, &sess_bak);
                        }
                        std::fs::remove_dir_all(&tg_dir)?;
                        if had {
                            std::fs::create_dir_all(&tg_dir)?;
                            let _ = std::fs::rename(&sess_bak, &sess_dir);
                        }
                        println!("  Cleaned .infigraph/ for full reindex (sessions preserved)");
                    }
                }
                let reg = bundled_registry()?;
                let mut prism = Infigraph::open(&entry.path, reg)?;
                prism.init()?;
                let result = prism.index()?;
                println!(
                    "  Indexed {}/{} files",
                    result.indexed_files, result.total_files
                );
                registry.register_repo(repo_name, &entry.path, &prism)?;
            }
            println!(
                "\nDone. All {} repos in group '{}' indexed.",
                g.repos.len(),
                group
            );
        }
        GroupAction::Combined { group } => {
            println!("Combined graph for group '{}' not yet implemented", group);
        }
        GroupAction::Sync { group } => {
            let count = infigraph_core::multi::sync_group_contracts(
                &mut registry,
                &group,
                bundled_registry,
            )?;
            println!("Synced {} contracts in group '{}'", count, group);
        }
        GroupAction::Contracts { group } => {
            let g = registry
                .groups
                .get(&group)
                .context(format!("group '{}' not found", group))?;
            if g.contracts.is_empty() {
                println!(
                    "No contracts discovered in group '{}'. Run 'infigraph group sync {}' first.",
                    group, group
                );
            } else {
                println!("Contracts in group '{}':", group);
                for c in &g.contracts {
                    println!(
                        "  {} {:>4} {:30} ({}) {}",
                        c.service, c.method, c.path, c.symbol_id, c.file
                    );
                }
            }
        }
        GroupAction::Deps { group } => {
            let deps = infigraph_core::multi::detect_cross_service_deps(
                &registry,
                &group,
                bundled_registry,
            )?;
            if deps.is_empty() {
                println!(
                    "No cross-service dependencies found in group '{}'. Run 'infigraph group sync {}' first.",
                    group, group
                );
            } else {
                println!("Cross-service dependencies in group '{}':", group);
                for d in &deps {
                    println!(
                        "  {} ({}) → {} {} {} [{}]",
                        d.caller_service,
                        d.caller_symbol,
                        d.target_service,
                        d.target_method,
                        d.target_path,
                        d.caller_file
                    );
                }
                println!("\n{} dependencies found.", deps.len());
            }
        }
        GroupAction::Link { group } => {
            let count = infigraph_core::multi::link_cross_service_calls(
                &registry,
                &group,
                bundled_registry,
            )?;
            println!(
                "Linked {} cross-service CALLS_SERVICE edges in group '{}'.",
                count, group
            );
        }
        GroupAction::Query { group, cypher } => {
            let results = registry.group_query(&group, &cypher, bundled_registry)?;
            for (repo, rows) in &results {
                println!("--- {} ---", repo);
                for row in rows {
                    println!("  {}", row.join(" | "));
                }
            }
        }
        GroupAction::Watch { group, .. } => {
            println!("Watch for group '{}' not yet implemented", group);
        }
    }

    Ok(())
}

pub(crate) fn cmd_repos() -> Result<()> {
    use infigraph_core::multi::Registry;

    let registry = Registry::load().unwrap_or_default();

    if registry.repos.is_empty() {
        println!(
            "No repositories registered. Use 'infigraph group add <group> <repo>' to register."
        );
        return Ok(());
    }

    println!("Registered repositories:");
    for (name, entry) in &registry.repos {
        println!(
            "  {} — {} ({} symbols, {} modules)",
            name,
            entry.path.display(),
            entry.symbol_count,
            entry.module_count
        );
    }

    Ok(())
}
