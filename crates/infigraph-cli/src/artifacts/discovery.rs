use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::convention::{infer_strategy, ConventionStrategy};
use super::manifest::parse_manifest;
use super::step::InstallStep;
use super::strategy::Strategy;
use super::template::{substitute_mcp_path, TemplateFormat};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolverSpec {
    /// The script's normalized path within the bundled/user-override
    /// registry, e.g. "zed/resolve-zed-path.sh" -- kept for error messages.
    pub script_relative_path: String,
    pub script_bytes: Vec<u8>,
    /// Just the filename (e.g. "resolve-zed-path.sh"), used when writing the
    /// script to a temp directory before spawning it.
    pub script_filename: String,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedArtifact {
    pub integration_label: String,
    pub target_relative_path: Option<String>,
    pub strategy: Strategy,
    /// Fully resolved (template-substituted) content, ready to hand to the
    /// matching `strategy::apply_*` function. `None` only for resolver-driven
    /// artifacts whose content is generated at apply time (Plan 2 territory);
    /// this plan's discovery always populates it when a `content_file` exists.
    pub content: Option<Vec<u8>>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub key_path: Option<Vec<String>>,
    pub resolver: Option<ResolverSpec>,
    pub step: InstallStep,
}

fn is_toml_extension(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
        == Some("toml")
}

/// True for `<integration>/config.toml` (the manifest), false for any deeper
/// nesting like `<integration>/some-subdir/config.toml` (content that merely
/// shares the filename) or a file elsewhere named differently.
fn is_manifest_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    path.file_name().and_then(|f| f.to_str()) == Some("config.toml")
        && path.components().count() == 2 // "<integration>" / "config.toml"
}

fn integration_dir_of(relative_path: &str) -> &str {
    relative_path.split('/').next().unwrap_or(relative_path)
}

/// Strips the leading `<integration_dir>/` segment from a bundled/user-override
/// relative path, leaving the path relative to `$HOME`. Every convention-based
/// file's location below its integration directory is authored to already
/// mirror the real destination structure (e.g. `claude-code/.claude/hooks/x.sh`
/// strips to `.claude/hooks/x.sh`; `claude-code/.claude.json` strips to
/// `.claude.json`, since that file's real destination has no further nesting)
/// -- so a plain prefix strip is always correct, never a special case per integration.
fn strip_integration_prefix(relative_path: &str, integration_dir: &str) -> String {
    relative_path
        .strip_prefix(integration_dir)
        .and_then(|s| s.strip_prefix('/'))
        .unwrap_or(relative_path)
        .to_string()
}

/// Merges the bundled registry with the user-override directory into one
/// `relative_path -> content bytes` map, override winning at the same path.
fn merge_bundled_and_user(
    bundled: &[(&'static str, &'static [u8])],
    user_override_dir: &Path,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files: BTreeMap<String, Vec<u8>> = bundled
        .iter()
        .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
        .collect();

    if user_override_dir.is_dir() {
        for entry in walk_files(user_override_dir)? {
            let relative = entry
                .strip_prefix(user_override_dir)
                .expect("walked under this root")
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read(&entry)
                .with_context(|| format!("failed to read {}", entry.display()))?;
            files.insert(relative, content);
        }
    }

    Ok(files)
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn template_format_for_strategy(strategy: Strategy) -> Option<TemplateFormat> {
    match strategy {
        Strategy::JsonDeepMerge | Strategy::JsonKeyPath => Some(TemplateFormat::Json),
        Strategy::TomlSection => Some(TemplateFormat::Toml),
        Strategy::Overwrite | Strategy::MarkerDelimited => None,
    }
}

fn resolve_content_file(
    files: &BTreeMap<String, Vec<u8>>,
    integration_dir: &str,
    content_file: &str,
) -> Result<Vec<u8>> {
    // content_file is relative to the integration's own directory, and may
    // escape it with "../" (e.g. "../shared/agents.md").
    let combined = format!("{integration_dir}/{content_file}");
    let normalized = normalize_relative_path(&combined);
    files.get(&normalized).cloned().with_context(|| {
        format!("content_file \"{content_file}\" (resolved to \"{normalized}\") not found")
    })
}

fn normalize_relative_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn template_format_for(relative_path: &str) -> Option<TemplateFormat> {
    match Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("json") => Some(TemplateFormat::Json),
        Some("toml") => Some(TemplateFormat::Toml),
        _ => None,
    }
}

pub(crate) fn discover_artifacts(
    bundled: &[(&'static str, &'static [u8])],
    user_override_dir: &Path,
    mcp_path: &str,
) -> Result<Vec<ResolvedArtifact>> {
    let files = merge_bundled_and_user(bundled, user_override_dir)?;
    let mut artifacts = Vec::new();
    let mut manifest_claimed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: explicit [[artifact]] entries from every <integration>/config.toml manifest.
    for (relative_path, content) in &files {
        if !is_manifest_path(relative_path) {
            continue;
        }
        let integration_dir = integration_dir_of(relative_path);
        let manifest_text = String::from_utf8(content.clone())
            .with_context(|| format!("{relative_path} is not valid UTF-8"))?;
        let manifest = parse_manifest(&manifest_text)
            .with_context(|| format!("failed to parse manifest {relative_path}"))?;
        let label = manifest
            .label
            .clone()
            .unwrap_or_else(|| integration_dir.to_string());

        for entry in &manifest.artifacts {
            let strategy = Strategy::parse(&entry.strategy)
                .with_context(|| format!("in manifest {relative_path}, artifact declaration"))?;

            anyhow::ensure!(
                entry.path.is_some() || entry.resolver.is_some(),
                "in manifest {relative_path}: artifact must declare either \"path\" or \"resolver\""
            );

            let resolver_spec = match &entry.resolver {
                Some(resolver_cmd) => {
                    anyhow::ensure!(
                        !resolver_cmd.is_empty(),
                        "in manifest {relative_path}: resolver command must not be empty"
                    );
                    let script_arg = &resolver_cmd[0];
                    let script_relative = script_arg.strip_prefix("./").unwrap_or(script_arg);
                    let combined = format!("{integration_dir}/{script_relative}");
                    let normalized = normalize_relative_path(&combined);
                    let script_bytes = files.get(&normalized).cloned().with_context(|| {
                        format!(
                            "resolver script \"{script_arg}\" (resolved to \"{normalized}\") not found"
                        )
                    })?;
                    manifest_claimed.insert(normalized.clone());
                    let script_filename = Path::new(&normalized)
                        .file_name()
                        .expect("resolver script path must have a filename")
                        .to_string_lossy()
                        .to_string();
                    Some(ResolverSpec {
                        script_relative_path: normalized,
                        script_bytes,
                        script_filename,
                        extra_args: resolver_cmd[1..].to_vec(),
                    })
                }
                None => None,
            };

            let content = match &entry.content_file {
                Some(content_file) => {
                    let raw = resolve_content_file(&files, integration_dir, content_file)?;
                    let normalized_content_file =
                        normalize_relative_path(&format!("{integration_dir}/{content_file}"));
                    manifest_claimed.insert(normalized_content_file);
                    let text = String::from_utf8(raw).with_context(|| {
                        format!("content_file \"{content_file}\" is not valid UTF-8")
                    })?;
                    // Format is derived from the artifact's *strategy*, not its
                    // (possibly absent) path -- a resolver-only artifact like
                    // VS Code's has no static path but still needs a JSON
                    // template substitution for its local content_file.
                    let substituted = match template_format_for_strategy(strategy) {
                        Some(format) => substitute_mcp_path(&text, mcp_path, format),
                        None => text,
                    };
                    Some(substituted.into_bytes())
                }
                None => None,
            };

            let step = match &entry.path {
                Some(path) => InstallStep::classify(path, strategy),
                // Both current resolver-driven artifacts (VS Code, Zed) are
                // MCP registrations; revisit if a future resolver-driven
                // artifact is ever a hook or doc/rules entry instead.
                None => InstallStep::McpRegistration,
            };

            artifacts.push(ResolvedArtifact {
                integration_label: label.clone(),
                target_relative_path: entry.path.clone(),
                strategy,
                content,
                start: entry.start.clone(),
                end: entry.end.clone(),
                key_path: entry.key_path.clone(),
                resolver: resolver_spec,
                step,
            });
        }
    }

    // Pass 2: convention-based files -- everything not a manifest, not
    // claimed as a manifest's content_file, and not directly under shared/.
    for (relative_path, content) in &files {
        if is_manifest_path(relative_path) {
            continue;
        }
        if manifest_claimed.contains(relative_path) {
            continue;
        }
        if integration_dir_of(relative_path) == "shared" {
            continue;
        }
        if is_toml_extension(relative_path) {
            // .toml is never convention-based; without a manifest entry
            // referencing it, it is simply not applied (proven by the
            // skips_bundled_toml_file_with_no_manifest_entry test above).
            continue;
        }

        let Some(convention) = infer_strategy(relative_path) else {
            continue;
        };
        let strategy = match convention {
            ConventionStrategy::JsonDeepMerge => Strategy::JsonDeepMerge,
            ConventionStrategy::Overwrite => Strategy::Overwrite,
        };

        let text_for_template = match template_format_for(relative_path) {
            Some(format) => {
                let text = String::from_utf8(content.clone())
                    .with_context(|| format!("{relative_path} is not valid UTF-8"))?;
                substitute_mcp_path(&text, mcp_path, format).into_bytes()
            }
            None => content.clone(),
        };

        let integration_dir = integration_dir_of(relative_path);
        // The bundled subtree below <integration>/ already mirrors the real
        // destination path exactly (e.g. "claude-code/.claude/hooks/x.sh" is
        // authored that way specifically so stripping the leading
        // "claude-code/" leaves ".claude/hooks/x.sh", the correct path
        // relative to $HOME) -- except ".claude.json" itself, which sits
        // at the integration root because its real destination has no
        // further nesting. Both cases are handled by the same strip.
        let target_relative_path = strip_integration_prefix(relative_path, integration_dir);
        artifacts.push(ResolvedArtifact {
            integration_label: integration_dir.to_string(),
            target_relative_path: Some(target_relative_path),
            strategy,
            content: Some(text_for_template),
            start: None,
            end: None,
            key_path: None,
            resolver: None,
            step: InstallStep::classify(relative_path, strategy),
        });
    }

    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &std::path::Path, relative: &str, content: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_convention_based_json_artifact_from_bundled() {
        let bundled: &[(&str, &[u8])] = &[(
            "claude-code/.claude.json",
            br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#,
        )];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert_eq!(
            a.target_relative_path.as_deref(),
            Some(".claude.json"),
            "the leading \"claude-code/\" integration-directory segment must be stripped -- \
             the bundled subtree already mirrors the real path below $HOME"
        );
        assert_eq!(a.strategy, Strategy::JsonDeepMerge);
        let content = String::from_utf8(a.content.clone().unwrap()).unwrap();
        assert!(
            content.contains("/bin/infigraph-mcp"),
            "mcp_path should be substituted: {content}"
        );
        assert!(!content.contains("{{mcp_path}}"));
    }

    #[test]
    fn skips_bundled_toml_file_with_no_manifest_entry() {
        // A .toml content file with no config.toml manifest claiming it is a
        // discovery bug waiting to happen (it would silently never be
        // applied) -- but per Global Constraints, .toml is never
        // convention-based, so it's correctly excluded, not silently wrong.
        let bundled: &[(&str, &[u8])] =
            &[("codex/mcp-section.toml", b"command = \"{{mcp_path}}\"")];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(
            artifacts.is_empty(),
            "a .toml file with no manifest entry referencing it must not be auto-applied"
        );
    }

    #[test]
    fn manifest_entry_produces_explicit_artifact_with_all_fields() {
        let bundled: &[(&str, &[u8])] = &[
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

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert_eq!(a.integration_label, "Codex");
        assert_eq!(
            a.target_relative_path.as_deref(),
            Some(".codex/config.toml")
        );
        assert_eq!(a.strategy, Strategy::TomlSection);
        assert_eq!(
            a.key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        let content = String::from_utf8(a.content.clone().unwrap()).unwrap();
        assert!(content.contains("/bin/infigraph-mcp"));
    }

    #[test]
    fn manifest_content_file_can_escape_to_shared_directory() {
        let bundled: &[(&str, &[u8])] = &[
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
            ("shared/agents.md", b"## Infigraph instructions"),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            String::from_utf8(artifacts[0].content.clone().unwrap()).unwrap(),
            "## Infigraph instructions"
        );
    }

    #[test]
    fn user_override_replaces_bundled_file_at_same_relative_path() {
        let bundled: &[(&str, &[u8])] = &[(
            "claude-code/hooks/enforce.sh",
            b"#!/bin/bash\necho bundled\n",
        )];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(
            user_dir.path(),
            "claude-code/hooks/enforce.sh",
            "#!/bin/bash\necho overridden\n",
        );

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            String::from_utf8(artifacts[0].content.clone().unwrap()).unwrap(),
            "#!/bin/bash\necho overridden\n"
        );
    }

    #[test]
    fn user_override_adds_a_new_convention_based_file_with_zero_manifest_changes() {
        let bundled: &[(&str, &[u8])] = &[("claude-code/hooks/enforce.sh", b"bundled")];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(
            user_dir.path(),
            "claude-code/hooks/new-hook.sh",
            "new content",
        );

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 2);
        assert!(artifacts
            .iter()
            .any(|a| a.target_relative_path.as_deref() == Some("hooks/new-hook.sh")));
    }

    #[test]
    fn user_override_adds_a_wholly_new_integration_directory() {
        let bundled: &[(&str, &[u8])] = &[];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(
            user_dir.path(),
            "my-custom-agent/.custom/mcp.json",
            r#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}"}}}"#,
        );

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0].target_relative_path.as_deref(),
            Some(".custom/mcp.json")
        );
    }

    #[test]
    fn manifest_at_nested_path_is_not_treated_as_a_manifest() {
        // Only <integration>/config.toml at the integration's own root is a
        // manifest; a same-named file nested under a subdirectory is content.
        let bundled: &[(&str, &[u8])] =
            &[("codex/some-subdir/config.toml", b"not-a-manifest = true")];
        let user_dir = tempfile::tempdir().unwrap();

        // .toml is never convention-based, and this nested file has no
        // [[artifact]] entry referencing it (there's no codex/config.toml
        // manifest at all in this fixture), so it must simply be excluded --
        // not misinterpreted as a manifest and not silently applied.
        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn shared_directory_is_not_directly_discovered_as_a_convention_artifact() {
        let bundled: &[(&str, &[u8])] = &[(
            "shared/agents.md",
            b"shared content, never applied on its own",
        )];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(
            artifacts.is_empty(),
            "shared/ content must only be reachable via a content_file reference, never applied directly"
        );
    }

    #[test]
    fn discovered_artifacts_get_correct_install_step() {
        let bundled: &[(&str, &[u8])] = &[
            ("claude-code/hooks/enforce.sh", b"#!/bin/bash\n"),
            (
                "claude-code/.claude.json",
                br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}"}}}"#,
            ),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        let hook = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some("hooks/enforce.sh"))
            .unwrap();
        assert_eq!(hook.step, super::super::step::InstallStep::Hooks);

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude.json"))
            .unwrap();
        assert_eq!(mcp.step, super::super::step::InstallStep::McpRegistration);
    }

    #[test]
    fn resolver_only_manifest_entry_has_no_static_target_path() {
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
                b"#!/usr/bin/env bash\necho resolver\n",
            ),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert!(a.target_relative_path.is_none());
        assert!(a.content.is_none());
        let resolver = a.resolver.as_ref().expect("should have a resolver spec");
        assert_eq!(resolver.script_filename, "resolve-zed-path.sh");
        assert_eq!(
            resolver.script_bytes,
            b"#!/usr/bin/env bash\necho resolver\n"
        );
        assert!(resolver.extra_args.is_empty());
        assert_eq!(a.step, super::super::step::InstallStep::McpRegistration);
    }

    #[test]
    fn manifest_entry_missing_both_path_and_resolver_is_an_error() {
        let bundled: &[(&str, &[u8])] = &[(
            "broken/config.toml",
            br#"[[artifact]]
strategy = "overwrite"
"#,
        )];
        let user_dir = tempfile::tempdir().unwrap();
        let result = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp");
        assert!(
            result.is_err(),
            "an artifact with neither path nor resolver is a manifest bug"
        );
    }
}
