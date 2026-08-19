use super::strategy::Strategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallStep {
    /// Every mcp.json-shaped artifact across all integrations.
    McpRegistration,
    /// CLAUDE.md, editor rules, and the shared reindex skill.
    DocsAndRules,
    /// Hook scripts and their settings.json wiring.
    Hooks,
    /// Semantic search model files -- unchanged, not artifact-based.
    Models,
}

impl InstallStep {
    pub(crate) const ALL: &'static [InstallStep] = &[
        InstallStep::McpRegistration,
        InstallStep::DocsAndRules,
        InstallStep::Hooks,
        InstallStep::Models,
    ];

    /// Classifies an artifact by its destination path (for `path`/mirrored
    /// paths, not the bundled-resources source path) and strategy. A
    /// `marker_delimited` artifact is always `DocsAndRules` regardless of
    /// path (there is no other kind of marker-delimited content in this
    /// design). Otherwise: anything under a `hooks/` path segment is
    /// `Hooks`; anything under `rules/` or `skills/` is `DocsAndRules`;
    /// everything else is `McpRegistration`. This heuristic only needs to be
    /// *good enough* to group #50's future `--mode` flag -- no such flag
    /// ships yet.
    pub(crate) fn classify(relative_target_path: &str, strategy: Strategy) -> InstallStep {
        if strategy == Strategy::MarkerDelimited {
            return InstallStep::DocsAndRules;
        }
        let segments: Vec<&str> = relative_target_path.split('/').collect();
        if segments.contains(&"hooks") {
            InstallStep::Hooks
        } else if segments.iter().any(|s| *s == "rules" || *s == "skills") {
            InstallStep::DocsAndRules
        } else {
            InstallStep::McpRegistration
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::strategy::Strategy;
    use super::*;

    #[test]
    fn hooks_subdirectory_classifies_as_hooks() {
        assert_eq!(
            InstallStep::classify("claude-code/hooks/enforce.sh", Strategy::Overwrite),
            InstallStep::Hooks
        );
    }

    #[test]
    fn rules_subdirectory_classifies_as_docs_and_rules() {
        assert_eq!(
            InstallStep::classify("cursor/rules/infigraph.mdc", Strategy::Overwrite),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn skills_subdirectory_classifies_as_docs_and_rules() {
        assert_eq!(
            InstallStep::classify(
                ".claude/skills/infigraph-reindex/SKILL.md",
                Strategy::Overwrite
            ),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn marker_delimited_strategy_classifies_as_docs_and_rules_regardless_of_path() {
        assert_eq!(
            InstallStep::classify(".claude/CLAUDE.md", Strategy::MarkerDelimited),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn plain_mcp_fragment_classifies_as_mcp_registration() {
        assert_eq!(
            InstallStep::classify(".claude.json", Strategy::JsonDeepMerge),
            InstallStep::McpRegistration
        );
        assert_eq!(
            InstallStep::classify(".codex/config.toml", Strategy::TomlSection),
            InstallStep::McpRegistration
        );
        assert_eq!(
            InstallStep::classify(".vscode/mcp.json", Strategy::JsonKeyPath),
            InstallStep::McpRegistration
        );
    }

    #[test]
    fn all_contains_every_variant_exactly_once() {
        let all = InstallStep::ALL;
        assert_eq!(all.len(), 4);
        for step in [
            InstallStep::McpRegistration,
            InstallStep::DocsAndRules,
            InstallStep::Hooks,
            InstallStep::Models,
        ] {
            assert_eq!(all.iter().filter(|s| **s == step).count(), 1);
        }
    }
}
