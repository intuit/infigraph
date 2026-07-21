use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use crate::GroupAction;

pub(crate) fn cmd_group(root: &Path, action: GroupAction) -> Result<()> {
    use infigraph_core::multi::Registry;

    let mut registry = Registry::load().unwrap_or_default();

    match action {
        GroupAction::Create { name, org } => {
            let org = org.unwrap_or_else(infigraph_core::multi::default_org);
            let key = infigraph_core::multi::qualified_group_name(&org, &name);
            registry.create_group_with_org(&name, &org)?;
            registry.save()?;
            println!("Created group '{}'", key);
        }
        GroupAction::Add { group, repo } => {
            let repo_path = std::path::Path::new(&repo);
            let (repo_name, actual_path) = if repo_path.is_dir() {
                let name = repo_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                (name, repo_path.to_path_buf())
            } else {
                (repo.clone(), root.to_path_buf())
            };
            let reg = bundled_registry()?;
            let mut prism = Infigraph::open(&actual_path, reg)?;
            prism.init()?;
            registry.register_repo(&repo_name, &actual_path, &prism)?;
            registry.group_add(&group, &repo_name)?;
            registry.save()?;
            let resolved = registry
                .repos
                .get(&repo_name)
                .map(|e| e.path.display().to_string())
                .unwrap_or_else(|| actual_path.display().to_string());
            println!(
                "Added repo '{}' ({}) to group '{}'",
                repo_name, resolved, group
            );
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
            // Warn on duplicate resolved paths
            let mut seen_paths: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for repo_name in &g.repos {
                if let Some(entry) = registry.repos.get(repo_name) {
                    let resolved = std::fs::canonicalize(&entry.path)
                        .unwrap_or_else(|_| entry.path.clone())
                        .display()
                        .to_string();
                    if let Some(prev) = seen_paths.get(&resolved) {
                        eprintln!(
                            "WARNING: repos '{}' and '{}' resolve to the same path: {}",
                            prev, repo_name, resolved
                        );
                    } else {
                        seen_paths.insert(resolved, repo_name.clone());
                    }
                }
            }

            let mut skipped = 0usize;
            println!("Indexing {} repos in group '{}'...", g.repos.len(), group);
            for repo_name in &g.repos {
                let entry = registry
                    .repos
                    .get(repo_name)
                    .context(format!("repo '{}' not in registry", repo_name))?
                    .clone();
                println!("\n--- {} ({}) ---", repo_name, entry.path.display());

                if !full {
                    let current_commit = infigraph_core::multi::git_head_commit(&entry.path);
                    let unchanged = match (&entry.last_indexed_commit, &current_commit) {
                        (Some(prev), Some(curr)) => prev == curr,
                        _ => false,
                    };
                    if unchanged {
                        println!("  Skipped (unchanged at {})", current_commit.unwrap());
                        skipped += 1;
                        continue;
                    }
                }

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
                if let Some(backend) = prism.backend() {
                    let _ = infigraph_core::manifest::index_manifests(&entry.path, backend);
                }
                println!(
                    "  Indexed {}/{} files",
                    result.indexed_files, result.total_files
                );
                if result.indexed_files == 0 && result.total_files > 0 {
                    eprintln!(
                        "  WARNING: 0 files indexed for '{}' — path may be incorrect: {}",
                        repo_name,
                        entry.path.display()
                    );
                }
                registry.register_repo(repo_name, &entry.path, &prism)?;
            }
            if skipped > 0 {
                println!(
                    "\n  ({} repos skipped — unchanged since last index)",
                    skipped
                );
            }
            // Auto-start watcher for each repo in group
            for repo_name in &g.repos {
                if let Some(entry) = registry.repos.get(repo_name) {
                    crate::index::ensure_watcher_running(&entry.path);
                }
            }
            println!(
                "\nDone. All {} repos in group '{}' indexed.",
                g.repos.len(),
                group
            );
        }
        GroupAction::Combined { group } => {
            let g = registry
                .groups
                .get(&group)
                .context(format!("group '{}' not found", group))?;
            println!(
                "Building combined graph for group '{}' ({} repos)...",
                group,
                g.repos.len()
            );
            let (symbols, edges) =
                infigraph_core::multi::combined::build_combined_graph(&registry, &group)?;
            println!(
                "Combined graph ready: {} symbols, {} edges (including cross-repo).",
                symbols, edges
            );
            println!(
                "Query with: infigraph group query {} '<cypher>' --combined",
                group
            );
        }
        GroupAction::CombinedDocs { group } => {
            let stats = infigraph_docs::combined::build_combined_docs(&registry, &group)?;
            println!(
                "Combined document store ready: {} documents, {} chunks, {} links ({} intra-repo, {} cross-repo), {} sources, {} embeddings.",
                stats.documents,
                stats.chunks,
                stats.links,
                stats.intra_repo_links,
                stats.cross_repo_links,
                stats.sources,
                stats.embeddings
            );
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
        GroupAction::Query {
            group,
            cypher,
            combined,
        } => {
            if combined {
                let rows = infigraph_core::multi::combined::combined_query(&group, &cypher)?;
                for row in &rows {
                    println!("  {}", row.join(" | "));
                }
            } else {
                let results = registry.group_query(&group, &cypher, bundled_registry)?;
                for (repo, rows) in &results {
                    println!("--- {} ---", repo);
                    for row in rows {
                        println!("  {}", row.join(" | "));
                    }
                }
            }
        }
        GroupAction::Build { group, full } => {
            // Step 1: Index (uses index_group for namespace isolation + parallel on Neo4j)
            let results =
                infigraph_core::multi::index_group(&mut registry, &group, full, bundled_registry)?;
            println!("=== Step 1/5: Indexed {} repos ===", results.len());
            for (repo, indexed, total) in &results {
                println!("  {}: {}/{} files", repo, indexed, total);
            }

            // Step 2: Sync contracts
            println!("=== Step 2/5: Syncing contracts ===");
            let contract_count = infigraph_core::multi::sync_group_contracts(
                &mut registry,
                &group,
                bundled_registry,
            )?;
            registry.save()?;
            println!("  {} contracts", contract_count);

            // Step 3: Link cross-service calls
            println!("=== Step 3/5: Linking cross-service calls ===");
            let edge_count = infigraph_core::multi::link_cross_service_calls(
                &registry,
                &group,
                bundled_registry,
            )?;
            println!("  {} CALLS_SERVICE edges", edge_count);

            // Step 4: Build combined graph (skip on Neo4j — shared instance already namespaced)
            let is_remote = {
                #[cfg(feature = "remote")]
                {
                    std::env::var("INFIGRAPH_BACKEND")
                        .map(|v| v == "neo4j")
                        .unwrap_or(false)
                }
                #[cfg(not(feature = "remote"))]
                {
                    false
                }
            };

            if is_remote {
                println!(
                    "=== Step 4/5: Skipped combined graph (shared Neo4j already namespaced) ==="
                );
            } else {
                println!("=== Step 4/5: Building combined graph ===");
                let (symbols, edges) =
                    infigraph_core::multi::combined::build_combined_graph(&registry, &group)?;
                println!("  {} symbols, {} edges", symbols, edges);
            }

            // Step 5: Index docs + embeddings
            let g = registry
                .groups
                .get(&group)
                .context(format!("group '{}' not found", group))?
                .clone();
            let mut bfs_discovered = 0;
            for repo_name in &g.repos {
                let entry = registry
                    .repos
                    .get(repo_name)
                    .context(format!("repo '{}' not in registry", repo_name))?;
                let mut idx = infigraph_docs::DocIndex::open(&entry.path)?;
                if is_remote {
                    idx.set_skip_file_embeddings(true);
                }
                idx.init()?;
                let result = idx.index()?;
                bfs_discovered += result.bfs_discovered;

                #[cfg(feature = "remote")]
                if is_remote {
                    if let Some(store) = idx.store() {
                        let pg =
                            infigraph_core::meta::PostgresMetaStore::connect_from_env_cached()?;
                        pg.init_schema()?;
                        let chunk_refs: Vec<&infigraph_docs::chunk::Chunk> =
                            result.new_chunks.iter().collect();
                        let changed_refs: Vec<&str> =
                            result.changed_files.iter().map(|s| s.as_str()).collect();
                        let _ = infigraph_docs::embed::update_doc_embeddings_remote(
                            store,
                            &pg,
                            &chunk_refs,
                            &changed_refs,
                        )?;
                    }
                }
            }

            if is_remote {
                println!(
                    "=== Step 5/5: Indexed docs for {} repos, {} BFS discoveries, embeddings in pgvector ===",
                    g.repos.len(),
                    bfs_discovered
                );
            } else {
                println!("=== Step 5/5: Building combined document store ===");
                let doc_stats = infigraph_docs::combined::build_combined_docs(&registry, &group)?;
                println!(
                    "  {} documents, {} chunks, {} doc links ({} intra-repo, {} cross-repo), {} sources, {} BFS discoveries, {} embeddings",
                    doc_stats.documents,
                    doc_stats.chunks,
                    doc_stats.links,
                    doc_stats.intra_repo_links,
                    doc_stats.cross_repo_links,
                    doc_stats.sources,
                    bfs_discovered,
                    doc_stats.embeddings
                );
            }

            println!("Done.");
        }
        GroupAction::Search {
            group,
            query,
            limit,
            alpha,
            deep,
        } => {
            if deep {
                let output = infigraph_core::multi::combined::combined_search_deep(
                    &group, &query, limit, alpha,
                )?;
                print!("{}", output);
            } else {
                let results =
                    infigraph_core::multi::combined::combined_search(&group, &query, limit, alpha)?;
                if results.is_empty() {
                    println!("No results for '{}' in group '{}'", query, group);
                } else {
                    println!(
                        "Results for '{}' in group '{}' (alpha={:.1}):",
                        query, group, alpha
                    );
                    for r in &results {
                        let repo = infigraph_core::multi::combined::extract_repo(&r.symbol_id);
                        println!(
                            "  {:.3} (bm25:{:.2} vec:{:.2})  [{}]  {}",
                            r.score, r.bm25_score, r.vector_score, repo, r.symbol_id
                        );
                        if let Some(doc) = &r.docstring {
                            if !doc.is_empty() {
                                let preview: String = doc.chars().take(80).collect();
                                println!("         {}", preview);
                            }
                        }
                    }
                }
            }
        }
        GroupAction::SearchDocs {
            group,
            query,
            limit,
            alpha,
        } => {
            let results =
                infigraph_docs::combined::combined_doc_search(&group, &query, limit, alpha)?;
            if results.is_empty() {
                println!("No document results for '{}' in group '{}'", query, group);
            } else {
                println!("Document results for '{}' in group '{}':", query, group);
                for result in results {
                    let repo = infigraph_core::multi::combined::extract_repo(&result.doc_file);
                    println!(
                        "  {:.3} (bm25:{:.2} vec:{:.2}) [{}] {}",
                        result.score, result.bm25_score, result.vector_score, repo, result.doc_file
                    );
                    let preview: String = result.text.chars().take(120).collect();
                    println!("         {}", preview.replace('\n', " "));
                }
            }
        }
        GroupAction::Watch { group, debounce } => {
            let g = registry
                .groups
                .get(&group)
                .context(format!("group '{}' not found", group))?
                .clone();

            if g.repos.is_empty() {
                println!("Group '{}' has no repos.", group);
                return Ok(());
            }

            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            ctrlc::set_handler(move || {
                let _ = stop_tx.send(());
            })
            .ok();

            println!(
                "Watching {} repos in group '{}' (debounce {}ms) — Ctrl-C to stop",
                g.repos.len(),
                group,
                debounce
            );

            let mut handles = Vec::new();
            for repo_name in &g.repos {
                let entry = registry
                    .repos
                    .get(repo_name)
                    .context(format!("repo '{}' not in registry", repo_name))?
                    .clone();

                let lock_path = entry.path.join(".infigraph").join("watch.lock");
                let lock = crate::info_commands::acquire_watch_lock(&lock_path)?;

                let repo_root = entry.path.clone();
                let repo_label = repo_name.clone();
                let thread_label = repo_name.clone();
                let (local_stop_tx, local_stop_rx) = std::sync::mpsc::channel();
                handles.push((repo_label, local_stop_tx, lock));

                std::thread::spawn(move || {
                    let err_label = thread_label.clone();
                    let res = infigraph_core::watch::watch_project(
                        &repo_root,
                        bundled_registry,
                        debounce,
                        local_stop_rx,
                        move |evt| {
                            println!("[watch:{}] {evt}", thread_label);
                        },
                    );
                    if let Err(e) = res {
                        eprintln!("[watch:{}] error: {e}", err_label);
                    }
                });
            }

            // Block until Ctrl-C
            let _ = stop_rx.recv();

            // Signal all repo watchers to stop
            for (name, tx, _lock) in &handles {
                let _ = tx.send(());
                println!("Stopping watcher for {}...", name);
            }

            // Give watchers time to exit
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!("All watchers stopped.");
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
