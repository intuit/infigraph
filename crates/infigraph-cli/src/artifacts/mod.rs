//! Data-driven integration artifacts: bundled config fragments, hook scripts,
//! and docs for every supported agent/editor, applied via one of five
//! strategies. See docs/superpowers/specs/2026-08-09-agent-target-templates-design.md.
//!
//! Built bottom-up (Tasks 1-12 of the implementation plan): most items here
//! are only exercised by this module's own unit tests until discovery
//! (Task 9) and dispatch (Task 11) wire them together, and until
//! `cmd_install`/`cmd_uninstall` (Tasks 18-19) call into the module from
//! `install.rs`. Suppressed here rather than per-item; remove once real
//! non-test callers make it unnecessary.
#![allow(dead_code, unused_imports)]

use anyhow::Context;

mod convention;
mod discovery;
mod manifest;
mod resolver;
mod step;
mod strategy;
mod template;

pub(crate) use discovery::{discover_artifacts, ResolvedArtifact};
pub(crate) use step::InstallStep;
pub(crate) use strategy::{ApplyOutcome, Strategy};

include!(concat!(env!("OUT_DIR"), "/bundled_integrations.rs"));

pub(crate) fn apply_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
    mcp_path: &str,
) -> anyhow::Result<ApplyOutcome> {
    let (target_path, resolved_content) = match &artifact.resolver {
        Some(spec) => {
            let output = resolver::run_resolver_from_script(
                &spec.script_bytes,
                &spec.script_filename,
                &spec.extra_args,
                mcp_path,
                home,
            )
            .with_context(|| format!("running resolver {}", spec.script_relative_path))?;
            match output {
                resolver::ResolverOutput::Ok { data } => {
                    let content = match data.content {
                        Some(v) => Some(serde_json::to_vec(&v)?),
                        None => artifact.content.clone(),
                    };
                    (std::path::PathBuf::from(data.path), content)
                }
                resolver::ResolverOutput::Skip { message } => {
                    return Ok(ApplyOutcome::Skipped {
                        reason: format!("resolver reported skip: {message}"),
                        manual_snippet: String::new(),
                    });
                }
                resolver::ResolverOutput::Error { message } => {
                    anyhow::bail!("resolver {} failed: {message}", spec.script_relative_path);
                }
            }
        }
        None => {
            let relative = artifact.target_relative_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("artifact has neither a static path nor a resolver")
            })?;
            (home.join(relative), artifact.content.clone())
        }
    };

    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            strategy::apply_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("overwrite artifact has no content"))?;
            strategy::apply_overwrite(&target_path, &content)
        }
        Strategy::MarkerDelimited => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::apply_marker_delimited(&target_path, start, end, text)
        }
        Strategy::TomlSection => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::apply_toml_section(&target_path, key_path, text)
        }
        Strategy::JsonKeyPath => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::apply_json_key_path(&target_path, key_path, text)
        }
    }
}

pub(crate) fn remove_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
    mcp_path: &str,
) -> anyhow::Result<bool> {
    let target_path = match &artifact.resolver {
        Some(spec) => {
            let output = resolver::run_resolver_from_script(
                &spec.script_bytes,
                &spec.script_filename,
                &spec.extra_args,
                mcp_path,
                home,
            )
            .with_context(|| format!("running resolver {}", spec.script_relative_path))?;
            match output {
                resolver::ResolverOutput::Ok { data } => std::path::PathBuf::from(data.path),
                resolver::ResolverOutput::Skip { .. } => return Ok(false),
                resolver::ResolverOutput::Error { message } => {
                    anyhow::bail!("resolver {} failed: {message}", spec.script_relative_path);
                }
            }
        }
        None => {
            let relative = artifact.target_relative_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("artifact has neither a static path nor a resolver")
            })?;
            home.join(relative)
        }
    };

    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            strategy::remove_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => strategy::remove_overwrite(&target_path),
        Strategy::MarkerDelimited => {
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::remove_marker_delimited(&target_path, start, end)
        }
        Strategy::TomlSection => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::remove_toml_section(&target_path, key_path)
        }
        Strategy::JsonKeyPath => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::remove_json_key_path(&target_path, key_path)
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn full_discover_apply_verify_uninstall_verify_removed_cycle() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "claude-code/.claude.json",
                br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#,
            ),
            (
                "claude-code/.claude/hooks/infigraph-enforce.sh",
                b"#!/usr/bin/env bash\necho enforce\n",
            ),
            (
                "claude-code/config.toml",
                br#"label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
"#,
            ),
            (
                "shared/agents.md",
                b"## Infigraph instructions\nUse infigraph tools first.",
            ),
            (
                "codex/config.toml",
                br#"label = "Codex"

[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"
"#,
            ),
            (
                "codex/mcp-section.toml",
                b"command = \"{{mcp_path}}\"\nargs = [\"--mcp\"]",
            ),
        ];

        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        assert_eq!(artifacts.len(), 4);

        for artifact in &artifacts {
            let outcome = apply_resolved_artifact(artifact, home, mcp_path).unwrap();
            assert!(
                matches!(outcome, ApplyOutcome::Written),
                "{:?} failed to apply",
                artifact.target_relative_path
            );
        }

        // Verify each landed at the expected real path with expected content --
        // .claude.json at $HOME directly (no .claude/ nesting, matching the
        // CLAUDE_CODE_SPECIAL destination it replaces), hooks nested under
        // .claude/hooks/ (stripped from the bundled claude-code/.claude/hooks/ tree).
        let claude_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(claude_json["mcpServers"]["infigraph"]["command"], mcp_path);

        let hook =
            std::fs::read_to_string(home.join(".claude/hooks/infigraph-enforce.sh")).unwrap();
        assert!(hook.contains("echo enforce"));

        let claude_md = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Use infigraph tools first."));

        let codex_toml = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(codex_toml.contains(mcp_path));
        assert!(codex_toml.contains("[mcp_servers.infigraph]"));

        // Reapply is idempotent (no duplication) -- exercises the whole
        // pipeline's self-healing property, not just one strategy in isolation.
        for artifact in &artifacts {
            apply_resolved_artifact(artifact, home, mcp_path).unwrap();
        }
        let claude_json_again: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(
            claude_json_again["mcpServers"]["infigraph"]["args"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Uninstall every artifact, verify each is actually gone.
        for artifact in &artifacts {
            let removed = remove_resolved_artifact(artifact, home, mcp_path).unwrap();
            assert!(
                removed,
                "{:?} was not removed",
                artifact.target_relative_path
            );
        }

        let claude_json_after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert!(claude_json_after["mcpServers"]["infigraph"].is_null());
        assert!(!home.join(".claude/hooks/infigraph-enforce.sh").exists());
        assert!(!std::fs::read_to_string(home.join(".claude/CLAUDE.md"))
            .unwrap()
            .contains("Use infigraph tools first."));
        assert!(!std::fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("[mcp_servers.infigraph]"));
    }

    #[test]
    fn user_override_end_to_end_replaces_bundled_content() {
        let bundled: &[(&str, &[u8])] = &[(
            "claude-code/.claude/hooks/infigraph-enforce.sh",
            b"#!/usr/bin/env bash\necho bundled\n",
        )];
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user_dir.path().join("claude-code/.claude/hooks")).unwrap();
        std::fs::write(
            user_dir
                .path()
                .join("claude-code/.claude/hooks/infigraph-enforce.sh"),
            "#!/usr/bin/env bash\necho overridden\n",
        )
        .unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        for artifact in &artifacts {
            apply_resolved_artifact(artifact, home_dir.path(), mcp_path).unwrap();
        }

        let content =
            std::fs::read_to_string(home_dir.path().join(".claude/hooks/infigraph-enforce.sh"))
                .unwrap();
        assert!(content.contains("echo overridden"));
    }

    #[test]
    fn resolver_driven_artifact_applies_to_resolver_computed_path() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "zed/config.toml",
                br#"label = "Zed"

[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#,
            ),
            (
                "zed/resolve-zed-path.sh",
                // Real resolvers (VS Code, Zed) compute a fully absolute
                // destination path themselves -- apply_resolved_artifact uses
                // `data.path` directly with no `home`-joining of its own.
                // This fixture mirrors that by reading "home" out of the
                // ResolverInput JSON on stdin and building an absolute path,
                // instead of returning a bare relative filename.
                b"#!/usr/bin/env bash\npython3 -c \"\nimport json, sys\nd = json.load(sys.stdin)\nout = {'status': 'ok', 'data': {'path': d['home'] + '/zed-settings.json', 'content': {'command': d['mcp_path'], 'args': ['--mcp'], 'env': {}}}}\nprint(json.dumps(out))\n\"\n",
            ),
        ];
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        assert_eq!(artifacts.len(), 1);

        let outcome = apply_resolved_artifact(&artifacts[0], home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join("zed-settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["context_servers"]["infigraph"]["command"], mcp_path);

        let removed = remove_resolved_artifact(&artifacts[0], home_dir.path(), mcp_path).unwrap();
        assert!(removed);
        let after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join("zed-settings.json")).unwrap(),
        )
        .unwrap();
        assert!(after["context_servers"]["infigraph"].is_null());
    }

    #[test]
    fn bundled_gemini_cli_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let gemini = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".gemini/settings.json"))
            .expect("gemini-cli fragment should be discovered from the bundled registry");
        assert_eq!(gemini.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(gemini, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_opencode_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let opencode = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".config/opencode/opencode.json"))
            .expect("opencode fragment should be discovered from the bundled registry");
        assert_eq!(opencode.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(opencode, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".config/opencode/opencode.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcp"]["infigraph"]["type"], "local");
        assert_eq!(written["mcp"]["infigraph"]["command"][0], mcp_path);
        assert_eq!(written["mcp"]["infigraph"]["command"][1], "--mcp");
    }

    #[test]
    fn bundled_aider_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let aider = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".aider/mcp.json"))
            .expect("aider fragment should be discovered from the bundled registry");
        assert_eq!(aider.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(aider, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".aider/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_kiro_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let kiro = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".kiro/settings/mcp.json"))
            .expect("kiro fragment should be discovered from the bundled registry");
        assert_eq!(kiro.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(kiro, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".kiro/settings/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_github_copilot_cli_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let copilot = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".copilot/mcp-config.json"))
            .expect("github-copilot-cli fragment should be discovered from the bundled registry");
        assert_eq!(copilot.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(copilot, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".copilot/mcp-config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }
}
