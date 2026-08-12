use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(crate) struct InstallReport {
    pub written: Vec<String>,
    pub skipped: Vec<(String, String)>, // (path, reason)
}

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
    println!("Found infigraph-mcp at: {}", mcp_path.to_string_lossy());

    let home = dirs::home_dir().context("Could not determine home directory")?;

    let report = run_install(&mcp_path, &home)?;

    if report.written.is_empty() {
        println!("No agents were configured.");
    } else {
        for path in &report.written {
            println!("  Configured: {}", home.join(path).display());
        }
        let configured_labels = configured_integration_labels(&mcp_path, &home, &report)?;
        print_capabilities_summary(&configured_labels);
    }

    for (path, reason) in &report.skipped {
        eprintln!("  Skipped {}: {}", home.join(path).display(), reason);
    }

    // Copy model files to ~/.infigraph/models/ -- unchanged, not artifact-based.
    install_models(&mcp_path, &home)?;

    Ok(())
}

/// The actual artifact-engine install logic, factored out from `cmd_install`
/// so it's testable against a fake `$HOME` without touching the real one.
pub(crate) fn run_install(mcp_path: &Path, home: &Path) -> Result<InstallReport> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");

    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let mut report = InstallReport {
        written: Vec::new(),
        skipped: Vec::new(),
    };

    for artifact in &artifacts {
        let outcome = crate::artifacts::apply_resolved_artifact(artifact, home, &mcp_path_str)
            .with_context(|| {
                format!(
                    "applying {} artifact for {}",
                    artifact.integration_label,
                    artifact
                        .target_relative_path
                        .as_deref()
                        .unwrap_or("(resolver-determined path)")
                )
            })?;
        let label = artifact
            .target_relative_path
            .clone()
            .unwrap_or_else(|| format!("{} (resolver-determined)", artifact.integration_label));
        match outcome {
            crate::artifacts::ApplyOutcome::Written => report.written.push(label),
            crate::artifacts::ApplyOutcome::Skipped { reason, .. } => {
                report.skipped.push((label, reason))
            }
        }
    }

    write_claude_allowlist_and_hooks_extras(home)?;

    Ok(report)
}

/// Everything the artifact engine doesn't cover: the Claude Code permission
/// allowlist (a grant list, not "content deployed to a path" -- see the
/// design spec's "stays outside the artifact mechanism").
fn write_claude_allowlist_and_hooks_extras(home: &Path) -> Result<()> {
    crate::hooks::install_claude_allowlist(home)?;
    Ok(())
}

/// Derives the human-readable "Configured for: X, Y, Z" summary from which
/// integrations actually had at least one artifact written -- replaces the
/// old per-`AgentTarget` `configured.push(target.label)` bookkeeping now that
/// artifacts (not agent targets) are the unit of installation.
fn configured_integration_labels(
    mcp_path: &Path,
    home: &Path,
    report: &InstallReport,
) -> Result<Vec<String>> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");
    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let written_set: std::collections::HashSet<&str> =
        report.written.iter().map(|s| s.as_str()).collect();

    let mut labels: Vec<String> = artifacts
        .iter()
        .filter(|a| {
            let key = a
                .target_relative_path
                .as_deref()
                .map(|p| p.to_string())
                .unwrap_or_else(|| format!("{} (resolver-determined)", a.integration_label));
            written_set.contains(key.as_str())
        })
        .map(|a| a.integration_label.clone())
        .collect();
    labels.sort();
    labels.dedup();
    Ok(labels)
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
    let mcp_path = find_mcp_binary().unwrap_or_else(|_| PathBuf::from("infigraph-mcp"));

    let removed = run_uninstall(&mcp_path, &home)?;

    if removed.is_empty() {
        println!("No agents had infigraph configured.");
    } else {
        println!(
            "\nUninstalled infigraph MCP server artifacts: {}",
            removed.join(", ")
        );
    }

    // Remove hooks and Claude Code allowlist -- outside the artifact mechanism.
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

    clean_infigraph_cache_dir(&home)?;

    Ok(())
}

/// The actual artifact-engine uninstall logic, factored out from
/// `cmd_uninstall` so it's testable against a fake `$HOME`. Returns the list
/// of artifact labels that were actually removed (mirrors `run_install`'s
/// `InstallReport.written` shape, one label per artifact whose `remove_*`
/// call reported `true`).
pub(crate) fn run_uninstall(mcp_path: &Path, home: &Path) -> Result<Vec<String>> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");

    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let mut removed = Vec::new();
    for artifact in &artifacts {
        let was_removed = crate::artifacts::remove_resolved_artifact(artifact, home, &mcp_path_str)
            .with_context(|| format!("removing {} artifact", artifact.integration_label))?;
        if was_removed {
            removed.push(artifact.integration_label.clone());
        }
    }
    removed.sort();
    removed.dedup();
    Ok(removed)
}

/// Removes disposable caches under `~/.infigraph/` (the semantic-search
/// model files, the update-check cache) while deliberately preserving
/// `~/.infigraph/integrations/` -- the persistent, user-authored override
/// directory the two-tier discovery system depends on (see Task 9). Before
/// this PR, `~/.infigraph/` only ever held disposable caches, so wiping the
/// whole directory on uninstall was safe; now that it also holds real user
/// customization work, that would silently destroy it. Extracted as its own
/// function (rather than inlined in `cmd_uninstall`) specifically so it's
/// testable against a fake home directory -- `cmd_uninstall` itself calls
/// `dirs::home_dir()` directly and can't be pointed at a temp dir in a test.
fn clean_infigraph_cache_dir(home: &Path) -> Result<()> {
    let models_cache = home.join(".infigraph").join("models");
    if models_cache.exists() {
        std::fs::remove_dir_all(&models_cache)?;
        println!("  Removed model cache: {}", models_cache.display());
    }
    let update_check_cache = home.join(".infigraph").join("update_check.json");
    if update_check_cache.exists() {
        let _ = std::fs::remove_file(&update_check_cache);
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

pub(crate) fn print_capabilities_summary(configured: &[String]) {
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
    let mcp_path = find_mcp_binary()?;
    run_install(&mcp_path, &home)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_install_writes_every_convention_based_integration_under_a_fake_home() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let report = run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        assert!(report.written.iter().any(|p| p == ".claude.json"));
        assert!(report.written.iter().any(|p| p == ".gemini/settings.json"));
        assert!(report.written.iter().any(|p| p == ".codex/config.toml"));
        assert!(
            report.skipped.is_empty(),
            "nothing should be skipped against an empty $HOME"
        );

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(claude_json["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn cmd_install_is_idempotent() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();
        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            claude_json["mcpServers"]["infigraph"]["args"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn cmd_uninstall_removes_everything_cmd_install_wrote() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();
        assert!(home_dir.path().join(".claude.json").exists());

        run_uninstall(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(claude_json["mcpServers"]["infigraph"].is_null());
        assert!(!home_dir
            .path()
            .join(".claude/hooks/infigraph-enforce.sh")
            .exists());

        let codex_toml =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert!(!codex_toml.contains("[mcp_servers.infigraph]"));
    }

    #[test]
    fn clean_infigraph_cache_dir_preserves_user_integration_overrides() {
        let home_dir = tempfile::tempdir().unwrap();
        let integrations_dir = home_dir.path().join(".infigraph").join("integrations");
        std::fs::create_dir_all(&integrations_dir).unwrap();
        std::fs::write(
            integrations_dir.join("my-custom-agent.json"),
            "a user's own override, not a cache",
        )
        .unwrap();

        let models_dir = home_dir
            .path()
            .join(".infigraph")
            .join("models")
            .join("potion-base-8M");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("model.safetensors"), "fake model data").unwrap();

        clean_infigraph_cache_dir(home_dir.path()).unwrap();

        assert!(
            integrations_dir.join("my-custom-agent.json").exists(),
            "user-authored integration overrides must survive uninstall's cache cleanup"
        );
        assert!(
            !models_dir.exists(),
            "the model cache should still be removed"
        );
    }
}
