use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::multi::{self, combined, Registry};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use super::watch::auto_start_watch_opportunistic as auto_start_watch;

#[cfg(feature = "remote")]
fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

pub fn tool_group_list(_args: &Value) -> Result<String> {
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

pub fn tool_group_create(args: &Value) -> Result<String> {
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .context("missing 'name' argument")?;
    let org = args
        .get("org")
        .and_then(|o| o.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(infigraph_core::multi::default_org);

    let mut registry = Registry::load()?;
    let key = infigraph_core::multi::qualified_group_name(&org, name);
    registry.create_group_with_org(name, &org)?;

    Ok(format!("Group '{}' created.", key))
}

pub fn tool_group_add(args: &Value) -> Result<String> {
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

pub fn tool_group_query(args: &Value) -> Result<String> {
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

pub fn tool_group_sync(args: &Value) -> Result<String> {
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

pub fn tool_group_contracts(args: &Value) -> Result<String> {
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

pub fn tool_group_deps(args: &Value) -> Result<String> {
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

pub fn tool_group_index(args: &Value) -> Result<String> {
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
                if let Err(e) = infigraph_core::claude_md::ensure_project_claude_md(&entry.path) {
                    out.push_str(&format!(
                        "warning: CLAUDE.md update failed for {}: {e}\n",
                        repo_name
                    ));
                }
            }
        }
    }
    Ok(out)
}

pub fn tool_group_link(args: &Value) -> Result<String> {
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

pub fn tool_group_link_docs(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;

    #[cfg(feature = "remote")]
    if is_remote_mode() {
        return Ok(format!(
            "Skipped combined doc store for group '{}' — in remote mode, \
             docs are stored in shared Neo4j. Use search_docs or group_search_docs instead.",
            group_name
        ));
    }

    let registry = Registry::load()?;
    let stats = infigraph_docs::combined::build_combined_docs(&registry, group_name)?;

    Ok(format!(
        "Combined document store rebuilt for group '{}': {} documents, {} chunks, {} links ({} intra-repo, {} cross-repo), {} sources, {} embeddings.",
        group_name,
        stats.documents,
        stats.chunks,
        stats.links,
        stats.intra_repo_links,
        stats.cross_repo_links,
        stats.sources,
        stats.embeddings
    ))
}

pub fn tool_group_search(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query' argument")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
    let alpha = args.get("alpha").and_then(|a| a.as_f64()).unwrap_or(0.3) as f32;
    let deep = args.get("deep").and_then(|d| d.as_bool()).unwrap_or(false);

    #[cfg(feature = "remote")]
    if is_remote_mode() {
        let registry = Registry::load()?;
        let group = registry
            .groups
            .get(group_name)
            .context(format!("group '{}' not found", group_name))?;
        let first_repo = group
            .repos
            .first()
            .and_then(|r| registry.repos.get(r))
            .context("group has no repos")?;
        let search_args = serde_json::json!({
            "path": first_repo.path.to_string_lossy(),
            "query": query,
            "limit": limit,
            "alpha": alpha,
        });
        return super::search::tool_search(&search_args);
    }

    if deep {
        return combined::combined_search_deep(group_name, query, limit, alpha);
    }

    let results = combined::combined_search(group_name, query, limit, alpha)?;

    if results.is_empty() {
        return Ok(format!(
            "No results for '{}' in group '{}'",
            query, group_name
        ));
    }

    let mut out = format!(
        "Results for '{}' in group '{}' ({} hits, alpha={:.1}):\n",
        query,
        group_name,
        results.len(),
        alpha
    );
    for r in &results {
        let repo = combined::extract_repo(&r.symbol_id);
        out.push_str(&format!(
            "  {:.3} (bm25:{:.2} vec:{:.2})  [{}]  {}\n",
            r.score, r.bm25_score, r.vector_score, repo, r.symbol_id
        ));
    }
    Ok(out)
}

pub fn tool_group_search_docs(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .context("missing 'query' argument")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
    let alpha = args.get("alpha").and_then(|a| a.as_f64()).unwrap_or(0.5) as f32;

    #[cfg(feature = "remote")]
    if is_remote_mode() {
        let registry = Registry::load()?;
        let group = registry
            .groups
            .get(group_name)
            .context(format!("group '{}' not found", group_name))?;
        let first_repo = group
            .repos
            .first()
            .and_then(|r| registry.repos.get(r))
            .context("group has no repos")?;
        let search_args = serde_json::json!({
            "path": first_repo.path.to_string_lossy(),
            "query": query,
            "limit": limit,
        });
        return super::docs::tool_search_docs(&search_args);
    }

    let results = infigraph_docs::combined::combined_doc_search(group_name, query, limit, alpha)?;
    if results.is_empty() {
        return Ok(format!(
            "No document results for '{}' in group '{}'.",
            query, group_name
        ));
    }

    let mut out = format!(
        "Document results for '{}' in group '{}' ({} hits):\n",
        query,
        group_name,
        results.len()
    );
    for result in results {
        let repo = combined::extract_repo(&result.doc_file);
        out.push_str(&format!(
            "  {:.3} (bm25:{:.2} vec:{:.2}) [{}] {}",
            result.score, result.bm25_score, result.vector_score, repo, result.doc_file
        ));
        if let Some(heading) = result.heading {
            out.push_str(&format!(" > {heading}"));
        }
        let preview: String = result.text.chars().take(200).collect();
        out.push_str(&format!("\n      {}\n", preview.replace('\n', " ")));
    }
    Ok(out)
}

pub fn tool_group_build(args: &Value) -> Result<String> {
    let group_name = args
        .get("group_name")
        .and_then(|g| g.as_str())
        .context("missing 'group_name' argument")?;
    let full = args.get("full").and_then(|f| f.as_bool()).unwrap_or(false);

    let mut out = String::new();

    // Step 1: Index
    let mut registry = Registry::load()?;
    let results = infigraph_core::multi::index_group(
        &mut registry,
        group_name,
        full,
        infigraph_languages::bundled_registry,
    )?;
    out.push_str(&format!("Step 1/5 — Indexed {} repos:\n", results.len()));
    for (repo, indexed, total) in &results {
        out.push_str(&format!("  {}: {}/{} files\n", repo, indexed, total));
    }

    // Step 2: Sync contracts
    let contract_count = multi::sync_group_contracts(&mut registry, group_name, bundled_registry)?;
    out.push_str(&format!("Step 2/5 — {} contracts synced\n", contract_count));

    // Step 3: Link cross-service calls
    let edge_count = multi::link_cross_service_calls(&registry, group_name, bundled_registry)?;
    out.push_str(&format!(
        "Step 3/5 — {} CALLS_SERVICE edges linked\n",
        edge_count
    ));

    // Step 4: Build combined graph (skip in remote mode — shared Neo4j already namespaced)
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
        out.push_str("Step 4/5 — Skipped combined graph (shared Neo4j already namespaced)\n");
    } else {
        let (symbols, edges) = combined::build_combined_graph(&registry, group_name)?;
        out.push_str(&format!(
            "Step 4/5 — Combined graph: {} symbols, {} edges\n",
            symbols, edges
        ));
    }

    // Step 5: Index per-repo docs + embeddings.
    // In remote mode: skip combined doc store (shared Neo4j), store embeddings in pgvector.
    // In local mode: build combined Kuzu doc store, store embeddings in file.
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?
        .clone();
    let mut bfs_discovered = 0;
    for repo_name in &group.repos {
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
                let pg = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached()?;
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
        out.push_str(&format!(
            "Step 5/5 — Indexed docs for {} repos, {} BFS discoveries, embeddings in pgvector\n",
            group.repos.len(),
            bfs_discovered
        ));
    } else {
        let doc_stats = infigraph_docs::combined::build_combined_docs(&registry, group_name)?;
        out.push_str(&format!(
            "Step 5/5 — Combined documents: {} docs, {} chunks, {} links ({} intra-repo, {} cross-repo), {} sources, {} BFS discoveries, {} embeddings\n",
            doc_stats.documents,
            doc_stats.chunks,
            doc_stats.links,
            doc_stats.intra_repo_links,
            doc_stats.cross_repo_links,
            doc_stats.sources,
            bfs_discovered,
            doc_stats.embeddings
        ));
    }

    // Start watchers + CLAUDE.md (skip watchers in remote mode — reindex via webhooks)
    if !is_remote {
        if let Some(group) = registry.groups.get(group_name) {
            for repo_name in &group.repos {
                if let Some(entry) = registry.repos.get(repo_name) {
                    let p = entry.path.to_string_lossy();
                    auto_start_watch(&p);
                    let _ = infigraph_core::claude_md::ensure_project_claude_md(&entry.path);
                }
            }
        }
    }

    out.push_str(
        "\nReady. Use group_search for code or group_search_docs for documents across all repos.",
    );
    Ok(out)
}
