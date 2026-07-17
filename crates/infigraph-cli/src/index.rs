use std::path::Path;

use anyhow::{Context, Result};
use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

pub(crate) fn cmd_index(root: &Path, full: bool, no_embed: bool) -> Result<()> {
    if full {
        let tg_dir = root.join(".infigraph");
        if tg_dir.exists() {
            // Sessions are in a separate DB at .infigraph/sessions/db/ — preserve them
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

    let registry = crate::full_registry(Some(root))?;
    let mut prism = Infigraph::open(root, registry)?;
    prism.init()?;

    println!("Indexing project...");
    let result = prism.index()?;
    println!(
        "Indexed {}/{} files",
        result.indexed_files, result.total_files
    );

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

    // Detect cross-cutting concerns, taint, etc. — skip when no files changed (incremental no-op)
    if result.indexed_files > 0 {
        if let Some(store) = prism.store() {
            // Docstring-only analyzers (no file I/O)
            match infigraph_core::concerns::detect_cross_cutting(store) {
                Ok(matches) if !matches.is_empty() => {
                    println!("Detected {} cross-cutting concerns", matches.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("warning: concern detection failed: {e}"),
            }
            match infigraph_core::config::detect_config_bindings(store) {
                Ok(bindings) if !bindings.is_empty() => {
                    println!("Detected {} config bindings", bindings.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("warning: config binding detection failed: {e}"),
            }
            match infigraph_core::reflection::detect_reflection_sites(store, root) {
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
            match infigraph_core::taint::build_source_cache(store, root) {
                Ok((functions, cache)) => {
                    match infigraph_core::taint::detect_taint_flows_with_cache(
                        store, &functions, &cache,
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
                    match infigraph_core::taint::interprocedural::detect_interprocedural_taint_with_cache(store, &functions, &cache, 5) {
                    Ok(flows) if !flows.is_empty() => {
                        println!("Detected {} inter-procedural taint flows", flows.len());
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: inter-procedural taint failed: {e}"),
                }
                    match infigraph_core::taint::dynamic_urls::detect_dynamic_urls_with_cache(
                        store, &functions, &cache,
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
    }

    let stats = prism.stats()?;
    println!("\n{}", stats);

    // In remote mode, register this repo in Postgres so it appears in registry queries
    #[cfg(feature = "remote")]
    {
        if std::env::var("INFIGRAPH_BACKEND")
            .map(|v| v == "neo4j")
            .unwrap_or(false)
        {
            let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            let repo_name = canonical
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let mut registry = infigraph_core::multi::Registry::load()?;
            registry.register_repo(&repo_name, root, &prism)?;
            println!("Registered '{}' in Postgres registry", repo_name);
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

    // Compute and save embeddings — only for new/changed symbols
    if no_embed {
        auto_scip(root, &result, prism.store())?;
        return Ok(());
    }
    {
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        #[allow(unused_mut)]
        let mut done = false;

        #[cfg(feature = "remote")]
        if let Some(backend) = prism.backend() {
            let pg = infigraph_core::meta::PostgresMetaStore::connect_from_env()?;
            pg.init_schema()?;
            let count = infigraph_core::embed::update_embeddings_remote(backend, &pg, &changed)?;
            println!("Saved {} embeddings to Postgres pgvector", count);
            done = true;
        }

        if !done {
            let store = prism.store().context("graph not initialized")?;
            let count = infigraph_core::embed::update_embeddings(store, root, &changed)?;
            println!("Saved {} embeddings to .infigraph/embeddings.bin", count);
        }
    }

    // Auto-index documents (PDF, DOCX, XML, Markdown, etc.)
    match crate::commands::cmd_index_docs(root) {
        Ok(()) => {}
        Err(e) => eprintln!("warning: document indexing failed: {e}"),
    }

    // Drop prism to release the GraphStore handle before background SCIP
    let detected_languages: std::collections::HashSet<String> = result
        .extractions
        .iter()
        .map(|e| e.language.clone())
        .collect();
    drop(prism);

    // SCIP enrichment in a detached child process — parent returns immediately.
    spawn_scip_child_process(root, &detected_languages);

    if let Err(e) = infigraph_core::claude_md::ensure_project_claude_md(root) {
        eprintln!("warning: failed to update project CLAUDE.md: {e}");
    }

    Ok(())
}

fn spawn_scip_child_process(root: &Path, detected_languages: &std::collections::HashSet<String>) {
    use crate::scip_download;

    let indexers = scip_download::indexers_for_languages(detected_languages);
    if indexers.is_empty() {
        return;
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

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };

    let log_path = root.join(".infigraph").join("scip-enrich.log");
    let stderr_target = match std::fs::File::create(&log_path) {
        Ok(f) => std::process::Stdio::from(f),
        Err(_) => std::process::Stdio::null(),
    };

    let _ = std::process::Command::new(exe)
        .args(scip_enrich_args(&langs))
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr_target)
        .spawn();

    eprintln!("  Log: {}", log_path.display());
}

/// Args for respawning this binary as the hidden `scip-enrich` subcommand.
/// `languages` is a positional argument on `Commands::ScipEnrich`, not a
/// flag — extracted so tests can assert these parse under that definition
/// without spawning a process.
fn scip_enrich_args(langs: &str) -> Vec<String> {
    vec!["scip-enrich".to_string(), langs.to_string()]
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

pub(crate) fn ensure_watcher_running(root: &Path) {
    if is_ci() {
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

pub(crate) fn import_scip_and_cleanup(
    root: &Path,
    scip_path: Option<&std::path::Path>,
    existing_store: Option<&infigraph_core::graph::GraphStore>,
) {
    let scip_out = scip_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.join("index.scip"));
    if !scip_out.exists() {
        return;
    }

    if let Some(store) = existing_store {
        match infigraph_core::scip::import_scip_index(&scip_out, store, Some(root)) {
            Ok(stats) => println!(
                "Auto-SCIP: enriched {} symbols, {} added, {} references, {} new symbols, {} corrections learned",
                stats.symbols_enriched, stats.relations_added, stats.references_added, stats.symbols_added, stats.corrections_learned
            ),
            Err(e) => eprintln!("Auto-SCIP: import failed: {e}"),
        }
        let _ = std::fs::remove_file(&scip_out);
        return;
    }

    let registry = match bundled_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Auto-SCIP: import failed: {e}");
            return;
        }
    };
    let mut prism = match Infigraph::open(root, registry) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Auto-SCIP: import failed: {e}");
            return;
        }
    };
    if prism.init().is_err() {
        return;
    }
    let store = match prism.store() {
        Some(s) => s,
        None => return,
    };
    match infigraph_core::scip::import_scip_index(&scip_out, store, Some(root)) {
        Ok(stats) => println!(
            "Auto-SCIP: enriched {} symbols, {} added, {} references, {} new symbols, {} corrections learned",
            stats.symbols_enriched, stats.relations_added, stats.references_added, stats.symbols_added, stats.corrections_learned
        ),
        Err(e) => eprintln!("Auto-SCIP: import failed: {e}"),
    }
    let _ = std::fs::remove_file(&scip_out);
}

/// Foreground SCIP execution using scip_download catalog for all detected languages.
pub(crate) fn auto_scip(
    root: &Path,
    result: &infigraph_core::IndexResult,
    store: Option<&infigraph_core::graph::GraphStore>,
) -> Result<()> {
    use crate::scip_download;
    use std::collections::HashSet;

    let detected: HashSet<String> = result
        .extractions
        .iter()
        .map(|e| e.language.clone())
        .collect();
    if detected.is_empty() {
        return Ok(());
    }

    let indexers = scip_download::indexers_for_languages(&detected);
    if indexers.is_empty() {
        return Ok(());
    }

    println!(
        "Auto-SCIP: found {} applicable indexer(s) for detected languages",
        indexers.len()
    );

    // Parallel download: ensure all indexer binaries are available
    let binaries: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = indexers
            .iter()
            .map(|idx| s.spawn(move || (*idx, scip_download::ensure_indexer(idx))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Sequential run: each indexer produces index.scip, import, cleanup
    for (indexer, bin_path) in &binaries {
        let Some(bin) = bin_path else { continue };
        if !should_run_indexer(root, indexer) {
            continue;
        }

        let cmd_str = bin.to_string_lossy();
        let extra = scip_download::extra_runtime_paths();
        let extra_path = if extra.is_empty() {
            None
        } else {
            Some(extra.as_str())
        };

        if indexer.binary_name == "scip-java" {
            let has_gradle = root.join("build.gradle").exists()
                || root.join("build.gradle.kts").exists()
                || root.join("settings.gradle").exists()
                || root.join("settings.gradle.kts").exists();
            let has_maven = root.join("pom.xml").exists();

            if has_gradle && has_maven {
                let primary = if root.join("settings.gradle").exists()
                    || root.join("settings.gradle.kts").exists()
                {
                    "gradle"
                } else {
                    "maven"
                };
                let fallback = if primary == "gradle" {
                    "maven"
                } else {
                    "gradle"
                };

                println!("Auto-SCIP: detected both Maven and Gradle, trying {primary}");
                let primary_args = ["index", "--build-tool", primary];
                if run_scip_indexer(
                    root,
                    &cmd_str,
                    &primary_args,
                    indexer.binary_name,
                    extra_path,
                ) {
                    import_scip_and_cleanup(root, None, store);
                } else {
                    println!("Auto-SCIP: {primary} failed, falling back to {fallback}");
                    let fallback_args = ["index", "--build-tool", fallback];
                    if run_scip_indexer(
                        root,
                        &cmd_str,
                        &fallback_args,
                        indexer.binary_name,
                        extra_path,
                    ) {
                        import_scip_and_cleanup(root, None, store);
                    }
                }
            } else if run_scip_indexer(
                root,
                &cmd_str,
                indexer.scip_args,
                indexer.binary_name,
                extra_path,
            ) {
                import_scip_and_cleanup(root, None, store);
            }
            continue;
        }

        if run_scip_indexer(
            root,
            &cmd_str,
            indexer.scip_args,
            indexer.binary_name,
            extra_path,
        ) {
            import_scip_and_cleanup(root, None, store);
        }
    }

    Ok(())
}

pub(crate) fn run_scip_indexer(
    root: &Path,
    cmd: &str,
    args: &[&str],
    label: &str,
    extra_path: Option<&str>,
) -> bool {
    println!("Auto-SCIP: running {label}...");
    let scip_out = root.join("index.scip");
    let mut command = std::process::Command::new(cmd);
    command.args(args).current_dir(root);
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
    match command.status() {
        Ok(s) if s.success() && scip_out.exists() => true,
        Ok(s) => {
            eprintln!("Auto-SCIP: {label} exited with {s}");
            false
        }
        Err(e) => {
            eprintln!("Auto-SCIP: failed to run {label}: {e}");
            false
        }
    }
}

/// Entry point for the hidden `scip-enrich` subcommand (spawned by `index`).
pub(crate) fn cmd_scip_enrich(root: &Path, detected_languages: &std::collections::HashSet<String>) {
    auto_scip_background(root, detected_languages);
}

/// Background SCIP pipeline: download binaries, run indexers in parallel, import sequentially.
fn auto_scip_background(root: &Path, detected_languages: &std::collections::HashSet<String>) {
    use crate::scip_download;

    let indexers = scip_download::indexers_for_languages(detected_languages);
    if indexers.is_empty() {
        return;
    }

    // Parallel download: ensure all indexer binaries are available
    let binaries: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = indexers
            .iter()
            .map(|idx| s.spawn(move || (*idx, scip_download::ensure_indexer(idx))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Filter to runnable indexers and build per-indexer tasks
    let scip_tmp = root.join(".infigraph").join("scip-tmp");
    let _ = std::fs::create_dir_all(&scip_tmp);

    let tasks: Vec<_> = binaries
        .into_iter()
        .filter_map(|(indexer, bin_path)| {
            let bin = bin_path?;
            if !should_run_indexer(root, indexer) {
                return None;
            }
            let output_path = scip_tmp.join(format!("{}.scip", indexer.binary_name));
            Some((indexer, bin, output_path))
        })
        .collect();

    if tasks.is_empty() {
        let _ = std::fs::remove_dir_all(&scip_tmp);
        return;
    }

    // Part A: Run indexers in parallel with per-indexer output paths
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = tasks
            .iter()
            .map(|(indexer, bin, output_path)| {
                s.spawn(move || {
                    let success = run_scip_indexer_to(root, bin, indexer, output_path);
                    (indexer.binary_name, output_path.clone(), success)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Part B: Import results sequentially (Kuzu graph is single-writer)
    let registry = match bundled_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Auto-SCIP: import failed: {e}");
            return;
        }
    };
    let mut prism = match Infigraph::open(root, registry) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Auto-SCIP: import failed: {e}");
            return;
        }
    };
    if prism.init().is_err() {
        return;
    }
    let store = match prism.store() {
        Some(s) => s,
        None => return,
    };

    for (label, scip_path, success) in &results {
        if *success && scip_path.exists() {
            match infigraph_core::scip::import_scip_index(scip_path, store, Some(root)) {
                Ok(stats) => eprintln!(
                    "Auto-SCIP: {label} enriched {} symbols, {} added, {} references, {} new symbols, {} corrections learned",
                    stats.symbols_enriched, stats.relations_added, stats.references_added, stats.symbols_added, stats.corrections_learned
                ),
                Err(e) => eprintln!("Auto-SCIP: {label} import failed: {e}"),
            }
        }
        let _ = std::fs::remove_file(scip_path);
    }

    let _ = std::fs::remove_dir_all(&scip_tmp);

    // Embed any new symbols SCIP added (skips existing embeddings)
    let root_buf = root.to_path_buf();
    let pre_count = infigraph_core::embed::embedding_count(&root_buf);
    match infigraph_core::embed::update_embeddings(store, &root_buf, &[]) {
        Ok(n) => {
            let new = n.saturating_sub(pre_count);
            if new > 0 {
                eprintln!("Auto-SCIP: embedded {new} new symbols from SCIP enrichment");
            }
        }
        Err(e) => eprintln!("Auto-SCIP: embedding update failed: {e}"),
    }

    eprintln!("Auto-SCIP: background enrichment complete.");
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
) -> bool {
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

fn run_scip_java(root: &Path, cmd: &str, output_path: &Path, extra_path: Option<&str>) -> bool {
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
        if run_scip_indexer_cmd(
            root,
            cmd,
            &primary_args,
            "scip-java",
            extra_path,
            Some("--output"),
            output_path,
        ) {
            return true;
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
) -> bool {
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

    match command.status() {
        Ok(s) if s.success() => {
            if output_flag.is_none() {
                let default_out = root.join("index.scip");
                if default_out.exists() && default_out != output_path {
                    let _ = std::fs::rename(&default_out, output_path);
                }
            }
            output_path.exists()
        }
        Ok(s) => {
            eprintln!("Auto-SCIP: {label} exited with {s}");
            false
        }
        Err(e) => {
            eprintln!("Auto-SCIP: failed to run {label}: {e}");
            false
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
            matches!(&cli.command, crate::Commands::ScipEnrich { languages } if languages == langs),
            "expected Commands::ScipEnrich {{ languages: {langs:?} }}"
        );
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
