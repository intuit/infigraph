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
) -> anyhow::Result<ApplyOutcome> {
    let target_path = home.join(&artifact.target_relative_path);
    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            strategy::apply_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("overwrite artifact has no content"))?;
            strategy::apply_overwrite(&target_path, content)
        }
        Strategy::MarkerDelimited => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
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
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::apply_toml_section(&target_path, key_path, text)
        }
        Strategy::JsonKeyPath => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
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
) -> anyhow::Result<bool> {
    let target_path = home.join(&artifact.target_relative_path);
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
            let outcome = apply_resolved_artifact(artifact, home).unwrap();
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
            apply_resolved_artifact(artifact, home).unwrap();
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
            let removed = remove_resolved_artifact(artifact, home).unwrap();
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
            apply_resolved_artifact(artifact, home_dir.path()).unwrap();
        }

        let content =
            std::fs::read_to_string(home_dir.path().join(".claude/hooks/infigraph-enforce.sh"))
                .unwrap();
        assert!(content.contains("echo overridden"));
    }
}
