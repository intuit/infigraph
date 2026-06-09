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
        }

        configured.push(target.label);
        println!("  Configured {} ({})", target.label, config_path.display());
    }

    if configured.is_empty() {
        println!("No agents were configured.");
    } else {
        println!(
            "\nInstalled infigraph MCP server for {} agent(s): {}",
            configured.len(),
            configured.join(", ")
        );
    }

    // Write primary search instructions to ~/.claude/CLAUDE.md
    write_claude_md_instructions(&home)?;

    // Write cursor/windsurf rules
    write_editor_rules(&home)?;

    // Write /infigraph-reindex command to ~/.claude/commands/
    write_reindex_command(&home)?;

    // Install hooks
    crate::hooks::install_enforcement_hook(&home)?;
    crate::hooks::install_session_save_hook(&home)?;

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

Reindex the current project in a subagent to avoid polluting main context with index output.

## Usage

```
/infigraph-reindex [path]
```

If `path` is omitted, uses the current working directory.

## Agent Instructions

You are a Infigraph reindex subagent. Your only job is to reindex the project and report results.

1. Determine project path: use the argument provided, or fall back to the current working directory.
2. Call `mcp__infigraph__index_project` with that path.
3. Report back in this exact format (nothing else):

```
Reindexed: <path>
Files: <N> | Symbols: <N> | Calls: <N> resolved / <N> unresolved
Languages: <comma-separated list with file counts>
```

If indexing fails, report the error verbatim. Do not attempt fixes.
"#;
    if !reindex_cmd.exists() {
        std::fs::write(&reindex_cmd, reindex_content)?;
        println!(
            "  Added /infigraph-reindex command to {}",
            reindex_cmd.display()
        );
    } else {
        println!(
            "  /infigraph-reindex command already exists at {}",
            reindex_cmd.display()
        );
    }
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
            ConfigFormat::Json => config_targets::uninstall_json_target(&config_path, target.label)?,
            ConfigFormat::Toml => config_targets::uninstall_toml_target(&config_path, target.label)?,
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

    // Remove hooks
    crate::hooks::uninstall_hooks(&home)?;

    // Remove binaries from ~/.local/bin/
    for bin in &["infigraph", "infigraph-mcp"] {
        let bin_path = home.join(".local").join("bin").join(bin);
        if bin_path.exists() {
            std::fs::remove_file(&bin_path)?;
            println!("  Removed binary: {}", bin_path.display());
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

pub(crate) fn cmd_update() -> Result<()> {
    println!("Updating infigraph...");
    println!("Downloading latest install script and running it.");
    println!("This will fetch the latest binary and re-register MCP configs.\n");

    let gh_host =
        std::env::var("INFIGRAPH_GH_HOST").unwrap_or_else(|_| "github.com".to_string());
    let gh_owner =
        std::env::var("INFIGRAPH_GH_OWNER").unwrap_or_else(|_| "intuit".to_string());
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
