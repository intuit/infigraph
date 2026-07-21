use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::open_prism;

pub fn tool_detect_reflection(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let sites = infigraph_core::reflection::detect_reflection_sites(backend, &root)?;

    let mechanism_filter = args
        .get("mechanism")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let filtered: Vec<_> = if let Some(ref m) = mechanism_filter {
        sites
            .iter()
            .filter(|s| s.mechanism.to_lowercase() == *m)
            .cloned()
            .collect()
    } else {
        sites
    };

    Ok(infigraph_core::reflection::format_reflection_sites(
        &filtered,
    ))
}
