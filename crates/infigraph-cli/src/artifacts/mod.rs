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

    #[test]
    fn bundled_claude_json_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let claude_json = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude.json"))
            .expect("claude-code's .claude.json fragment should be discovered");
        assert_eq!(claude_json.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(claude_json, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_claude_md_and_reindex_skill_apply_via_manifest() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let claude_md = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude/CLAUDE.md"))
            .expect(
                "CLAUDE.md marker_delimited artifact should be discovered from claude-code/config.toml",
            );
        assert_eq!(claude_md.strategy, Strategy::MarkerDelimited);
        apply_resolved_artifact(claude_md, home_dir.path(), mcp_path).unwrap();
        let claude_md_content =
            std::fs::read_to_string(home_dir.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(claude_md_content.contains("## Infigraph — Primary Code Intelligence"));
        assert!(claude_md_content.contains("<!-- infigraph-primary-search -->"));

        let skill = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref()
                    == Some(".claude/skills/infigraph-reindex/SKILL.md")
            })
            .expect("reindex skill artifact should be discovered from claude-code/config.toml");
        assert_eq!(skill.strategy, Strategy::Overwrite);
        apply_resolved_artifact(skill, home_dir.path(), mcp_path).unwrap();
        let skill_content = std::fs::read_to_string(
            home_dir
                .path()
                .join(".claude/skills/infigraph-reindex/SKILL.md"),
        )
        .unwrap();
        assert!(skill_content.starts_with("---\nname: infigraph-reindex"));
        assert!(skill_content.contains("mcp__infigraph__index_project"));
    }

    #[test]
    fn bundled_settings_json_multi_event_test() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let settings = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude/settings.json"))
            .expect("settings.json fragment should be discovered");
        assert_eq!(settings.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(settings, home_dir.path(), mcp_path).unwrap();
        apply_resolved_artifact(settings, home_dir.path(), mcp_path).unwrap(); // reapply: must not duplicate

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        for (event, expected_count) in [
            ("PreToolUse", 1),
            ("PostToolUse", 4),
            ("UserPromptSubmit", 3),
            ("SessionStart", 1),
            ("SessionEnd", 1),
            ("PreCompact", 1),
        ] {
            let arr = written["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("{event} should be an array"));
            assert_eq!(
                arr.len(),
                expected_count,
                "{event} should have exactly {expected_count} entries after reapply, got {arr:?}"
            );
        }

        let enforce_command = written["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(enforce_command.contains("infigraph-enforce.sh"));
    }

    #[test]
    fn bundled_enforce_hook_allows_piped_grep_but_still_blocks_bare_grep() {
        // Regression test for the grep-piping detection bug caught live this
        // session: a real Bash command filtering another command's output
        // (`cmd 2>&1 | grep -iE "error"`, explicitly allowed by this repo's
        // own CLAUDE.md guidance) must NOT be blocked, while a bare/leading
        // grep call (a real code search) must still be blocked. This checks
        // the bundled content carries the fix's `cmd_without_piped_grep`
        // logic rather than the old blanket substring check.
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let enforce = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref() == Some(".claude/hooks/infigraph-enforce.sh")
            })
            .unwrap();
        apply_resolved_artifact(enforce, home_dir.path(), mcp_path).unwrap();
        let content =
            std::fs::read_to_string(home_dir.path().join(".claude/hooks/infigraph-enforce.sh"))
                .unwrap();
        assert!(
            content.contains("cmd_without_piped_grep"),
            "enforce.sh must carry the piped-grep exemption, not the old blanket check"
        );
        // Check the actual sed command line, not the whole file -- the fix's
        // own explanatory comment legitimately mentions "\b" as prose (why it
        // was avoided), so a whole-file substring check would false-positive
        // on that comment instead of testing the real regex.
        let sed_line = content
            .lines()
            .find(|line| line.contains("sed -E"))
            .expect("enforce.sh should contain the piped-grep sed substitution");
        assert!(
            !sed_line.contains("\\b"),
            "the sed substitution must not use \\b -- unsupported by macOS's BSD sed, \
             verified live during this fix (silently no-ops instead of erroring)"
        );
    }

    #[test]
    fn bundled_hook_scripts_have_expected_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        for (relative, expected_substring) in [
            (".claude/hooks/infigraph-enforce.sh", "deny-by-default"),
            (
                ".claude/hooks/infigraph-edit-tracker.sh",
                "recent_edits.log",
            ),
            (".claude/hooks/infigraph-session-save.sh", "save_session"),
            (".claude/hooks/infigraph-session-reset.sh", "save_session"),
            (
                ".claude/hooks/infigraph-session-start.sh",
                "inject_session_summary",
            ),
            (
                ".claude/hooks/infigraph-session-end-save.sh",
                "unsaved-transcript",
            ),
            (
                ".claude/hooks/infigraph-clear-suggest.sh",
                "save session and type",
            ),
            (
                ".claude/hooks/infigraph-clear-guard.sh",
                "Session not saved",
            ),
            (
                ".claude/hooks/infigraph-test-context-sentinel.sh",
                "generate_test_context",
            ),
            (
                ".claude/hooks/infigraph-search-fallback-sentinel.sh",
                "search-fallback-allowed",
            ),
        ] {
            let artifact = artifacts
                .iter()
                .find(|a| a.target_relative_path.as_deref() == Some(relative))
                .unwrap_or_else(|| panic!("{relative} should be discovered"));
            assert_eq!(artifact.strategy, Strategy::Overwrite);
            apply_resolved_artifact(artifact, home_dir.path(), mcp_path).unwrap();
            let content = std::fs::read_to_string(home_dir.path().join(relative)).unwrap();
            assert!(
                content.contains(expected_substring),
                "{relative} should contain {expected_substring:?}"
            );
        }
    }

    #[test]
    fn bundled_cursor_rules_and_mcp_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/rules/infigraph.mdc"))
            .expect("cursor rules artifact should be discovered from cursor/config.toml");
        assert_eq!(rules.strategy, Strategy::Overwrite);
        apply_resolved_artifact(rules, home_dir.path(), mcp_path).unwrap();
        let rules_content =
            std::fs::read_to_string(home_dir.path().join(".cursor/rules/infigraph.mdc")).unwrap();
        assert!(rules_content.contains("## Infigraph — Primary Code Intelligence"));

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/mcp.json"))
            .expect("cursor mcp.json fragment should be discovered");
        assert_eq!(mcp.strategy, Strategy::JsonDeepMerge);
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let mcp_written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".cursor/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mcp_written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_windsurf_rules_and_mcp_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".windsurf/rules/infigraph.md"))
            .expect("windsurf rules artifact should be discovered from windsurf/config.toml");
        assert_eq!(rules.strategy, Strategy::Overwrite);
        apply_resolved_artifact(rules, home_dir.path(), mcp_path).unwrap();
        let rules_content =
            std::fs::read_to_string(home_dir.path().join(".windsurf/rules/infigraph.md")).unwrap();
        assert!(rules_content.contains("## Infigraph — Primary Code Intelligence"));

        let mcp = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref() == Some(".codeium/windsurf/mcp_config.json")
            })
            .expect("windsurf mcp_config.json fragment should be discovered");
        assert_eq!(mcp.strategy, Strategy::JsonDeepMerge);
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let mcp_written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".codeium/windsurf/mcp_config.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mcp_written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn shared_agents_md_override_changes_both_cursor_and_windsurf_output() {
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user_dir.path().join("shared")).unwrap();
        std::fs::write(
            user_dir.path().join("shared/agents.md"),
            "## Overridden instructions",
        )
        .unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let cursor_rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/rules/infigraph.mdc"))
            .unwrap();
        assert_eq!(
            String::from_utf8(cursor_rules.content.clone().unwrap()).unwrap(),
            "## Overridden instructions"
        );

        let windsurf_rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".windsurf/rules/infigraph.md"))
            .unwrap();
        assert_eq!(
            String::from_utf8(windsurf_rules.content.clone().unwrap()).unwrap(),
            "## Overridden instructions"
        );
    }

    #[test]
    fn bundled_codex_mcp_and_skill_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".codex/config.toml"))
            .expect("codex toml_section MCP artifact should be discovered");
        assert_eq!(mcp.strategy, Strategy::TomlSection);
        assert_eq!(
            mcp.key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let toml_content =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert!(toml_content.contains("[mcp_servers.infigraph]"));
        assert!(toml_content.contains(mcp_path));

        let skill = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref()
                    == Some(".codex/skills/infigraph-reindex/SKILL.md")
            })
            .expect("codex reindex skill artifact should be discovered");
        assert_eq!(skill.strategy, Strategy::Overwrite);
        apply_resolved_artifact(skill, home_dir.path(), mcp_path).unwrap();
        let skill_content = std::fs::read_to_string(
            home_dir
                .path()
                .join(".codex/skills/infigraph-reindex/SKILL.md"),
        )
        .unwrap();
        assert!(skill_content.starts_with("---\nname: infigraph-reindex"));
    }

    #[test]
    fn bundled_codex_toml_reapply_does_not_duplicate_section() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".codex/config.toml"))
            .unwrap();

        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();

        let toml_content =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert_eq!(toml_content.matches("[mcp_servers.infigraph]").count(), 1);
    }

    #[test]
    fn bundled_vscode_resolver_resolves_path_and_uses_local_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let vscode = artifacts
            .iter()
            .find(|a| a.integration_label == "VS Code")
            .expect("VS Code resolver artifact should be discovered");
        assert_eq!(vscode.strategy, Strategy::JsonDeepMerge);
        assert!(vscode.target_relative_path.is_none());
        assert!(vscode.resolver.is_some());

        let outcome = apply_resolved_artifact(vscode, home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        // The resolver branches on OS -- assert against whichever path it
        // actually resolved to for the OS running this test.
        let expected_suffix = match std::env::consts::OS {
            "macos" => "Library/Application Support/Code/User/mcp.json",
            "linux" => ".config/Code/User/mcp.json",
            "windows" => "AppData/Roaming/Code/User/mcp.json",
            other => panic!("unhandled test OS {other}"),
        };
        let path = home_dir.path().join(expected_suffix);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["servers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_zed_resolver_resolves_path_and_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts =
            discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let zed = artifacts
            .iter()
            .find(|a| a.integration_label == "Zed")
            .expect("Zed resolver artifact should be discovered");
        assert_eq!(zed.strategy, Strategy::JsonKeyPath);
        assert_eq!(
            zed.key_path,
            Some(vec!["context_servers".to_string(), "infigraph".to_string()])
        );
        assert!(zed.target_relative_path.is_none());

        let outcome = apply_resolved_artifact(zed, home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        let expected_suffix = match std::env::consts::OS {
            "macos" => "Library/Application Support/Zed/settings.json",
            "linux" => ".config/zed/settings.json",
            "windows" => "AppData/Roaming/Zed/settings.json",
            other => panic!("unhandled test OS {other}"),
        };
        let path = home_dir.path().join(expected_suffix);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["context_servers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["context_servers"]["infigraph"]["args"][0], "--mcp");
    }
}
