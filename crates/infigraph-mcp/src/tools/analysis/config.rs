use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::open_prism;

pub fn tool_detect_config_bindings(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let bindings = infigraph_core::config::detect_config_bindings(backend)?;
    let config_files = infigraph_core::config::detect_config_files(&root);

    let kind_filter = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let profile_filter = args
        .get("profile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let filtered: Vec<_> = bindings
        .iter()
        .filter(|b| {
            kind_filter
                .as_ref()
                .is_none_or(|k| b.kind.to_lowercase() == *k)
                && profile_filter
                    .as_ref()
                    .is_none_or(|p| b.profile.to_lowercase() == *p)
        })
        .cloned()
        .collect();

    Ok(infigraph_core::config::format_config_bindings(
        &filtered,
        &config_files,
    ))
}
