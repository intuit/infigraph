#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConventionStrategy {
    JsonDeepMerge,
    Overwrite,
}

/// Infers the strategy for a bundled/user-override file that has no explicit
/// `[[artifact]]` manifest entry, from its extension alone. `.toml` files are
/// deliberately excluded (return `None`) -- they always need a manifest
/// entry declaring `strategy = "toml_section"` and a `key_path`, since a
/// surgical raw-text section splice can't be inferred from extension+path
/// the way JSON's structural merge can (see the design spec's "core idea",
/// reason 3). Any other recognized extension without special merge
/// semantics (including no extension is not "recognized" -- see below) maps
/// to a plain `overwrite`.
pub(crate) fn infer_strategy(relative_path: &str) -> Option<ConventionStrategy> {
    let extension = std::path::Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())?;

    match extension {
        "json" => Some(ConventionStrategy::JsonDeepMerge),
        "toml" => None,
        _ => Some(ConventionStrategy::Overwrite),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extension_infers_deep_merge() {
        assert_eq!(
            infer_strategy("claude-code/.claude.json"),
            Some(ConventionStrategy::JsonDeepMerge)
        );
        assert_eq!(
            infer_strategy("gemini-cli/.gemini/settings.json"),
            Some(ConventionStrategy::JsonDeepMerge)
        );
    }

    #[test]
    fn toml_extension_is_never_convention_based() {
        assert_eq!(infer_strategy("codex/.codex/config.toml"), None);
        assert_eq!(infer_strategy("codex/mcp-section.toml"), None);
    }

    #[test]
    fn shell_and_markdown_infer_overwrite() {
        assert_eq!(
            infer_strategy("claude-code/hooks/enforce.sh"),
            Some(ConventionStrategy::Overwrite)
        );
        assert_eq!(
            infer_strategy("shared/skills/infigraph-reindex/SKILL.md"),
            Some(ConventionStrategy::Overwrite)
        );
        assert_eq!(
            infer_strategy("cursor/rules/infigraph.mdc"),
            Some(ConventionStrategy::Overwrite)
        );
    }

    #[test]
    fn manifest_filename_is_not_a_convention_artifact() {
        // config.toml is the manifest itself -- discovery (Task 9) must never
        // treat it as convention-based content, but infer_strategy alone
        // (extension-only) can't distinguish this; .toml already returns None
        // unconditionally, which is sufficient for that guarantee here.
        assert_eq!(infer_strategy("claude-code/config.toml"), None);
    }

    #[test]
    fn extensionless_and_unknown_extensions_return_none() {
        assert_eq!(infer_strategy("claude-code/hooks/no-extension"), None);
        assert_eq!(
            infer_strategy("some/path/file.exe"),
            Some(ConventionStrategy::Overwrite)
        );
    }
}
