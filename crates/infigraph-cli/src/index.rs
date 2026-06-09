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

    let stats = prism.stats()?;
    println!("\n{}", stats);

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
        auto_scip(root, &result)?;
        return Ok(());
    }
    {
        let store = prism.store().context("graph not initialized")?;
        let changed: Vec<&str> = result.extractions.iter().map(|e| e.file.as_str()).collect();
        let count = infigraph_core::embed::update_embeddings(store, root, &changed)?;
        println!("Saved {} embeddings to .infigraph/embeddings.bin", count);
    }

    // Auto-index documents (PDF, DOCX, XML, Markdown, etc.)
    match crate::commands::cmd_index_docs(root) {
        Ok(()) => {}
        Err(e) => eprintln!("warning: document indexing failed: {e}"),
    }

    // Auto-SCIP: detect languages and run available SCIP indexers
    auto_scip(root, &result)?;

    Ok(())
}

pub(crate) fn on_path(cmd: &str) -> bool {
    let lookup = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(lookup)
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Try to install an LSP server automatically. Returns true if now available.
pub(crate) fn try_install_lsp(lsp_server: &str) -> bool {
    if on_path(lsp_server) {
        return true;
    }

    let os = std::env::consts::OS;
    let has_brew = on_path("brew");
    let has_apt = on_path("apt-get");
    let has_npm = on_path("npm");
    let has_pip = on_path("pip3") || on_path("pip");
    let has_gem = on_path("gem");
    let has_cargo = on_path("cargo");
    let has_opam = on_path("opam");
    let has_ghcup = on_path("ghcup");
    let has_dotnet = on_path("dotnet");

    #[allow(clippy::type_complexity)]
    let installs: &[(&str, &[(&str, &str, &[&str])])] = &[
        ("typescript-language-server", &[("any", "npm", &["install", "-g", "typescript-language-server"])]),
        ("pylsp", &[("any", "pip3", &["install", "python-lsp-server"]), ("any", "pip", &["install", "python-lsp-server"])]),
        ("rust-analyzer", &[("any", "rustup", &["component", "add", "rust-analyzer"])]),
        ("solargraph", &[("any", "gem", &["install", "solargraph"])]),
        ("lua-language-server", &[("macos", "brew", &["install", "lua-language-server"]), ("linux", "apt-get", &["install", "-y", "lua-language-server"])]),
        ("clangd", &[("macos", "brew", &["install", "llvm"]), ("linux", "apt-get", &["install", "-y", "clangd"])]),
        ("zls", &[("any", "cargo", &["install", "zls"])]),
        ("clojure-lsp", &[("macos", "brew", &["install", "clojure-lsp/brew/clojure-lsp-native"])]),
        ("ocamllsp", &[("any", "opam", &["install", "ocaml-lsp-server"])]),
        ("haskell-language-server-wrapper", &[("any", "ghcup", &["install", "hls"])]),
        ("fsautocomplete", &[("any", "dotnet", &["tool", "install", "-g", "fsautocomplete"])]),
        ("pasls", &[("macos", "brew", &["install", "fpc"]), ("linux", "apt-get", &["install", "-y", "fpc"])]),
        ("intelephense", &[("any", "npm", &["install", "-g", "intelephense"])]),
        ("erlang-ls", &[("macos", "brew", &["install", "erlang-ls"]), ("linux", "apt-get", &["install", "-y", "erlang-ls"])]),
        ("jdtls", &[("macos", "brew", &["install", "jdtls"]), ("linux", "apt-get", &["install", "-y", "jdtls"])]),
        ("gopls", &[("any", "go", &["install", "golang.org/x/tools/gopls@latest"])]),
        ("omnisharp", &[("any", "dotnet", &["tool", "install", "-g", "csharp-ls"])]),
        ("sourcekit-lsp", &[("macos", "brew", &["install", "swift"])]),
        ("dart", &[("macos", "brew", &["install", "dart"]), ("linux", "apt-get", &["install", "-y", "dart"])]),
        ("elixir-ls", &[("macos", "brew", &["install", "elixir-ls"]), ("linux", "apt-get", &["install", "-y", "elixir-ls"])]),
        ("pls", &[("any", "cpan", &["App::PerlLanguageServer"])]),
    ];

    let avail = |installer: &str| match installer {
        "npm" => has_npm,
        "pip3" | "pip" => has_pip,
        "gem" => has_gem,
        "cargo" => has_cargo,
        "brew" => has_brew,
        "apt-get" => has_apt,
        "opam" => has_opam,
        "ghcup" => has_ghcup,
        "dotnet" => has_dotnet,
        "rustup" => on_path("rustup"),
        _ => false,
    };

    if let Some((_, cmds)) = installs.iter().find(|(s, _)| *s == lsp_server) {
        for (target_os, installer, args) in *cmds {
            if (*target_os != "any" && *target_os != os) || !avail(installer) {
                continue;
            }
            println!("Auto-SCIP: installing {} via {}...", lsp_server, installer);
            let ok = std::process::Command::new(installer)
                .args(*args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok && on_path(lsp_server) {
                println!("Auto-SCIP: {} installed", lsp_server);
                return true;
            }
            break;
        }
    }

    false
}

pub(crate) fn run_scip_indexer(root: &Path, cmd: &str, args: &[&str], label: &str) -> bool {
    println!("Auto-SCIP: {} found — enriching graph...", label);
    let scip_out = root.join("index.scip");
    match std::process::Command::new(cmd)
        .args(args)
        .current_dir(root)
        .status()
    {
        Ok(s) if s.success() && scip_out.exists() => true,
        Ok(s) => {
            eprintln!("Auto-SCIP: {} exited with {}", label, s);
            false
        }
        Err(e) => {
            eprintln!("Auto-SCIP: failed to run {}: {}", label, e);
            false
        }
    }
}

pub(crate) fn try_lsp_bridge(root: &Path, lsp_server: &str, lang: &str) -> bool {
    if !on_path("lsp-to-scip") || !on_path(lsp_server) {
        return false;
    }
    println!(
        "Auto-SCIP: lsp-to-scip + {} — enriching graph...",
        lsp_server
    );
    let scip_out = root.join("index.scip");
    match std::process::Command::new("lsp-to-scip")
        .args([
            "--server",
            lsp_server,
            "--lang",
            lang,
            "--out",
            "index.scip",
        ])
        .current_dir(root)
        .status()
    {
        Ok(s) if s.success() && scip_out.exists() => true,
        Ok(s) => {
            eprintln!("Auto-SCIP: lsp-to-scip exited with {}", s);
            false
        }
        Err(e) => {
            eprintln!("Auto-SCIP: lsp-to-scip failed: {}", e);
            false
        }
    }
}

pub(crate) fn import_scip_and_cleanup(root: &Path) {
    let scip_out = root.join("index.scip");
    if !scip_out.exists() {
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
    match infigraph_core::scip::import_scip_index(&scip_out, store) {
        Ok(stats) => println!(
            "Auto-SCIP: enriched {} symbols, {} relations added",
            stats.symbols_enriched, stats.relations_added
        ),
        Err(e) => eprintln!("Auto-SCIP: import failed: {e}"),
    }
    let _ = std::fs::remove_file(&scip_out);
}

pub(crate) fn auto_scip(root: &Path, result: &infigraph_core::IndexResult) -> Result<()> {
    let mut lang_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for ext in &result.extractions {
        *lang_counts.entry(ext.language.clone()).or_insert(0) += 1;
    }
    if lang_counts.is_empty() {
        return Ok(());
    }
    let dominant = lang_counts
        .iter()
        .max_by_key(|(_, c)| *c)
        .map(|(l, _)| l.clone())
        .unwrap();

    #[allow(clippy::type_complexity)]
    let entries: &[(&[&str], &str, &[&str], &str, &str, &str)] = &[
        (&["typescript","javascript","tsx"], "scip-typescript", &["index"],             "typescript-language-server", "typescript", "npm i -g @sourcegraph/scip-typescript"),
        (&["python"],                        "scip-python",      &["index","--cwd","."], "pylsp",                      "python",     "pip install scip-python"),
        (&["rust"],                          "rust-analyzer",    &["scip","."],          "rust-analyzer",              "rust",       "rustup component add rust-analyzer"),
        (&["java","kotlin"],                 "scip-java",        &["index"],             "jdtls",                      "java",       "brew install scip-java  # or download from github.com/sourcegraph/scip-java"),
        (&["go"],                            "scip-go",          &["--cwd","."],         "gopls",                      "go",         "go install github.com/sourcegraph/scip-go@latest"),
        (&["c","cpp"],                       "",                 &[],                    "clangd",                     "cpp",        "brew install llvm  # provides clangd"),
        (&["csharp"],                        "",                 &[],                    "omnisharp",                  "csharp",     "dotnet tool install -g csharp-ls"),
        (&["ruby"],                          "",                 &[],                    "solargraph",                 "ruby",       "gem install solargraph"),
        (&["swift"],                         "",                 &[],                    "sourcekit-lsp",              "swift",      "brew install swift  # includes sourcekit-lsp"),
        (&["dart"],                          "",                 &[],                    "dart",                       "dart",       "brew install dart"),
        (&["elixir"],                        "",                 &[],                    "elixir-ls",                  "elixir",     "brew install elixir-ls"),
        (&["haskell"],                       "",                 &[],                    "haskell-language-server-wrapper", "haskell", "ghcup install hls"),
        (&["lua"],                           "",                 &[],                    "lua-language-server",        "lua",        "brew install lua-language-server"),
        (&["php"],                           "",                 &[],                    "intelephense",               "php",        "npm i -g intelephense"),
        (&["zig"],                           "",                 &[],                    "zls",                        "zig",        "brew install zls"),
        (&["pascal"],                        "DelphiLSP64.exe",  &[],                    "pasls",                      "pascal",     "Windows only: place DelphiLSP64.exe on PATH"),
        (&["fsharp"],                        "",                 &[],                    "fsautocomplete",             "fsharp",     "dotnet tool install -g fsautocomplete"),
        (&["clojure"],                       "",                 &[],                    "clojure-lsp",                "clojure",    "brew install clojure-lsp/brew/clojure-lsp-native"),
        (&["erlang"],                        "",                 &[],                    "erlang-ls",                  "erlang",     "brew install erlang-ls"),
        (&["perl"],                          "",                 &[],                    "pls",                        "perl",       "cpan App::PerlLanguageServer"),
        (&["ocaml"],                         "",                 &[],                    "ocamllsp",                   "ocaml",      "opam install ocaml-lsp-server"),
    ];

    for (lang_tags, scip_cmd, scip_args, lsp_server, lsp_lang, install_hint) in entries {
        if !lang_tags.iter().any(|t| *t == dominant) {
            continue;
        }

        let has_scip = !scip_cmd.is_empty() && on_path(scip_cmd);
        let has_lsp = on_path(lsp_server) || (!has_scip && try_install_lsp(lsp_server));

        if !has_scip && !has_lsp {
            println!(
                "Auto-SCIP: {} detected but no indexer found — for compiler-grade enrichment install:\n  {}",
                lang_tags[0], install_hint
            );
            continue;
        }

        let indexed = if has_scip {
            let ok = run_scip_indexer(root, scip_cmd, scip_args, scip_cmd);
            if !ok && try_install_lsp(scip_cmd) {
                run_scip_indexer(root, scip_cmd, scip_args, scip_cmd)
            } else {
                ok
            }
        } else {
            try_lsp_bridge(root, lsp_server, lsp_lang)
        };

        if indexed {
            import_scip_and_cleanup(root);
        }
    }

    Ok(())
}
