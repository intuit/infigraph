use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::open_prism;

pub fn tool_detect_cross_cutting(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let matches = infigraph_core::concerns::detect_cross_cutting(backend)?;

    let kind_filter = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let filtered: Vec<_> = if let Some(ref k) = kind_filter {
        matches
            .iter()
            .filter(|m| m.kind.to_lowercase() == *k)
            .cloned()
            .collect()
    } else {
        matches
    };

    Ok(infigraph_core::concerns::format_concerns(&filtered))
}
