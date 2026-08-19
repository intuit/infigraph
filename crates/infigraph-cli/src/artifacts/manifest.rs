use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IntegrationManifest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(rename = "artifact", default)]
    pub artifacts: Vec<ArtifactEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArtifactEntry {
    #[serde(default)]
    pub path: Option<String>,
    pub strategy: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub content_file: Option<String>,
    #[serde(default)]
    pub key_path: Option<Vec<String>>,
    #[serde(default)]
    pub resolver: Option<Vec<String>>,
}

pub(crate) fn parse_manifest(content: &str) -> Result<IntegrationManifest> {
    toml::from_str(content).context("failed to parse integration manifest (config.toml)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest_with_one_artifact() {
        let toml = r#"
label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert_eq!(manifest.label.as_deref(), Some("Claude Code"));
        assert_eq!(manifest.artifacts.len(), 1);
        let a = &manifest.artifacts[0];
        assert_eq!(a.path.as_deref(), Some(".claude/CLAUDE.md"));
        assert_eq!(a.strategy, "marker_delimited");
        assert_eq!(
            a.start.as_deref(),
            Some("<!-- infigraph-primary-search -->")
        );
        assert_eq!(a.end.as_deref(), Some("<!-- /infigraph-primary-search -->"));
        assert_eq!(a.content_file.as_deref(), Some("../shared/agents.md"));
        assert!(a.key_path.is_none());
        assert!(a.resolver.is_none());
    }

    #[test]
    fn parses_manifest_with_multiple_artifacts_and_no_label() {
        let toml = r#"
[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"

[[artifact]]
path = ".codex/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert!(manifest.label.is_none());
        assert_eq!(manifest.artifacts.len(), 2);
        assert_eq!(
            manifest.artifacts[0].key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        assert_eq!(manifest.artifacts[1].strategy, "overwrite");
    }

    #[test]
    fn parses_resolver_field() {
        let toml = r#"
[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert_eq!(
            manifest.artifacts[0].resolver,
            Some(vec!["./resolve-zed-path.sh".to_string()])
        );
        assert!(manifest.artifacts[0].path.is_none());
    }

    #[test]
    fn rejects_malformed_toml() {
        let result = parse_manifest("this is not [ valid toml");
        assert!(result.is_err());
    }

    #[test]
    fn resolver_only_entry_with_no_path_parses_successfully() {
        let toml = r#"
[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#;
        let manifest = parse_manifest(toml).expect("resolver-only entries have no static path");
        assert!(manifest.artifacts[0].path.is_none());
        assert_eq!(
            manifest.artifacts[0].resolver,
            Some(vec!["./resolve-zed-path.sh".to_string()])
        );
    }
}
