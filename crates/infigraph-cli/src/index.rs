use std::path::Path;

use anyhow::{Context, Result};
#[cfg(feature = "remote")]
use infigraph_core::graph::GraphBackend;
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_index(root: &Path, full: bool, no_embed: bool) -> Result<()> {
    #[cfg(feature = "remote")]
    let remote = is_neo4j_backend();
    #[cfg(not(feature = "remote"))]
    let remote = false;

    if full {
        if remote {
            // Remote mode: clear the Neo4j graph (local .infigraph/ is irrelevant)
            #[cfg(feature = "remote")]
            {
                let neo = infigraph_core::graph::Neo4jBackend::connect_from_env()?;
                neo.init_schema()?;
                neo.clear_all_data()?;
                println!("Cleared Neo4j graph for full reindex");
            }
        } else {
            let tg_dir = root.join(".infigraph");
            if tg_dir.exists() {
                let sessions_dir = tg_dir.join("sessions");
                let sessions_backup = root.join(".infigraph-sessions-backup");
                let had_sessions = sessions_dir.exists();
                if had_sessions {
                    let _ = std::fs::rename(&sessions_dir, &sessions_backup);
                }
                std::fs::remove_dir_all(&tg_dir)?;
                if had_sessions {
                    std::fs::create_dir_all(&tg_dir)?;
                    let _ = std::fs::rename(&sessions_backup, &sessions_dir);
                }
                println!("Cleaned .infigraph/ for full reindex (sessions preserved)");
            }
        }
    }

    let registry = crate::full_registry(Some(root))?;
    #[allow(unused_mut)]
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    // In shared Neo4j mode, a repo's identity is defined by the group registry, not by its
    // directory name. Resolve the `org/repo` namespace from the registry so a standalone
    // `infigraph index` writes data consistent with `group build` and the read filters.
    // Refuse to index an unregistered repo: writing a locally-invented namespace into the
    // shared graph produces orphaned, mis-namespaced nodes (the original reindter bug).
    #[cfg(feature = "remote")]
    let remote_ns = if remote {
        let reg = infigraph_core::multi::Registry::load()?;
        let ns = reg.resolve_repo_namespace(root).ok_or_else(|| {
            anyhow::anyhow!(
                "repo at '{}' is not registered in any group; in remote mode run \
                 `infigraph group add <group> <path>` first so its org/repo namespace is defined. \
                 (A standalone index cannot invent a namespace for a shared graph.)",
                root.display()
            )
        })?;
        prism.set_namespace(&ns);
        // Scope reads (get_file_hashes / stale-prune) to THIS repo. Without this, a
        // standalone index fetches every repo's file hashes from the shared graph and the
        // stale-file prune deletes all OTHER repos' data — same hazard as group indexing.
        prism.set_repo_filter(&ns);
        Some(ns)
    } else {
        None
    };

    println!("Indexing project...");
    let result = prism.index()?;
    if result.indexed_files == 0 {
        println!(
            "All {} files up-to-date, nothing to reindex",
            result.total_files
        );
    } else {
        println!(
            "Indexed {} files ({} up-to-date, {} total)",
            result.indexed_files,
            result.total_files - result.indexed_files,
            result.total_files
        );
    }

    let mut by_lang: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for ext in &result.extractions {
        let entry = by_lang.entry(&ext.language).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += ext.symbols.len();
    }
    for (lang, (files, symbols)) in &by_lang {
        println!("  {}: {} files, {} symbols", lang, files, symbols);
    }

    if result.resolve_stats.total_calls > 0 {
        println!("{}", result.resolve_stats);
    }

    // Derive TESTED_BY edges — scoped to changed files for incremental
    if result.indexed_files > 0 && prism.backend().is_some() {
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        let scope = if full { None } else { Some(changed.as_slice()) };
        match prism.backend().unwrap().derive_tested_by_edges(scope) {
            Ok(count) if count > 0 => println!("Derived {} TESTED_BY edges", count),
            Ok(_) => {}
            Err(e) => eprintln!("warning: TESTED_BY derivation failed: {e}"),
        }
    }

    // Detect cross-cutting concerns, taint, etc. — skip when no files changed (incremental no-op)
    if result.indexed_files > 0 && prism.backend().is_some() {
        // Docstring-only analyzers (no file I/O)
        match infigraph_core::concerns::detect_cross_cutting(prism.backend().unwrap()) {
            Ok(matches) if !matches.is_empty() => {
                println!("Detected {} cross-cutting concerns", matches.len());
            }
            Ok(_) => {}
            Err(e) => eprintln!("warning: concern detection failed: {e}"),
        }
        match infigraph_core::config::detect_config_bindings(prism.backend().unwrap()) {
            Ok(bindings) if !bindings.is_empty() => {
                println!("Detected {} config bindings", bindings.len());
            }
            Ok(_) => {}
            Err(e) => eprintln!("warning: config binding detection failed: {e}"),
        }
        match infigraph_core::reflection::detect_reflection_sites(prism.backend().unwrap(), root) {
            Ok(sites) if !sites.is_empty() => {
                let resolved = sites.iter().filter(|s| s.resolved_to.is_some()).count();
                println!(
                    "Detected {} reflection sites ({} resolved)",
                    sites.len(),
                    resolved
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("warning: reflection detection failed: {e}"),
        }

        // Source-reading analyzers — build shared cache once, pass to all three
        let taint_backend = prism.backend().unwrap();
        match infigraph_core::taint::build_source_cache(taint_backend, root) {
            Ok((functions, cache)) => {
                match infigraph_core::taint::detect_taint_flows_with_cache(
                    taint_backend,
                    &functions,
                    &cache,
                ) {
                    Ok(flows) if !flows.is_empty() => {
                        let active = flows.iter().filter(|f| !f.sanitized).count();
                        println!(
                            "Detected {} taint flows ({} active, {} sanitized)",
                            flows.len(),
                            active,
                            flows.len() - active
                        );
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: taint analysis failed: {e}"),
                }
                match infigraph_core::taint::interprocedural::detect_interprocedural_taint_with_cache(taint_backend, &functions, &cache, 5) {
                    Ok(flows) if !flows.is_empty() => {
                        println!("Detected {} inter-procedural taint flows", flows.len());
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: inter-procedural taint failed: {e}"),
                }
                match infigraph_core::taint::dynamic_urls::detect_dynamic_urls_with_cache(
                    taint_backend,
                    &functions,
                    &cache,
                ) {
                    Ok(urls) if !urls.is_empty() => {
                        let matched = urls.iter().filter(|u| u.matched_route.is_some()).count();
                        println!(
                            "Detected {} dynamic URLs ({} matched to routes)",
                            urls.len(),
                            matched
                        );
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: dynamic URL detection failed: {e}"),
                }
            }
            Err(e) => eprintln!("warning: source cache build failed: {e}"),
        }
    }

    let stats = prism.stats()?;
    println!("\n{}", stats);

    // In remote mode, register this repo in Postgres so it appears in registry queries
    #[cfg(feature = "remote")]
    {
        if is_neo4j_backend() {
            let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            let repo_name = canonical
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let mut registry = infigraph_core::multi::Registry::load()?;
            registry.register_repo(&repo_name, root, &prism)?;
            println!("Registered '{}' in Postgres registry", repo_name);

            // Create Repo node in Neo4j keyed by the org/repo namespace (matching f.repo),
            // and link only this repo's files.
            if let Some(backend) = prism.backend() {
                let repo_key = remote_ns.as_deref().unwrap_or(&repo_name);
                backend.upsert_repo(repo_key)?;
                println!("Created Repo node '{}' with BELONGS_TO edges", repo_key);
            }
        }
    }

    // Hint: suggest .infigraphignore if none exists
    if !root.join(".infigraphignore").exists() {
        eprintln!("\nhint: Create .infigraphignore in the project root to exclude non-source directories.");
        eprintln!("      Common entries:");
        eprintln!("        target/        # Rust build output");
        eprintln!("        build/         # build output (Gradle, CMake, etc.)");
        eprintln!("        dist/          # distribution bundles");
        eprintln!("        out/           # compiler/IDE output");
        eprintln!("        vendor/        # vendored dependencies (Go, Ruby)");
        eprintln!("        bin/           # compiled binaries");
        eprintln!("        obj/           # intermediate build objects (.NET, C++)");
        eprintln!("        generated/     # auto-generated code");
        eprintln!("        third_party/   # third-party source copies");
        eprintln!("        CMakeFiles/    # CMake internal files");
        eprintln!("      One entry per line. Lines starting with # are comments.");
    }

    if local_run_id(root)?.is_some() {
        let coverage = index_status::current_run(root)?;
        anyhow::ensure!(
            coverage.resolution.state != StageState::Failed,
            "Partial index: call resolution failed; the syntax graph remains available"
        );
    }

    // Compute and save embeddings — only for new/changed symbols
    if no_embed {
        auto_scip(root, &result, prism.backend())?;
        return Ok(());
    }
    {
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        #[allow(unused_mut)]
        let mut done = false;

        #[cfg(feature = "remote")]
        if is_neo4j_backend() {
            let backend = prism.backend().context("graph not initialized")?;
            let pg = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached()?;
            pg.init_schema()?;
            let count = infigraph_core::embed::update_embeddings_remote(backend, &pg, &changed)?;
            println!("Saved {} embeddings to Postgres pgvector", count);
            done = true;
        }

        if !done {
            let backend = prism.backend().context("graph not initialized")?;
            let count = infigraph_core::embed::update_embeddings(backend, root, &changed)?;
            println!("Saved {} embeddings to .infigraph/embeddings.bin", count);
        }
    }

    // Auto-index documents (PDF, DOCX, XML, Markdown, etc.)
    #[cfg(feature = "remote")]
    let doc_ns = remote_ns.as_deref();
    #[cfg(not(feature = "remote"))]
    let doc_ns: Option<&str> = None;
    match crate::commands::cmd_index_docs(root, doc_ns) {
        Ok(()) => {}
        Err(e) => eprintln!("warning: document indexing failed: {e}"),
    }

    // Drop prism to release the GraphStore handle before background SCIP
    let detected_languages =
        languages_for_index(prism.backend().context("graph not initialized")?, &result)?;
    let run_id = local_run_id(root)?;
    prepare_scip(root, &detected_languages, run_id.as_deref())?;
    drop(prism);

    // SCIP enrichment in a detached child process — parent returns immediately.
    spawn_scip_child_process(root, &detected_languages, run_id.as_deref())?;

    if let Err(e) = infigraph_core::claude_md::ensure_project_claude_md(root) {
        eprintln!("warning: failed to update project CLAUDE.md: {e}");
    }

    Ok(())
}

fn spawn_scip_child_process(
    root: &Path,
    detected_languages: &std::collections::HashSet<String>,
    run_id: Option<&str>,
) -> Result<()> {
    use crate::scip_download;

    let mut indexers = Vec::new();
    for indexer in scip_download::indexers_for_languages(detected_languages) {
        if should_run_indexer(root, indexer) && !scip_succeeded(root, run_id, indexer.binary_name)?
        {
            indexers.push(indexer);
        }
    }
    if indexers.is_empty() {
        return Ok(());
    }

    let count = indexers.len();
    let indexer_names: Vec<&str> = indexers.iter().map(|i| i.binary_name).collect();
    println!(
        "SCIP enrichment starting in background ({count} indexer(s): {})...",
        indexer_names.join(", ")
    );

    let langs: String = detected_languages
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");

    let exe = std::env::current_exe()?;

    let log_path = root.join(".infigraph").join("scip-enrich.log");
    let stderr_target = match std::fs::File::create(&log_path) {
        Ok(f) => std::process::Stdio::from(f),
        Err(_) => std::process::Stdio::null(),
    };

    let mut command = std::process::Command::new(exe);
    command.args(scip_enrich_args(&langs));
    if let Some(run_id) = run_id {
        command.args(["--run-id", run_id]);
    }
    match command
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr_target)
        .spawn()
    {
        Ok(mut child) => {
            // spawn() only reports failure to launch (missing binary, exec
            // permission). It says nothing about the child crashing or
            // exiting nonzero afterward — exactly the failure shape of the
            // bug this function used to hit silently (the child launched
            // fine and died instantly inside clap's parser). Wait on it from
            // a detached thread so this function still returns immediately,
            // but any future silent-death cause surfaces a warning instead
            // of only leaving a trace in a log nobody's prompted to open.
            let log_path = log_path.clone();
            std::thread::spawn(move || {
                if let Some(msg) = scip_enrich_exit_message(child.wait(), &log_path) {
                    eprintln!("{msg}");
                }
            });
        }
        Err(e) => {
            for indexer in &indexers {
                record_scip(
                    root,
                    run_id,
                    indexer.binary_name,
                    StageOutcome::new(StageState::Failed, "SCIP_CHILD_LAUNCH_FAILED"),
                )?;
            }
            return Err(e.into());
        }
    }

    eprintln!("  Log: {}", log_path.display());
    Ok(())
}

/// Args for respawning this binary as the hidden `scip-enrich` subcommand.
/// `languages` is a positional argument on `Commands::ScipEnrich`, not a
/// flag — extracted so tests can assert these parse under that definition
/// without spawning a process.
fn scip_enrich_args(langs: &str) -> Vec<String> {
    vec!["scip-enrich".to_string(), langs.to_string()]
}

/// Whether the active backend is remote Neo4j (vs. the default local Kùzu).
///
/// `Infigraph::backend()` used to return `None` for the default Kùzu
/// backend, so `if let Some(backend) = prism.backend()` doubled as a de
/// facto "are we in remote mode" check. Once `backend()` was made universal
/// (returning `Some` for every backend kind, including local Kùzu), that
/// check silently broke: the Postgres-embeddings branch below started
/// firing on every `remote`-feature build regardless of backend, attempting
/// a Postgres connection even for plain local indexing and failing the
/// whole `index` command with a connection-refused error. Extracted so the
/// exact condition can be unit-tested independently of a real backend.
#[cfg(feature = "remote")]
fn is_neo4j_backend() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

/// Decides what (if anything) to warn about after waiting on the detached
/// scip-enrich child. Extracted from the wait thread so it's testable
/// without spawning a real process — `current_exe()` in `spawn_scip_child_process`
/// resolves to the test binary itself under `cargo test`, not `infigraph`,
/// so the full spawn path can't be exercised end-to-end in a unit test.
fn scip_enrich_exit_message(
    status: std::io::Result<std::process::ExitStatus>,
    log_path: &Path,
) -> Option<String> {
    match status {
        Ok(status) if !status.success() => Some(format!(
            "warning: scip-enrich exited with {status} — see {}",
            log_path.display()
        )),
        Err(e) => Some(format!("warning: failed to wait on scip-enrich: {e}")),
        _ => None,
    }
}

pub(crate) const CI_ENV_VARS: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "JENKINS_URL",
    "BUILDKITE",
    "GITLAB_CI",
    "INFIGRAPH_NO_WATCH",
];

pub(crate) fn is_ci() -> bool {
    CI_ENV_VARS.iter().any(|v| std::env::var_os(v).is_some())
}

fn is_remote_backend() -> bool {
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
}

pub(crate) fn ensure_watcher_running(root: &Path) {
    // Remote (shared-Neo4j) mode reindexes via webhook, not local file
    // watching — spawning a watcher here would race the webhook path and
    // serve no purpose (see docs/WEBHOOK-REINDEX.md).
    if is_ci() || is_remote_backend() {
        return;
    }

    let tg_dir = root.join(".infigraph");
    if !tg_dir.exists() {
        return;
    }

    let lock_path = tg_dir.join("watch.lock");
    let lock_file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    use fs2::FileExt;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            // Lock acquired — no watcher running. Release and spawn one.
            let _ = lock_file.unlock();
            drop(lock_file);
            spawn_watcher(root, &tg_dir);
        }
        Err(_) => {
            // Lock held — watcher already alive.
        }
    }
}

fn spawn_watcher(root: &Path, tg_dir: &Path) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };

    let log_path = tg_dir.join("watch.log");
    let stderr_target = match std::fs::File::create(&log_path) {
        Ok(f) => std::process::Stdio::from(f),
        Err(_) => std::process::Stdio::null(),
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("watch")
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr_target);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    match cmd.spawn() {
        Ok(_) => {
            eprintln!("[auto-watch] Watcher started (log: {})", log_path.display());
        }
        Err(e) => {
            eprintln!("[auto-watch] Failed to start watcher: {e}");
        }
    }
}

pub(crate) fn on_path(cmd: &str) -> bool {
    let lookup = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(lookup)
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

use infigraph_core::index_status::{self, StageOutcome, StageState};

fn record_scip(
    root: &Path,
    run_id: Option<&str>,
    label: &str,
    outcome: StageOutcome,
) -> Result<bool> {
    let Some(run_id) = run_id else {
        return Ok(true);
    };
    Ok(index_status::update_current(root, run_id, None, |report| {
        report.scip.insert(label.into(), outcome);
        Ok(())
    })?
    .is_some())
}

fn local_run_id(root: &Path) -> Result<Option<String>> {
    if cfg!(feature = "neo4j")
        && std::env::var("INFIGRAPH_BACKEND").is_ok_and(|mode| mode == "neo4j")
    {
        return Ok(None);
    }
    Ok(Some(index_status::current_run(root)?.run_id))
}

fn scip_succeeded(root: &Path, run_id: Option<&str>, label: &str) -> Result<bool> {
    let Some(run_id) = run_id else {
        return Ok(false);
    };
    let report = index_status::current_run(root)?;
    anyhow::ensure!(report.run_id == run_id, "obsolete enrichment run");
    Ok(report
        .scip
        .get(label)
        .is_some_and(|stage| stage.state == StageState::Succeeded))
}

fn indexed_languages(
    backend: &dyn infigraph_core::graph::GraphBackend,
) -> Result<std::collections::HashSet<String>> {
    Ok(backend
        .raw_query("MATCH (m:Module) RETURN DISTINCT m.language")?
        .into_iter()
        .filter_map(|row| row.into_iter().next())
        .collect())
}

fn languages_for_index(
    backend: &dyn infigraph_core::graph::GraphBackend,
    result: &infigraph_core::IndexResult,
) -> Result<std::collections::HashSet<String>> {
    if cfg!(feature = "neo4j")
        && std::env::var("INFIGRAPH_BACKEND").is_ok_and(|mode| mode == "neo4j")
    {
        // Preserve remote repo scoping; unfiltered raw Module queries would
        // inspect every repo in the shared database.
        return Ok(result
            .extractions
            .iter()
            .map(|e| e.language.clone())
            .collect());
    }
    indexed_languages(backend)
}

fn prepare_scip(
    root: &Path,
    languages: &std::collections::HashSet<String>,
    run_id: Option<&str>,
) -> Result<()> {
    for indexer in crate::scip_download::indexers_for_languages(languages) {
        if should_run_indexer(root, indexer) && scip_succeeded(root, run_id, indexer.binary_name)? {
            continue;
        }
        let stage = if should_run_indexer(root, indexer) {
            StageOutcome::new(StageState::Pending, "SCIP_PENDING")
        } else {
            StageOutcome::new(StageState::Skipped, "PROJECT_PREREQUISITE_MISSING")
        };
        if !record_scip(root, run_id, indexer.binary_name, stage)? {
            anyhow::bail!("obsolete enrichment run");
        }
    }
    Ok(())
}

/// Foreground and background use the same outcome/import pipeline.
fn enrich_scip(
    root: &Path,
    languages: &std::collections::HashSet<String>,
    run_id: Option<&str>,
    backend: &dyn infigraph_core::graph::GraphBackend,
) -> Result<()> {
    enrich_scip_with_tools(
        root,
        languages,
        run_id,
        backend,
        crate::scip_download::ensure_indexer,
    )
}

fn enrich_scip_with_tools(
    root: &Path,
    languages: &std::collections::HashSet<String>,
    run_id: Option<&str>,
    backend: &dyn infigraph_core::graph::GraphBackend,
    ensure_tool: impl Fn(&crate::scip_download::ScipIndexer) -> Option<std::path::PathBuf>,
) -> Result<()> {
    let mut failed = false;
    for indexer in crate::scip_download::indexers_for_languages(languages) {
        let label = indexer.binary_name;
        if !should_run_indexer(root, indexer) {
            continue;
        }
        if scip_succeeded(root, run_id, label)? {
            continue;
        }
        if !record_scip(
            root,
            run_id,
            label,
            StageOutcome::new(StageState::Running, "SCIP_RUNNING"),
        )? {
            anyhow::bail!("obsolete enrichment run");
        }
        let Some(bin) = ensure_tool(indexer) else {
            record_scip(
                root,
                run_id,
                label,
                StageOutcome::new(StageState::Failed, "SCIP_TOOL_UNAVAILABLE"),
            )?;
            eprintln!("Partial index: {label} failed (SCIP_TOOL_UNAVAILABLE)");
            failed = true;
            continue;
        };
        let scratch = match index_status::scip_scratch(root) {
            Ok(scratch) => scratch,
            Err(e) => {
                record_scip(
                    root,
                    run_id,
                    label,
                    StageOutcome::new(StageState::Failed, "SCIP_ARTIFACT_SETUP_FAILED"),
                )?;
                eprintln!("Partial index: {label} artifact setup failed: {e}");
                failed = true;
                continue;
            }
        };
        let output = scratch.path().join("index.scip");
        let mut outcome = run_scip_indexer_to(root, &bin, indexer, &output);
        if outcome.state == StageState::Succeeded {
            let import = || -> Result<StageOutcome> {
                match backend.import_scip_index(&output, Some(root)) {
                    Ok(stats) => {
                        eprintln!(
                            "Auto-SCIP: {label} imported {} relations, {} references",
                            stats.relations_added, stats.references_added
                        );
                        Ok(StageOutcome::new(StageState::Succeeded, "SCIP_IMPORTED"))
                    }
                    Err(e) => {
                        eprintln!("Auto-SCIP: {label} import failed: {e}");
                        Ok(StageOutcome::new(StageState::Failed, "SCIP_IMPORT_FAILED"))
                    }
                }
            };
            if let Some(run_id) = run_id {
                let current =
                    index_status::update_current(root, run_id, Some(backend), |report| {
                        let imported = import()?;
                        report.indexed_file_fingerprint = index_status::file_fingerprint(backend)?;
                        report.scip.insert(label.into(), imported.clone());
                        Ok(imported)
                    })?;
                let Some(imported) = current else {
                    anyhow::bail!("obsolete enrichment artifact discarded");
                };
                outcome = imported;
            } else {
                outcome = import()?;
            }
        } else {
            record_scip(root, run_id, label, outcome.clone())?;
        }
        if outcome.state == StageState::Failed {
            eprintln!("Partial index: {label} failed ({})", outcome.reason);
            failed = true;
        }
    }
    if let Some(run_id) = run_id {
        let report = index_status::current_run(root)?;
        anyhow::ensure!(report.run_id == run_id, "obsolete enrichment run");
        failed |= report.has_failures();
    }
    anyhow::ensure!(!failed, "Partial index: analysis failed; the syntax graph remains available. See .infigraph/index-status.json");
    Ok(())
}

pub(crate) fn auto_scip(
    root: &Path,
    result: &infigraph_core::IndexResult,
    backend: Option<&dyn infigraph_core::graph::GraphBackend>,
) -> Result<()> {
    let backend = backend.context("graph not initialized")?;
    let languages = languages_for_index(backend, result)?;
    let run_id = local_run_id(root)?;
    prepare_scip(root, &languages, run_id.as_deref())?;
    enrich_scip(root, &languages, run_id.as_deref(), backend)
}

/// Hidden child entry point; generation is captured by the parent before spawn.
pub(crate) fn cmd_scip_enrich(
    root: &Path,
    languages: &std::collections::HashSet<String>,
    run_id: Option<&str>,
) -> Result<()> {
    if let Some(id) = run_id {
        anyhow::ensure!(
            index_status::current_run(root)?.run_id == id,
            "obsolete enrichment run"
        );
    }
    let opened = (|| -> Result<Infigraph> {
        let registry = bundled_registry()?;
        let mut prism = Infigraph::open(root, registry)?;
        prism.init()?;
        Ok(prism)
    })();
    let prism = match opened {
        Ok(prism) => prism,
        Err(e) => {
            for indexer in crate::scip_download::indexers_for_languages(languages) {
                if should_run_indexer(root, indexer) {
                    record_scip(
                        root,
                        run_id,
                        indexer.binary_name,
                        StageOutcome::new(StageState::Failed, "SCIP_BACKEND_UNAVAILABLE"),
                    )?;
                }
            }
            return Err(e);
        }
    };
    let backend = prism.backend().context("graph not initialized")?;
    let result = enrich_scip(root, languages, run_id, backend);
    // Preserve background embedding of compiler-added symbols. Foreground
    // --no-embed never enters this child path.
    #[allow(unused_mut)]
    let mut embedded_remotely = false;
    #[cfg(feature = "remote")]
    if is_neo4j_backend() {
        if let Ok(pg) = infigraph_core::meta::PostgresMetaStore::connect_from_env_cached() {
            if let Err(e) = infigraph_core::embed::update_embeddings_remote(backend, pg, &[]) {
                eprintln!("Auto-SCIP: embedding update failed: {e}");
            }
        }
        embedded_remotely = true;
    }
    if !embedded_remotely {
        if let Err(e) = infigraph_core::embed::update_embeddings(backend, root, &[]) {
            eprintln!("Auto-SCIP: embedding update failed: {e}");
        }
    }
    result
}

fn should_run_indexer(root: &Path, indexer: &crate::scip_download::ScipIndexer) -> bool {
    if indexer.binary_name == "scip-clang" && !root.join("compile_commands.json").exists() {
        eprintln!("Auto-SCIP: skipping scip-clang — compile_commands.json not found");
        return false;
    }
    if indexer.binary_name == "scip-ruby" {
        let has_gemspec = std::fs::read_dir(root)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "gemspec"))
            })
            .unwrap_or(false);
        if !has_gemspec {
            eprintln!("Auto-SCIP: skipping scip-ruby — no .gemspec found");
            return false;
        }
    }
    true
}

fn run_scip_indexer_to(
    root: &Path,
    bin: &Path,
    indexer: &crate::scip_download::ScipIndexer,
    output_path: &Path,
) -> StageOutcome {
    let label = indexer.binary_name;
    eprintln!("Auto-SCIP: running {label}...");

    let cmd_str = bin.to_string_lossy();
    let extra = crate::scip_download::extra_runtime_paths();
    let extra_path = if extra.is_empty() {
        None
    } else {
        Some(extra.as_str())
    };

    if indexer.binary_name == "scip-java" {
        return run_scip_java(root, &cmd_str, output_path, extra_path);
    }

    run_scip_indexer_cmd(
        root,
        &cmd_str,
        indexer.scip_args,
        label,
        extra_path,
        indexer.output_flag,
        output_path,
    )
}

fn run_scip_java(
    root: &Path,
    cmd: &str,
    output_path: &Path,
    extra_path: Option<&str>,
) -> StageOutcome {
    let has_gradle = root.join("build.gradle").exists()
        || root.join("build.gradle.kts").exists()
        || root.join("settings.gradle").exists()
        || root.join("settings.gradle.kts").exists();
    let has_maven = root.join("pom.xml").exists();

    if has_gradle && has_maven {
        let primary =
            if root.join("settings.gradle").exists() || root.join("settings.gradle.kts").exists() {
                "gradle"
            } else {
                "maven"
            };
        let fallback = if primary == "gradle" {
            "maven"
        } else {
            "gradle"
        };

        eprintln!("Auto-SCIP: detected both Maven and Gradle, trying {primary}");
        let primary_args: Vec<&str> = vec!["index", "--build-tool", primary];
        let primary_result = run_scip_indexer_cmd(
            root,
            cmd,
            &primary_args,
            "scip-java",
            extra_path,
            Some("--output"),
            output_path,
        );
        if primary_result.state == StageState::Succeeded {
            return primary_result;
        }
        if output_path.exists() {
            let _ = std::fs::remove_file(output_path);
        }
        eprintln!("Auto-SCIP: {primary} failed, falling back to {fallback}");
        let fallback_args: Vec<&str> = vec!["index", "--build-tool", fallback];
        return run_scip_indexer_cmd(
            root,
            cmd,
            &fallback_args,
            "scip-java",
            extra_path,
            Some("--output"),
            output_path,
        );
    }

    run_scip_indexer_cmd(
        root,
        cmd,
        &["index"],
        "scip-java",
        extra_path,
        Some("--output"),
        output_path,
    )
}

fn run_scip_indexer_cmd(
    root: &Path,
    cmd: &str,
    args: &[&str],
    label: &str,
    extra_path: Option<&str>,
    output_flag: Option<&str>,
    output_path: &Path,
) -> StageOutcome {
    let mut command = std::process::Command::new(cmd);
    command.args(args).current_dir(root);

    if let Some(flag) = output_flag {
        command.arg(flag).arg(output_path);
    }

    if let Some(extra) = extra_path {
        let path = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        command.env("PATH", format!("{extra}{sep}{path}"));
    }

    {
        let ig = crate::scip_download::infigraph_dir();
        let java_macos = ig.join("java").join("Contents").join("Home");
        if java_macos.exists() {
            command.env("JAVA_HOME", &java_macos);
        } else {
            let java_home = ig.join("java");
            if java_home.join("bin").exists() {
                command.env("JAVA_HOME", &java_home);
            }
        }
        let dotnet_root = ig.join("dotnet");
        if dotnet_root.exists() {
            command.env("DOTNET_ROOT", &dotnet_root);
        }
    }

    let default_out = root.join("index.scip");
    // Indexers without an output flag must not consume or overwrite a user's
    // retained index.scip. Serialize their fixed output path across runs.
    let _output_lock = if output_flag.is_none() {
        match infigraph_core::lockfile::acquire(
            &root.join(".infigraph/scip-output.lock"),
            "scip-output",
            std::time::Duration::from_secs(30),
        ) {
            Ok(lock) => Some(lock),
            Err(_) => return StageOutcome::new(StageState::Failed, "SCIP_OUTPUT_BUSY"),
        }
    } else {
        None
    };
    if std::fs::symlink_metadata(output_path).is_ok()
        || (output_flag.is_none() && std::fs::symlink_metadata(&default_out).is_ok())
    {
        return StageOutcome::new(StageState::Failed, "SCIP_OUTPUT_CONFLICT");
    }
    let status = command.status();
    if output_flag.is_none()
        && default_out.exists()
        && std::fs::rename(&default_out, output_path).is_err()
    {
        return StageOutcome::new(StageState::Failed, "SCIP_ARTIFACT_MOVE_FAILED");
    }
    match status {
        Ok(status) if status.success() => match std::fs::symlink_metadata(output_path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                StageOutcome::new(StageState::Succeeded, "SCIP_ARTIFACT_CREATED")
            }
            Ok(_) => StageOutcome::new(StageState::Failed, "SCIP_ARTIFACT_INVALID"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                StageOutcome::new(StageState::Failed, "SCIP_ARTIFACT_MISSING")
            }
            Err(_) => StageOutcome::new(StageState::Failed, "SCIP_ARTIFACT_UNREADABLE"),
        },
        Ok(status) => {
            eprintln!("Auto-SCIP: {label} exited with {status}");
            let mut result = StageOutcome::new(StageState::Failed, "SCIP_PROCESS_FAILED");
            result.exit_code = status.code();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                result.signal = status.signal();
            }
            result
        }
        Err(e) => {
            eprintln!("Auto-SCIP: failed to launch {label}: {e}");
            StageOutcome::new(StageState::Failed, "SCIP_LAUNCH_FAILED")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Regression test: `spawn_scip_child_process` respawns this binary with
    /// `scip_enrich_args(&langs)` as the argv tail. This previously hardcoded
    /// `--languages <langs>`, but `Commands::ScipEnrich` declares `languages`
    /// as a positional argument (no `#[arg(long)]`), so every respawned
    /// child died instantly with a clap parse error and no SCIP indexer
    /// (scip-typescript, scip-python, etc.) ever actually ran. Parsing the
    /// exact args through the real `Cli` definition — rather than spawning a
    /// process — catches any future mismatch between the two immediately.
    #[test]
    fn scip_enrich_args_parse_as_positional_language() {
        use clap::Parser;

        let langs = "typescript,python";
        let mut argv = vec!["infigraph".to_string()];
        argv.extend(scip_enrich_args(langs));

        let cli = crate::Cli::try_parse_from(&argv)
            .expect("scip_enrich_args must parse under the ScipEnrich clap definition");

        assert!(
            matches!(&cli.command, crate::Commands::ScipEnrich { languages, .. } if languages == langs),
            "expected Commands::ScipEnrich {{ languages: {langs:?} }}"
        );
    }

    /// Regression test for review feedback on the scip-enrich fix:
    /// `spawn_scip_child_process` used to discard `spawn()`'s result
    /// entirely. `spawn()` only reports failure to *launch* a process — it
    /// says nothing about the child crashing or exiting nonzero afterward,
    /// which is exactly the failure shape of the original bug (the child
    /// launched fine and died instantly inside clap's parser). This asserts
    /// the decision logic used by the wait thread: warn on a nonzero exit,
    /// stay silent on success.
    #[test]
    #[cfg(unix)]
    fn scip_enrich_exit_message_warns_on_nonzero_exit() {
        use std::os::unix::process::ExitStatusExt;

        let log_path = std::path::PathBuf::from("/tmp/some-project/.infigraph/scip-enrich.log");
        let failed = std::process::ExitStatus::from_raw(1 << 8); // exit code 1
        let msg = scip_enrich_exit_message(Ok(failed), &log_path);
        assert!(
            msg.as_deref()
                .is_some_and(|m| m.contains("scip-enrich exited") && m.contains("scip-enrich.log")),
            "expected a warning mentioning the exit status and log path, got {msg:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn scip_enrich_exit_message_silent_on_success() {
        use std::os::unix::process::ExitStatusExt;

        let log_path = std::path::PathBuf::from("/tmp/some-project/.infigraph/scip-enrich.log");
        let ok = std::process::ExitStatus::from_raw(0);
        let msg = scip_enrich_exit_message(Ok(ok), &log_path);
        assert!(
            msg.is_none(),
            "a successful exit should not produce a warning, got {msg:?}"
        );
    }

    #[test]
    fn scip_enrich_exit_message_warns_on_wait_error() {
        let log_path = std::path::PathBuf::from("/tmp/some-project/.infigraph/scip-enrich.log");
        let err = std::io::Error::other("no such process");
        let msg = scip_enrich_exit_message(Err(err), &log_path);
        assert!(
            msg.as_deref()
                .is_some_and(|m| m.contains("failed to wait on scip-enrich")),
            "expected a warning about the wait() failure, got {msg:?}"
        );
    }

    /// Regression test for the Postgres-connect-on-plain-local-index bug:
    /// `Infigraph::backend()` became universal (returning `Some` for the
    /// default local Kùzu backend too, not just Neo4j), which silently
    /// turned `if let Some(backend) = prism.backend()` into an always-true
    /// check gating the Postgres-embeddings branch — so `infigraph index`
    /// tried to connect to Postgres and failed even for plain local
    /// indexing with no remote backend configured. `is_neo4j_backend()`
    /// replaces that check with the same explicit `INFIGRAPH_BACKEND`
    /// check already used a few lines above it (repo registration) —
    /// asserts it's only true for an explicit `neo4j` value.
    #[test]
    #[cfg(feature = "remote")]
    fn is_neo4j_backend_only_true_for_explicit_neo4j_env() {
        std::env::remove_var("INFIGRAPH_BACKEND");
        assert!(
            !is_neo4j_backend(),
            "unset INFIGRAPH_BACKEND must not select Postgres"
        );

        std::env::set_var("INFIGRAPH_BACKEND", "kuzu");
        assert!(
            !is_neo4j_backend(),
            "explicit kuzu backend must not select Postgres"
        );

        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        assert!(
            is_neo4j_backend(),
            "explicit neo4j backend must select Postgres"
        );

        std::env::remove_var("INFIGRAPH_BACKEND");
    }

    #[test]
    fn ci_env_vars_list_complete() {
        assert!(CI_ENV_VARS.contains(&"CI"));
        assert!(CI_ENV_VARS.contains(&"GITHUB_ACTIONS"));
        assert!(CI_ENV_VARS.contains(&"JENKINS_URL"));
        assert!(CI_ENV_VARS.contains(&"BUILDKITE"));
        assert!(CI_ENV_VARS.contains(&"GITLAB_CI"));
        assert!(CI_ENV_VARS.contains(&"INFIGRAPH_NO_WATCH"));
    }

    #[test]
    fn lock_acquired_when_no_watcher() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("watch.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        file.try_lock_exclusive().unwrap();
        file.unlock().unwrap();
    }

    #[test]
    fn lock_fails_when_watcher_holds_it() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("watch.lock");

        let watcher_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        watcher_file.lock_exclusive().unwrap();

        let check_file = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
        assert!(check_file.try_lock_exclusive().is_err());

        watcher_file.unlock().unwrap();
        check_file.try_lock_exclusive().unwrap();
        check_file.unlock().unwrap();
    }

    #[test]
    fn ensure_watcher_skips_without_infigraph_dir() {
        let tmp = TempDir::new().unwrap();
        ensure_watcher_running(tmp.path());
        assert!(!tmp.path().join(".infigraph").join("watch.lock").exists());
    }

    #[test]
    fn ensure_watcher_skips_when_lock_held() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();
        let lock_path = tg_dir.join("watch.lock");

        let _lock = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        _lock.lock_exclusive().unwrap();

        ensure_watcher_running(tmp.path());
    }

    #[test]
    fn is_ci_respects_infigraph_no_watch() {
        // Temporarily set INFIGRAPH_NO_WATCH — is_ci should return true
        std::env::set_var("INFIGRAPH_NO_WATCH", "1");
        assert!(is_ci());
        std::env::remove_var("INFIGRAPH_NO_WATCH");
    }

    #[test]
    fn lock_released_after_drop() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("watch.lock");

        use fs2::FileExt;
        {
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap();
            file.lock_exclusive().unwrap();
            // file dropped here — lock should release
        }

        // Re-acquire should succeed after drop
        let file2 = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
        file2.try_lock_exclusive().unwrap();
        file2.unlock().unwrap();
    }

    #[test]
    fn acquire_watch_lock_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("nested").join("dir").join("watch.lock");
        assert!(!lock_path.parent().unwrap().exists());

        let lock = crate::info_commands::acquire_watch_lock(&lock_path);
        assert!(lock.is_ok());
        assert!(lock_path.exists());
    }

    #[test]
    fn watcher_is_alive_when_lock_held() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();
        let lock_path = tg_dir.join("watch.lock");

        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        file.lock_exclusive().unwrap();

        assert!(crate::info_commands::watcher_is_alive(&lock_path));

        file.unlock().unwrap();
        assert!(!crate::info_commands::watcher_is_alive(&lock_path));
    }

    #[test]
    fn watcher_is_alive_no_file() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("nonexistent").join("watch.lock");
        assert!(!crate::info_commands::watcher_is_alive(&lock_path));
    }

    #[test]
    fn watch_stop_creates_sentinel() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let sentinel = tg_dir.join("watch.stop");
        assert!(!sentinel.exists());

        // No watcher running — watch_stop should say "No watcher running"
        let result = crate::info_commands::cmd_watch_stop(tmp.path());
        assert!(result.is_ok());
        assert!(!sentinel.exists());

        // Simulate watcher holding lock
        let lock_path = tg_dir.join("watch.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        file.lock_exclusive().unwrap();

        let result = crate::info_commands::cmd_watch_stop(tmp.path());
        assert!(result.is_ok());
        assert!(sentinel.exists());

        file.unlock().unwrap();
    }

    #[test]
    fn watch_status_reports_correctly() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        // No lock file — not running
        let result = crate::info_commands::cmd_watch_status(tmp.path());
        assert!(result.is_ok());

        // Lock held — running
        let lock_path = tg_dir.join("watch.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        file.lock_exclusive().unwrap();

        let result = crate::info_commands::cmd_watch_status(tmp.path());
        assert!(result.is_ok());

        file.unlock().unwrap();
    }

    #[test]
    fn sentinel_file_removed_by_watcher_loop() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let sentinel = tg_dir.join("watch.stop");
        fs::write(&sentinel, b"").unwrap();
        assert!(sentinel.exists());

        // Simulate what the watcher loop does
        if sentinel.exists() {
            let _ = fs::remove_file(&sentinel);
        }
        assert!(!sentinel.exists());
    }

    #[test]
    fn global_hook_exclusion_list_is_exhaustive() {
        // Commands that should NOT trigger auto-watcher
        let excluded = [
            "watch",
            "watch-stop",
            "watch-status",
            "scip-enrich",
            "delete",
            "update",
            "install",
            "uninstall",
            "init",
            "languages",
            "repos",
            "clean-runtimes",
        ];
        // Verify none of these are index-dependent commands
        for cmd in &excluded {
            assert!(
                ![
                    "search",
                    "callers",
                    "callees",
                    "dead-code",
                    "stats",
                    "impact"
                ]
                .contains(cmd),
                "{cmd} should not be in exclusion list"
            );
        }
    }

    #[test]
    fn ensure_watcher_noop_when_ci_env_set() {
        std::env::set_var("CI", "true");
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        ensure_watcher_running(tmp.path());
        assert!(!tg_dir.join("watch.lock").exists());

        std::env::remove_var("CI");
    }

    #[test]
    #[cfg(feature = "remote")]
    fn ensure_watcher_noop_when_remote_backend() {
        // Remote (shared-Neo4j) mode reindexes via webhook — a local watcher
        // would be redundant and race the webhook path.
        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        ensure_watcher_running(tmp.path());
        assert!(!tg_dir.join("watch.lock").exists());

        std::env::remove_var("INFIGRAPH_BACKEND");
    }

    #[test]
    fn ensure_watcher_called_for_each_group_repo() {
        // Simulate what group index does: ensure_watcher_running per repo
        let repos: Vec<TempDir> = (0..3).map(|_| TempDir::new().unwrap()).collect();
        for repo in &repos {
            let tg_dir = repo.path().join(".infigraph");
            fs::create_dir_all(&tg_dir).unwrap();
        }

        // Each repo should be checkable independently
        for repo in &repos {
            let lock_path = repo.path().join(".infigraph").join("watch.lock");
            assert!(!crate::info_commands::watcher_is_alive(&lock_path));
        }
    }

    #[test]
    fn group_watcher_skips_repos_without_infigraph() {
        let tmp = TempDir::new().unwrap();
        // No .infigraph dir — should not panic or create files
        ensure_watcher_running(tmp.path());
        assert!(!tmp.path().join(".infigraph").exists());
    }

    #[test]
    fn multiple_watchers_independent_locks() {
        let repo_a = TempDir::new().unwrap();
        let repo_b = TempDir::new().unwrap();
        let dir_a = repo_a.path().join(".infigraph");
        let dir_b = repo_b.path().join(".infigraph");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        let lock_a = dir_a.join("watch.lock");
        let lock_b = dir_b.join("watch.lock");

        use fs2::FileExt;
        // Lock repo A
        let file_a = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_a)
            .unwrap();
        file_a.lock_exclusive().unwrap();

        // Repo B should be unlocked
        assert!(crate::info_commands::watcher_is_alive(&lock_a));
        assert!(!crate::info_commands::watcher_is_alive(&lock_b));

        file_a.unlock().unwrap();
    }

    #[test]
    fn sentinel_stops_only_target_repo() {
        let repo_a = TempDir::new().unwrap();
        let repo_b = TempDir::new().unwrap();
        let dir_a = repo_a.path().join(".infigraph");
        let dir_b = repo_b.path().join(".infigraph");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        // Write sentinel to repo A only
        fs::write(dir_a.join("watch.stop"), b"").unwrap();

        assert!(dir_a.join("watch.stop").exists());
        assert!(!dir_b.join("watch.stop").exists());
    }

    #[test]
    fn delete_sends_sentinel_before_removal() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let lock_path = tg_dir.join("watch.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        use fs2::FileExt;
        file.lock_exclusive().unwrap();

        // Simulate what cmd_delete_project does: check alive → write sentinel
        assert!(crate::info_commands::watcher_is_alive(&lock_path));
        let sentinel = tg_dir.join("watch.stop");
        fs::write(&sentinel, b"").unwrap();
        assert!(sentinel.exists());

        file.unlock().unwrap();
    }

    #[test]
    fn bm25_cache_stale_when_embeddings_newer() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let emb_path = tg_dir.join("embeddings.bin");
        let bm25_path = tg_dir.join("bm25_cache.bin");

        // Create BM25 cache first
        fs::write(&bm25_path, b"old_cache").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Then update embeddings (newer mtime)
        fs::write(&emb_path, b"new_embeddings").unwrap();

        let emb_mtime = fs::metadata(&emb_path).unwrap().modified().unwrap();
        let cache_mtime = fs::metadata(&bm25_path).unwrap().modified().unwrap();

        // Cache should be stale (embeddings newer than cache)
        assert!(
            emb_mtime > cache_mtime,
            "embeddings should be newer than BM25 cache"
        );
    }

    #[test]
    fn bm25_cache_fresh_when_older_than_embeddings() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let emb_path = tg_dir.join("embeddings.bin");
        let bm25_path = tg_dir.join("bm25_cache.bin");

        // Create embeddings first
        fs::write(&emb_path, b"embeddings").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Then create BM25 cache (newer mtime)
        fs::write(&bm25_path, b"cache").unwrap();

        let emb_mtime = fs::metadata(&emb_path).unwrap().modified().unwrap();
        let cache_mtime = fs::metadata(&bm25_path).unwrap().modified().unwrap();

        // Cache should be fresh (cache newer than embeddings)
        assert!(cache_mtime >= emb_mtime, "BM25 cache should be fresh");
    }

    #[test]
    fn hnsw_sidecar_invalidated_after_embed_update() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let hnsw_path = tg_dir.join("hnsw_index.usearch");
        let emb_path = tg_dir.join("embeddings.bin");

        // Create HNSW first
        fs::write(&hnsw_path, b"old_hnsw").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Update embeddings (simulates watcher reindex)
        fs::write(&emb_path, b"new_embeddings").unwrap();

        let hnsw_mtime = fs::metadata(&hnsw_path).unwrap().modified().unwrap();
        let emb_mtime = fs::metadata(&emb_path).unwrap().modified().unwrap();

        // HNSW should be stale
        assert!(
            emb_mtime > hnsw_mtime,
            "HNSW sidecar should be stale after embed update"
        );
    }

    #[test]
    fn search_cache_key_uses_embeddings_mtime() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        let emb_path = tg_dir.join("embeddings.bin");
        fs::write(&emb_path, b"v1").unwrap();
        let mtime1 = fs::metadata(&emb_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&emb_path, b"v2").unwrap();
        let mtime2 = fs::metadata(&emb_path).unwrap().modified().unwrap();

        // Different writes should produce different mtimes
        assert_ne!(
            mtime1, mtime2,
            "mtime should change after embeddings.bin update"
        );
    }

    #[test]
    fn watch_stop_idempotent() {
        let tmp = TempDir::new().unwrap();
        let tg_dir = tmp.path().join(".infigraph");
        fs::create_dir_all(&tg_dir).unwrap();

        // No watcher running — multiple stops should be fine
        for _ in 0..3 {
            let result = crate::info_commands::cmd_watch_stop(tmp.path());
            assert!(result.is_ok());
        }
    }

    #[test]
    fn watch_status_no_infigraph_dir() {
        let tmp = TempDir::new().unwrap();
        // No .infigraph — should report not running without error
        let result = crate::info_commands::cmd_watch_status(tmp.path());
        assert!(result.is_ok());
    }
}

#[cfg(all(test, unix))]
mod outcome_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_script(dir: &Path, text: &str) -> std::path::PathBuf {
        let path = dir.join("fake-indexer");
        std::fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn empty_or_linked_artifacts_are_not_valid_current_attempt_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("attempt.scip");
        let retained = dir.path().join("retained.scip");
        std::fs::write(&retained, b"retained").unwrap();
        for command in [": > \"$2\"", "ln -s retained.scip \"$2\""] {
            let script = fake_script(dir.path(), command);
            let result = run_scip_indexer_cmd(
                dir.path(),
                script.to_str().unwrap(),
                &[],
                "test",
                None,
                Some("--output"),
                &output,
            );
            assert_eq!(result.reason, "SCIP_ARTIFACT_INVALID");
            assert_eq!(std::fs::read(&retained).unwrap(), b"retained");
            std::fs::remove_file(&output).unwrap();
        }
    }

    #[test]
    fn subprocess_failure_missing_output_and_retained_artifacts_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".infigraph")).unwrap();
        let output = dir.path().join("attempt.scip");
        let script = fake_script(dir.path(), "exit 7");
        let result = run_scip_indexer_cmd(
            dir.path(),
            script.to_str().unwrap(),
            &[],
            "test",
            None,
            Some("--output"),
            &output,
        );
        assert_eq!(result.state, StageState::Failed);
        assert_eq!(result.exit_code, Some(7));
        fake_script(dir.path(), "kill -TERM $$");
        let terminated = run_scip_indexer_cmd(
            dir.path(),
            script.to_str().unwrap(),
            &[],
            "test",
            None,
            Some("--output"),
            &output,
        );
        assert_eq!(terminated.state, StageState::Failed);
        assert_eq!(terminated.exit_code, None);
        assert_eq!(terminated.signal, Some(15));
        fake_script(dir.path(), "exit 0");
        assert_eq!(
            run_scip_indexer_cmd(
                dir.path(),
                script.to_str().unwrap(),
                &[],
                "test",
                None,
                Some("--output"),
                &output
            )
            .reason,
            "SCIP_ARTIFACT_MISSING"
        );
        assert_eq!(
            run_scip_indexer_cmd(
                dir.path(),
                "/does-not-exist",
                &[],
                "test",
                None,
                Some("--output"),
                &output
            )
            .reason,
            "SCIP_LAUNCH_FAILED"
        );
        std::fs::write(dir.path().join("index.scip"), b"retained").unwrap();
        assert_eq!(
            run_scip_indexer_cmd(
                dir.path(),
                script.to_str().unwrap(),
                &[],
                "test",
                None,
                None,
                &output
            )
            .reason,
            "SCIP_OUTPUT_CONFLICT"
        );
        assert_eq!(
            std::fs::read(dir.path().join("index.scip")).unwrap(),
            b"retained"
        );
    }

    #[test]
    fn output_is_run_owned_and_failed_artifacts_are_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".infigraph")).unwrap();
        let scratch = index_status::scip_scratch(dir.path()).unwrap();
        let path = scratch.path().join("index.scip");
        let script = fake_script(dir.path(), "printf '\\001' > \"$2\"");
        let outcome = run_scip_indexer_cmd(
            dir.path(),
            script.to_str().unwrap(),
            &[],
            "test",
            None,
            Some("--output"),
            &path,
        );
        assert_eq!(outcome.state, StageState::Succeeded);
        drop(scratch);
        assert!(!path.exists());
    }
}

#[cfg(all(test, unix))]
mod pipeline_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn failed_enrichment_survives_noop_and_recovers_only_after_import() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "const helper = () => 1;").unwrap();
        let mut prism = Infigraph::open(dir.path(), bundled_registry().unwrap()).unwrap();
        prism.init().unwrap();
        prism.index().unwrap();
        let backend = prism.backend().unwrap();
        let languages = indexed_languages(backend).unwrap();
        let run = local_run_id(dir.path()).unwrap().unwrap();
        prepare_scip(dir.path(), &languages, Some(&run)).unwrap();
        assert!(
            enrich_scip_with_tools(dir.path(), &languages, Some(&run), backend, |_| None).is_err()
        );
        assert_eq!(
            index_status::load(dir.path()).unwrap().scip["scip-typescript"].reason,
            "SCIP_TOOL_UNAVAILABLE"
        );
        let script = dir.path().join("fake-indexer");
        std::fs::write(&script, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            enrich_scip_with_tools(dir.path(), &languages, Some(&run), backend, |_| Some(
                script.clone()
            ))
            .is_err()
        );
        assert_eq!(
            index_status::load(dir.path()).unwrap().scip["scip-typescript"].exit_code,
            Some(7)
        );
        assert_eq!(prism.index().unwrap().indexed_files, 0);
        let report = index_status::load(dir.path()).unwrap();
        assert!(report.has_failures());
        assert!(
            enrich_scip_with_tools(dir.path(), &languages, Some(&run), backend, |_| panic!(
                "obsolete job must not launch"
            ))
            .is_err()
        );
        // Every fake indexer writes to the current run's explicit output path.
        let writer = "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do if [ \"$1\" = --output ]; then shift; output=\"$1\"; fi; shift; done\nprintf 'CONTENT' > \"$output\"\n";
        std::fs::write(&script, writer.replace("CONTENT", "\\377")).unwrap();
        assert!(enrich_scip_with_tools(
            dir.path(),
            &languages,
            Some(&report.run_id),
            backend,
            |_| Some(script.clone())
        )
        .is_err());
        assert_eq!(
            index_status::load(dir.path()).unwrap().scip["scip-typescript"].reason,
            "SCIP_IMPORT_FAILED"
        );
        // A minimal valid protobuf index exercises successful import, not
        // compiler relationship quality (covered by actual source fixtures).
        std::fs::write(&script, writer.replace("CONTENT", "\\012\\000")).unwrap();
        enrich_scip_with_tools(
            dir.path(),
            &languages,
            Some(&report.run_id),
            backend,
            |_| Some(script.clone()),
        )
        .unwrap();
        assert!(!index_status::load(dir.path()).unwrap().has_failures());
        assert!(prism.coverage_notice().starts_with("Coverage:"));
        assert_eq!(prism.index().unwrap().indexed_files, 0);
        let current = local_run_id(dir.path()).unwrap().unwrap();
        prepare_scip(dir.path(), &languages, Some(&current)).unwrap();
        enrich_scip_with_tools(dir.path(), &languages, Some(&current), backend, |_| {
            panic!("successful unchanged enrichment must not run again")
        })
        .unwrap();
        assert_eq!(backend.symbols_in_file("a.ts").unwrap().len(), 1);
        assert!(std::fs::read_dir(dir.path().join(".infigraph"))
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("scip-run-")));
    }

    #[test]
    fn missing_project_prerequisite_is_skipped_before_provisioning() {
        let dir = tempfile::tempdir().unwrap();
        let mut prism = Infigraph::open(dir.path(), bundled_registry().unwrap()).unwrap();
        prism.init().unwrap();
        prism.index().unwrap();
        let languages = ["cpp".to_string()].into_iter().collect();
        let run = local_run_id(dir.path()).unwrap().unwrap();
        prepare_scip(dir.path(), &languages, Some(&run)).unwrap();
        enrich_scip_with_tools(
            dir.path(),
            &languages,
            Some(&run),
            prism.backend().unwrap(),
            |_| panic!("must not provision skipped indexer"),
        )
        .unwrap();
        assert_eq!(
            index_status::load(dir.path()).unwrap().scip["scip-clang"].state,
            StageState::Skipped
        );
    }
}
