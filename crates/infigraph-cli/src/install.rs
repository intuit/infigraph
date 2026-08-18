use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config_targets::{self, ConfigFormat, AGENT_TARGETS};

/// Locate the infigraph-mcp binary: first check the same directory as the running
/// binary, then fall back to searching PATH.
pub(crate) fn find_mcp_binary() -> Result<PathBuf> {
    let bin_name = if cfg!(windows) {
        "infigraph-mcp.exe"
    } else {
        "infigraph-mcp"
    };

    // Check sibling of the running binary
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().unwrap().join(bin_name);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    // Fall back to PATH (use `where` on Windows, `which` elsewhere)
    let lookup = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(lookup).arg(bin_name).output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let path = stdout.lines().next().unwrap_or("").trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    anyhow::bail!(
        "Could not find infigraph-mcp binary. \
         Build it with `cargo build -p infigraph-mcp` or ensure it is on your PATH."
    )
}

pub(crate) fn cmd_install() -> Result<()> {
    let mcp_path = find_mcp_binary()?;
    let mcp_path_str = mcp_path.to_string_lossy().to_string();

    println!("Found infigraph-mcp at: {}", mcp_path_str);

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let mut configured = Vec::new();

    for target in AGENT_TARGETS {
        let dir = home.join(target.dir_name);

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;

        let config_path = if target.config_file == "CLAUDE_CODE_SPECIAL" {
            home.join(".claude.json")
        } else {
            dir.join(target.config_file)
        };

        match target.format {
            ConfigFormat::Json => config_targets::install_json_target(&config_path, &mcp_path_str)?,
            ConfigFormat::Toml => config_targets::install_toml_target(&config_path, &mcp_path_str)?,
            ConfigFormat::CopilotJson => {
                config_targets::install_copilot_target(&config_path, &mcp_path_str)?
            }
            ConfigFormat::OpenCodeJson => {
                config_targets::install_opencode_target(&config_path, &mcp_path_str)?
            }
        }

        configured.push(target.label);
        println!("  Configured {} ({})", target.label, config_path.display());
    }

    if configured.is_empty() {
        println!("No agents were configured.");
    } else {
        print_capabilities_summary(&configured);
    }

    // Write primary search instructions to ~/.claude/CLAUDE.md
    write_claude_md_instructions(&home)?;

    // Write cursor/windsurf rules
    write_editor_rules(&home)?;

    // Write /infigraph-reindex command to ~/.claude/commands/
    write_reindex_command(&home)?;

    // Install hooks and Claude Code allowlist
    crate::hooks::install_enforcement_hook(&home)?;
    crate::hooks::install_edit_tracker_hook(&home)?;
    crate::hooks::install_session_save_hook(&home)?;
    crate::hooks::install_clear_suggest_hook(&home)?;
    crate::hooks::install_clear_guard_hook(&home)?;
    crate::hooks::install_session_end_hook(&home)?;
    crate::hooks::install_test_context_sentinel_hook(&home)?;
    crate::hooks::install_search_fallback_sentinel_hook(&home)?;
    crate::hooks::install_claude_allowlist(&home)?;

    // Copy model files to ~/.infigraph/models/
    install_models(&mcp_path, &home)?;

    Ok(())
}

fn write_claude_md_instructions(home: &Path) -> Result<()> {
    let claude_md = home.join(".claude").join("CLAUDE.md");
    let marker = "<!-- infigraph-primary-search -->";
    let instructions = format!(
        r#"
{marker}
## Infigraph — Primary Code Intelligence

Infigraph MCP is indexed. Use Infigraph tools FIRST for all code tasks. Fall back to grep/Read only if Infigraph returns nothing or for non-code files.

### Rules
1. Check `list_projects` before indexing — don't re-index
2. **`search`** for ALL code search — hybrid BM25+vector+grep in one call, auto-escalates
3. **`get_doc_context`** before editing any function — returns source+callers+callees in one call
4. **`trace_callers`** / **`find_all_references`** before refactoring — never grep for callers
5. **`trace_callees`** / **`transitive_impact`** for blast radius — never manually trace call chains
6. Read files directly only for non-code files (configs, docs, manifests) or Edit tool line-number context

### Workflows
- **Find code:** `search` → if need symbol detail: `get_code_snippet` or `symbol_context`
- **Before editing:** `get_doc_context`
- **Before refactoring:** `find_all_references` → `transitive_impact` → edit
- **Onboarding:** `index_project` → `get_architecture` → `get_stats`
- **Multi-repo:** `group_create` → `group_add` × N → `group_index` → `group_sync` → `group_link`

### Subagents — infigraph-indexed projects
Do NOT spawn these agent types for code tasks — they lack MCP access and will fall back to grep/glob:
- **Explore** → use `search`, `search_code`, `search_symbols` directly instead
- **Plan** → use `get_architecture`, `get_skeleton`, `get_stats` directly instead
- **code-reviewer** → use `get_doc_context`, `get_code_snippet`, `review` directly instead

For tasks requiring a subagent, use **general-purpose** — it has full MCP/infigraph access.

### Verbose tools — delegate to subagent
`get_architecture`, `transitive_impact`, `detect_dead_code`, `detect_clusters`, `detect_clones`, `export_graph`, `query_graph`, `trace_callers`/`trace_callees` (deep), `group_query`, `group_index`

> All other Infigraph tools are safe to call inline. Each tool description says what it replaces — check descriptions when unsure which tool to use.

**Reindex:** `/infigraph-reindex [path]` — always runs in subagent.

### Session Continuity — MANDATORY
- **On session start:** MUST call `get_latest_session` to resume prior context
- **After context compaction:** if you see "continued from a previous conversation" or a compaction summary, IMMEDIATELY call `save_session` with whatever context survived before doing anything else
- **MUST call `save_session` IMMEDIATELY (before responding to the user)** when ANY of these occur. No session-end signal exists — if you don't save now, context is lost forever:
  1. **Finding** — root cause identified, discovered a bug, learned how something works
  2. **Milestone** — bug fixed and verified, feature committed, test passing, build green
  3. **Decision** — chose an approach, ruled something out, changed strategy
  4. **Task done** — any pending task from a prior session is completed
  5. **Periodic** — if you have NOT called `save_session` in the last 5 exchanges with the user, call it NOW regardless of whether anything dramatic happened. This is a hard rule, not a suggestion.
- Do NOT defer saves ("I'll save later"). Do NOT batch them. Do NOT wait for user to ask.
- "Later" does not exist — context compaction or session end can happen at any moment.
- **Before `/clear`:** ALWAYS call `save_session` first — `/clear` wipes context and LM2 can only restore what was persisted. Unsaved reasoning, decisions, and in-flight work will be lost.
- Same-day saves merge: summary/pending_tasks overwrite, decisions append, files_touched union
- **Narrative dumps:** On every `save_session`, include `narrative` field with full session story — what was explored, found, reasoned, decided, and why. Chronological prose, not terse bullets. Written to `.infigraph/sessions/session_YYYY-MM-DD.md` and embedded for semantic search. On session start, if `get_latest_session` shows a narrative log path, read it when structured fields aren't enough context.

### Session Field Guide
- **decisions** — structured format: `Goal: X. Decision: Y. Why: Z. Invalidates-if: W.`
- **constraints** — things that failed: `Tried: X. Failed because: Y. Do not retry unless: Z.`
- **assumptions** — what current approach depends on: `Assumes: X. If X changes: Y.`
- **blockers** — stuck items needing human input or external dependency
- **narrative** — full session story: explorations, findings, reasoning, code changes, decisions in chronological order. Write as prose, not structured fields.
"#
    );

    let existing = std::fs::read_to_string(&claude_md).unwrap_or_default();
    let new_content = if let Some(start) = existing.find(marker) {
        let after = &existing[start..];
        let end = after[marker.len()..]
            .find("\n<!-- ")
            .map(|p| start + marker.len() + p + 1)
            .unwrap_or(existing.len());
        format!("{}{}{}", &existing[..start], instructions, &existing[end..])
    } else {
        format!("{}\n{}", existing, instructions)
    };
    std::fs::write(&claude_md, new_content)?;
    println!(
        "  Updated primary search instructions in {}",
        claude_md.display()
    );
    Ok(())
}

// Project-level CLAUDE.md generation moved to infigraph_core::claude_md

fn write_editor_rules(home: &Path) -> Result<()> {
    let marker = "<!-- infigraph-primary-search -->";
    let instructions = crate::agent::infigraph_instructions();

    // Write .cursorrules to ~/.cursor/rules/infigraph.mdc
    let cursor_rules_dir = home.join(".cursor").join("rules");
    if home.join(".cursor").exists() {
        std::fs::create_dir_all(&cursor_rules_dir)?;
        let cursor_rule = cursor_rules_dir.join("infigraph.mdc");
        let cursor_content = format!(
            "---\ndescription: Infigraph primary code intelligence rules\nglobs: \nalwaysApply: true\n---\n\n{instructions}"
        );
        std::fs::write(&cursor_rule, cursor_content)?;
        println!("  Updated Cursor rules in {}", cursor_rule.display());
    }

    // Write .windsurfrules to ~/.windsurf/rules/infigraph.md
    let windsurf_rules_dir = home.join(".windsurf").join("rules");
    if home.join(".windsurf").exists() {
        std::fs::create_dir_all(&windsurf_rules_dir)?;
        let windsurf_rule = windsurf_rules_dir.join("infigraph.md");
        std::fs::write(&windsurf_rule, instructions)?;
        println!("  Updated Windsurf rules in {}", windsurf_rule.display());
    }

    let _ = marker;
    Ok(())
}

fn write_reindex_command(home: &Path) -> Result<()> {
    let commands_dir = home.join(".claude").join("commands");
    std::fs::create_dir_all(&commands_dir)?;
    let reindex_cmd = commands_dir.join("infigraph-reindex.md");
    let reindex_content = r#"# Infigraph Reindex

Reindex the project directly (no subagent — saves tokens).

## Usage

```
/infigraph-reindex [path]
```

If `path` is omitted, uses the current working directory.

## Instructions

1. Determine project path: use the argument provided, or fall back to the current working directory.
2. Load the tool schema: `ToolSearch("select:mcp__infigraph__index_project")`
3. Call `mcp__infigraph__index_project` with that path directly (do NOT spawn an Agent).
4. Report back in this exact format (nothing else):

```
Reindexed: <path>
Files: <N> | Symbols: <N> | Calls: <N> resolved / <N> unresolved
Languages: <comma-separated list with file counts>
```

If indexing fails, report the error verbatim. Do not attempt fixes.
"#;
    // Always overwrite to pick up content updates
    std::fs::write(&reindex_cmd, reindex_content)?;
    println!(
        "  Updated /infigraph-reindex command at {}",
        reindex_cmd.display()
    );
    Ok(())
}

pub(crate) fn install_models(mcp_path: &Path, home: &Path) -> Result<()> {
    let dest = home
        .join(".infigraph")
        .join("models")
        .join("potion-base-8M");

    let model_files = ["config.json", "model.safetensors", "tokenizer.json"];
    let mut src: Option<PathBuf> = None;
    let mut dir = mcp_path.parent().unwrap_or(Path::new("/"));
    loop {
        let candidate = dir.join("models").join("potion-base-8M");
        if candidate.join("model.safetensors").exists() {
            src = Some(candidate);
            break;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }

    let Some(src) = src else {
        println!("  Model files not found near binary — skipping model install (semantic search will use trigram fallback)");
        return Ok(());
    };

    let src_size = std::fs::metadata(src.join("model.safetensors"))
        .map(|m| m.len())
        .unwrap_or(0);
    let dest_size = std::fs::metadata(dest.join("model.safetensors"))
        .map(|m| m.len())
        .unwrap_or(0);
    if dest_size > 0 && dest_size == src_size {
        println!("  Model already installed at {}", dest.display());
        return Ok(());
    }

    std::fs::create_dir_all(&dest)
        .with_context(|| format!("Failed to create {}", dest.display()))?;
    for file in &model_files {
        std::fs::copy(src.join(file), dest.join(file))
            .with_context(|| format!("Failed to copy model file {file}"))?;
    }
    println!("  Installed semantic model to {}", dest.display());
    Ok(())
}

pub(crate) fn cmd_uninstall() -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let mut removed = Vec::new();

    for target in AGENT_TARGETS {
        let config_path = if target.config_file == "CLAUDE_CODE_SPECIAL" {
            home.join(".claude.json")
        } else {
            home.join(target.dir_name).join(target.config_file)
        };

        let result = match target.format {
            ConfigFormat::Json | ConfigFormat::CopilotJson => {
                config_targets::uninstall_json_target(&config_path, target.label)?
            }
            ConfigFormat::Toml => {
                config_targets::uninstall_toml_target(&config_path, target.label)?
            }
            ConfigFormat::OpenCodeJson => {
                config_targets::uninstall_opencode_target(&config_path, target.label)?
            }
        };

        if let Some(label) = result {
            removed.push(label);
        }
    }

    if removed.is_empty() {
        println!("No agents had infigraph configured.");
    } else {
        println!(
            "\nUninstalled infigraph MCP server from {} agent(s): {}",
            removed.len(),
            removed.join(", ")
        );
    }

    // Remove primary search instructions from ~/.claude/CLAUDE.md
    let claude_md = home.join(".claude").join("CLAUDE.md");
    let marker = "<!-- infigraph-primary-search -->";
    if claude_md.exists() {
        let content = std::fs::read_to_string(&claude_md)?;
        if let Some(start) = content.find(marker) {
            let new_content = content[..start].trim_end().to_string();
            std::fs::write(
                &claude_md,
                if new_content.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", new_content)
                },
            )?;
            println!(
                "  Removed primary search instructions from {}",
                claude_md.display()
            );
        }
    }

    // Remove Cursor rules
    let cursor_rule = home.join(".cursor").join("rules").join("infigraph.mdc");
    if cursor_rule.exists() {
        std::fs::remove_file(&cursor_rule)?;
        println!("  Removed Cursor rules: {}", cursor_rule.display());
    }

    // Remove Windsurf rules
    let windsurf_rule = home.join(".windsurf").join("rules").join("infigraph.md");
    if windsurf_rule.exists() {
        std::fs::remove_file(&windsurf_rule)?;
        println!("  Removed Windsurf rules: {}", windsurf_rule.display());
    }

    // Remove /infigraph-reindex skill from ~/.claude/commands/
    let reindex_cmd = home
        .join(".claude")
        .join("commands")
        .join("infigraph-reindex.md");
    if reindex_cmd.exists() {
        std::fs::remove_file(&reindex_cmd)?;
        println!("  Removed skill: {}", reindex_cmd.display());
    }

    // Remove hooks and Claude Code allowlist
    crate::hooks::uninstall_hooks(&home)?;
    crate::hooks::uninstall_claude_allowlist(&home)?;

    // Remove binaries from ~/.local/bin/
    for bin in &["infigraph", "infigraph-mcp"] {
        let bin_path = home.join(".local").join("bin").join(bin);
        if bin_path.exists() {
            std::fs::remove_file(&bin_path)?;
            println!("  Removed binary: {}", bin_path.display());
        }
    }

    // Remove project-level CLAUDE.md managed blocks from all registered projects
    if let Ok(registry) = infigraph_core::multi::Registry::load() {
        for (name, entry) in &registry.repos {
            match infigraph_core::claude_md::remove_project_claude_md(&entry.path) {
                Ok(true) => println!("  Removed CLAUDE.md block from {}", name),
                Ok(false) => {}
                Err(e) => eprintln!("  warning: failed to clean CLAUDE.md for {}: {e}", name),
            }
        }
    }

    // Remove model cache ~/.infigraph/
    let model_cache = home.join(".infigraph");
    if model_cache.exists() {
        std::fs::remove_dir_all(&model_cache)?;
        println!("  Removed model cache: {}", model_cache.display());
    }

    Ok(())
}

pub(crate) fn platform_triple() -> Result<(String, String, String)> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let os_tag = match os {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        _ => anyhow::bail!("unsupported OS: {os}"),
    };
    let arch_tag = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => anyhow::bail!("unsupported architecture: {arch}"),
    };
    let target = format!("{arch_tag}-{os_tag}");
    Ok((os_tag.to_string(), arch_tag.to_string(), target))
}

pub(crate) fn self_update(version: &str) -> Result<()> {
    let os = std::env::consts::OS;
    let (_, _, target) = platform_triple()?;
    let archive_ext = if os == "windows" { "zip" } else { "tar.gz" };
    let asset_name = format!("infigraph-{target}.{archive_ext}");
    let tag = format!("v{version}");

    let gh_host = std::env::var("INFIGRAPH_GH_HOST").unwrap_or_else(|_| "github.com".to_string());
    let gh_owner = std::env::var("INFIGRAPH_GH_OWNER").unwrap_or_else(|_| "intuit".to_string());
    let gh_repo = "infigraph";
    let full_repo = format!("{gh_host}/{gh_owner}/{gh_repo}");

    println!("Downloading {asset_name} from release {tag}...");

    let install_dir = std::env::var("INFIGRAPH_INSTALL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("bin")
        });

    let tmp_dir = std::env::temp_dir();
    let download_path = tmp_dir.join(&asset_name);

    let mut gh_args = vec![
        "release".to_string(),
        "download".to_string(),
        tag.clone(),
        "--repo".to_string(),
        format!("{gh_owner}/{gh_repo}"),
        "--pattern".to_string(),
        asset_name.clone(),
        "--dir".to_string(),
        tmp_dir.to_string_lossy().to_string(),
        "--clobber".to_string(),
    ];
    if gh_host != "github.com" {
        gh_args.push("--hostname".to_string());
        gh_args.push(gh_host.clone());
    }

    let status = std::process::Command::new("gh")
        .args(&gh_args)
        .status()
        .context("failed to run `gh release download`")?;

    if !status.success() {
        anyhow::bail!("download failed for {asset_name} in release {tag} from {full_repo}");
    }

    std::fs::create_dir_all(&install_dir)?;

    let bin_suffix = if os == "windows" { ".exe" } else { "" };
    for bin in &["infigraph", "infigraph-mcp", "lsp-to-scip"] {
        let bin_path = install_dir.join(format!("{bin}{bin_suffix}"));
        let old_path = install_dir.join(format!("{bin}{bin_suffix}.old"));
        if bin_path.exists() {
            let _ = std::fs::remove_file(&old_path);
            let _ = std::fs::rename(&bin_path, &old_path);
        }
    }

    if archive_ext == "zip" {
        let status = std::process::Command::new("unzip")
            .args([
                "-o",
                &download_path.to_string_lossy(),
                "-d",
                &install_dir.to_string_lossy(),
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to extract zip");
        }
    } else {
        let status = std::process::Command::new("tar")
            .args([
                "-xzf",
                &download_path.to_string_lossy(),
                "-C",
                &install_dir.to_string_lossy(),
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to extract tar.gz");
        }
    }

    let _ = std::fs::remove_file(&download_path);

    for bin in &["infigraph", "infigraph-mcp", "lsp-to-scip"] {
        let _ = std::fs::remove_file(install_dir.join(format!("{bin}{bin_suffix}.old")));
    }

    if os == "macos" {
        for bin in &["infigraph", "infigraph-mcp", "lsp-to-scip"] {
            let _ = std::process::Command::new("xattr")
                .args([
                    "-dr",
                    "com.apple.quarantine",
                    &install_dir.join(bin).to_string_lossy(),
                ])
                .status();
        }
    }

    if let Some(cache_path) = update_cache_path() {
        let _ = std::fs::remove_file(&cache_path);
    }

    println!("Installed v{version} to {}", install_dir.display());
    Ok(())
}

pub(crate) fn print_capabilities_summary(configured: &[&str]) {
    let version = env!("CARGO_PKG_VERSION");
    let count = configured.len();
    let agents = configured.join(", ");

    println!();
    println!("Infigraph v{version} installed for {count} agent(s): {agents}");
    println!();
    println!("What you can do now:");
    println!();
    println!("  Index & Search");
    println!("    infigraph index              Index your codebase (code + docs)");
    println!("    infigraph search \"query\"      Hybrid BM25 + semantic search");
    println!("    infigraph search-docs \"q\"     Search indexed documents");
    println!();
    println!("  Analysis");
    println!("    infigraph dead-code          Find unreachable functions");
    println!("    infigraph security           Scan for vulnerabilities (30+ patterns)");
    println!("    infigraph complexity         Cyclomatic complexity hotspots");
    println!("    infigraph check              CI quality gate (exit non-zero on violations)");
    println!("    infigraph review             AI-powered PR review");
    println!("    infigraph vulns              OSV vulnerability scanning");
    println!();
    println!("  Code Navigation");
    println!("    infigraph impact <symbol>    Blast radius of a change");
    println!("    infigraph routes             Detect HTTP/gRPC endpoints");
    println!("    infigraph cluster            Detect functional modules");
    println!("    infigraph architecture       Codebase overview");
    println!("    infigraph refs <symbol>      Find all references");
    println!();
    println!("  Visualization");
    println!("    infigraph visualize          Interactive graph in browser");
    println!("    infigraph viz-sym <symbol>   Focused subgraph for one symbol");
    println!();
    println!("  Multi-Repo");
    println!("    infigraph group create <name>  Create a service group");
    println!("    infigraph group link           Link cross-service calls");
    println!();
    println!("  Get started:");
    println!("    cd your-project && infigraph init");
    println!();
}

pub(crate) fn update_cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".infigraph").join("update_check.json"))
}

pub(crate) fn fetch_latest_version() -> Option<String> {
    let gh_host = std::env::var("INFIGRAPH_GH_HOST").unwrap_or_else(|_| "github.com".to_string());
    let gh_owner = std::env::var("INFIGRAPH_GH_OWNER").unwrap_or_else(|_| "intuit".to_string());
    let gh_repo = "infigraph";

    let mut args = vec!["api"];
    let api_path = format!("repos/{gh_owner}/{gh_repo}/releases/latest");
    if gh_host != "github.com" {
        args.extend(["--hostname", &gh_host]);
    }
    args.push(&api_path);
    args.extend(["--jq", ".tag_name"]);

    let output = std::process::Command::new("gh").args(&args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

pub(crate) fn version_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(latest) > parse(current)
}

pub(crate) fn check_for_update_background() -> Option<std::thread::JoinHandle<()>> {
    let cache_path = update_cache_path()?;

    if let Ok(content) = std::fs::read_to_string(&cache_path) {
        if let Ok(cached) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(ts) = cached.get("checked_at").and_then(|v| v.as_i64()) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if now - ts < 86400 {
                    return None;
                }
            }
        }
    }

    Some(std::thread::spawn(move || {
        let Some(latest) = fetch_latest_version() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let cache = serde_json::json!({
            "latest_version": latest,
            "current_version": env!("CARGO_PKG_VERSION"),
            "checked_at": now,
        });
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&cache).unwrap_or_default(),
        );
    }))
}

pub(crate) fn print_update_hint(handle: Option<std::thread::JoinHandle<()>>) {
    if let Some(h) = handle {
        let _ = h.join();
    }
    let Some(cache_path) = update_cache_path() else {
        return;
    };
    let content = match std::fs::read_to_string(&cache_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let cached: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };
    let latest = cached
        .get("latest_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let current = env!("CARGO_PKG_VERSION");
    if version_newer(latest, current) {
        eprintln!(
            "\n  infigraph v{latest} available (current: v{current}). Run `infigraph update` to upgrade.\n"
        );
    }
}

fn reinstall_hooks() -> Result<()> {
    let home = dirs::home_dir().context("cannot find home directory")?;
    println!("\nReinstalling hooks...");
    crate::hooks::install_enforcement_hook(&home)?;
    crate::hooks::install_edit_tracker_hook(&home)?;
    crate::hooks::install_session_save_hook(&home)?;
    crate::hooks::install_clear_suggest_hook(&home)?;
    crate::hooks::install_clear_guard_hook(&home)?;
    crate::hooks::install_session_end_hook(&home)?;
    crate::hooks::install_test_context_sentinel_hook(&home)?;
    crate::hooks::install_search_fallback_sentinel_hook(&home)?;
    crate::hooks::install_claude_allowlist(&home)?;
    Ok(())
}

pub(crate) fn cmd_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    // Try direct binary download via gh release if a new version is available
    if let Some(latest) = fetch_latest_version() {
        if version_newer(&latest, current) {
            println!("Updating infigraph: v{current} → v{latest}");
            match self_update(&latest) {
                Ok(()) => {
                    reinstall_hooks()?;
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("Binary update failed ({e}), falling back to install script...");
                }
            }
        } else {
            println!("Already at latest version v{current}.");
            return Ok(());
        }
    }

    // Fallback: install script
    println!("Downloading latest install script and running it.\n");

    let gh_host = std::env::var("INFIGRAPH_GH_HOST").unwrap_or_else(|_| "github.com".to_string());
    let gh_owner = std::env::var("INFIGRAPH_GH_OWNER").unwrap_or_else(|_| "intuit".to_string());
    let gh_repo = "infigraph";

    let is_ghe = gh_host != "github.com";
    let script_url = if is_ghe {
        format!(
            "https://{}/api/v3/repos/{}/{}/contents/install.sh",
            gh_host, gh_owner, gh_repo
        )
    } else {
        format!(
            "https://raw.githubusercontent.com/{}/{}/main/install.sh",
            gh_owner, gh_repo
        )
    };

    let cmd = if is_ghe {
        format!(
            "gh api -H 'Accept: application/vnd.github.raw' --hostname {} '{}' | bash",
            gh_host, script_url
        )
    } else {
        format!("curl -fsSL '{}' | bash", script_url)
    };

    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .status()
        .context("failed to run install script — is `gh` or `curl` installed?")?;

    if !status.success() {
        anyhow::bail!("update failed (exit code {:?})", status.code());
    }

    Ok(())
}
