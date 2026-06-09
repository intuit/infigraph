use anyhow::{Context, Result};
use serde_json::json;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ConfigFormat {
    Json,
    Toml,
}

pub(crate) struct AgentTarget {
    pub dir_name: &'static str,
    pub config_file: &'static str,
    pub format: ConfigFormat,
    pub label: &'static str,
}

pub(crate) const AGENT_TARGETS: &[AgentTarget] = &[
    AgentTarget { dir_name: ".claude",   config_file: "CLAUDE_CODE_SPECIAL", format: ConfigFormat::Json, label: "Claude Code" },
    AgentTarget { dir_name: ".cursor",   config_file: "mcp.json",    format: ConfigFormat::Json, label: "Cursor" },
    AgentTarget { dir_name: ".vscode",   config_file: "mcp.json",    format: ConfigFormat::Json, label: "VS Code" },
    AgentTarget { dir_name: ".codex",    config_file: "config.toml", format: ConfigFormat::Toml, label: "Codex" },
    AgentTarget { dir_name: ".gemini",   config_file: "settings.json", format: ConfigFormat::Json, label: "Gemini CLI" },
    AgentTarget { dir_name: ".zed",      config_file: "mcp.json",    format: ConfigFormat::Json, label: "Zed" },
    AgentTarget { dir_name: ".opencode", config_file: "config.json", format: ConfigFormat::Json, label: "OpenCode" },
    AgentTarget { dir_name: ".aider",    config_file: "mcp.json",    format: ConfigFormat::Json, label: "Aider" },
    AgentTarget { dir_name: ".windsurf", config_file: "mcp.json",    format: ConfigFormat::Json, label: "Windsurf" },
    AgentTarget { dir_name: ".kiro",     config_file: "mcp.json",    format: ConfigFormat::Json, label: "Kiro" },
    AgentTarget { dir_name: ".copilot",  config_file: "mcp.json",    format: ConfigFormat::Json, label: "GitHub Copilot CLI" },
];

pub(crate) fn install_json_target(
    config_path: &std::path::Path,
    mcp_path_str: &str,
) -> Result<()> {
    let mut config: serde_json::Value = if config_path.is_file() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", config_path.display()))?
    } else {
        json!({})
    };

    if config.get("mcpServers").is_none() {
        config["mcpServers"] = json!({});
    }

    config["mcpServers"]["infigraph"] = json!({
        "command": mcp_path_str,
        "args": ["--ui", "--mcp", "--port=9749"]
    });

    let pretty = serde_json::to_string_pretty(&config)?;
    std::fs::write(config_path, pretty.as_bytes())
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    Ok(())
}

pub(crate) fn install_toml_target(
    config_path: &std::path::Path,
    mcp_path_str: &str,
) -> Result<()> {
    let existing = if config_path.is_file() {
        std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?
    } else {
        String::new()
    };

    let mcp_block = format!(
        "[mcp]\ninfigraph = {{ command = \"{}\", args = [\"--ui\", \"--mcp\", \"--port=9749\"] }}\n",
        mcp_path_str
    );

    let new_content = if existing.is_empty() {
        mcp_block
    } else if let Some(start) = existing.find("[mcp]") {
        let after_header = start + "[mcp]".len();
        let section_end = existing[after_header..]
            .find("\n[")
            .map(|pos| after_header + pos + 1)
            .unwrap_or(existing.len());
        format!("{}{}{}", &existing[..start], mcp_block, &existing[section_end..])
    } else {
        let sep = if existing.ends_with('\n') { "" } else { "\n" };
        format!("{}{}\n{}", existing, sep, mcp_block)
    };

    std::fs::write(config_path, new_content.as_bytes())
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    Ok(())
}

pub(crate) fn uninstall_json_target<'a>(
    config_path: &std::path::Path,
    label: &'a str,
) -> Result<Option<&'a str>> {
    if !config_path.is_file() {
        println!("  Skipping {} (no config found)", label);
        return Ok(None);
    }

    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let mut config: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    if let Some(servers) = config.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        if servers.remove("infigraph").is_some() {
            let pretty = serde_json::to_string_pretty(&config)?;
            std::fs::write(config_path, pretty.as_bytes())
                .with_context(|| format!("Failed to write {}", config_path.display()))?;
            println!("  Removed infigraph from {} ({})", label, config_path.display());
            return Ok(Some(label));
        } else {
            println!("  Skipping {} (infigraph entry not found)", label);
        }
    } else {
        println!("  Skipping {} (no mcpServers in config)", label);
    }

    Ok(None)
}

pub(crate) fn uninstall_toml_target<'a>(
    config_path: &std::path::Path,
    label: &'a str,
) -> Result<Option<&'a str>> {
    if !config_path.is_file() {
        println!("  Skipping {} (no config found)", label);
        return Ok(None);
    }

    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    if let Some(start) = content.find("[mcp]") {
        let after_header = start + "[mcp]".len();
        let section_end = content[after_header..]
            .find("\n[")
            .map(|pos| after_header + pos + 1)
            .unwrap_or(content.len());

        let section_text = &content[start..section_end];
        if section_text.contains("infigraph") {
            let new_content = format!("{}{}", &content[..start], &content[section_end..]);
            let trimmed = new_content.trim_end().to_string();
            let final_content = if trimmed.is_empty() {
                String::new()
            } else {
                format!("{}\n", trimmed)
            };
            std::fs::write(config_path, final_content.as_bytes())
                .with_context(|| format!("Failed to write {}", config_path.display()))?;
            println!("  Removed infigraph from {} ({})", label, config_path.display());
            return Ok(Some(label));
        } else {
            println!("  Skipping {} (infigraph entry not found in [mcp])", label);
        }
    } else {
        println!("  Skipping {} (no [mcp] section in config)", label);
    }

    Ok(None)
}
