#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateFormat {
    Json,
    Toml,
}

/// Escapes `mcp_path` for safe embedding inside a JSON or TOML string literal,
/// then replaces every `{{mcp_path}}` occurrence in `content` with it.
pub(crate) fn substitute_mcp_path(content: &str, mcp_path: &str, format: TemplateFormat) -> String {
    let escaped = match format {
        // JSON and TOML basic-string escaping happen to coincide for the two
        // characters a filesystem path can contain that matter here.
        TemplateFormat::Json | TemplateFormat::Toml => {
            mcp_path.replace('\\', "\\\\").replace('"', "\\\"")
        }
    };
    content.replace("{{mcp_path}}", &escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_substitution_escapes_backslashes_and_quotes() {
        let content = r#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#;
        let result = substitute_mcp_path(
            content,
            r"C:\Users\foo\infigraph-mcp.exe",
            TemplateFormat::Json,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("must still be valid JSON");
        assert_eq!(
            parsed["mcpServers"]["infigraph"]["command"],
            r"C:\Users\foo\infigraph-mcp.exe"
        );
    }

    #[test]
    fn json_substitution_leaves_plain_unix_path_untouched() {
        let content = r#"{"command":"{{mcp_path}}"}"#;
        let result = substitute_mcp_path(content, "/usr/bin/infigraph-mcp", TemplateFormat::Json);
        assert_eq!(result, r#"{"command":"/usr/bin/infigraph-mcp"}"#);
    }

    #[test]
    fn toml_substitution_escapes_backslashes_and_quotes() {
        let content = "command = \"{{mcp_path}}\"\nargs = [\"--mcp\"]\n";
        let result = substitute_mcp_path(
            content,
            r"C:\Users\foo\infigraph-mcp.exe",
            TemplateFormat::Toml,
        );
        let parsed: toml::Value =
            toml::from_str(&format!("[x]\n{result}")).expect("must still be valid TOML");
        assert_eq!(
            parsed["x"]["command"].as_str(),
            Some(r"C:\Users\foo\infigraph-mcp.exe")
        );
    }

    #[test]
    fn substitution_handles_multiple_occurrences() {
        let content = r#"{"a":"{{mcp_path}}","b":"{{mcp_path}}"}"#;
        let result = substitute_mcp_path(content, "/bin/x", TemplateFormat::Json);
        assert_eq!(result, r#"{"a":"/bin/x","b":"/bin/x"}"#);
    }
}
