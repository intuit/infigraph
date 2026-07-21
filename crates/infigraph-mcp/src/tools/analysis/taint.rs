use anyhow::{Context, Result};
use serde_json::Value;

use super::super::helpers::open_prism;

pub fn tool_detect_taint_flows(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let flows = infigraph_core::taint::detect_taint_flows(backend, &root)?;

    let category_filter = args
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let show_sanitized = args
        .get("show_sanitized")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let filtered: Vec<_> = flows
        .iter()
        .filter(|f| {
            category_filter
                .as_ref()
                .is_none_or(|c| f.sink_category.to_lowercase() == *c)
                && (show_sanitized || !f.sanitized)
        })
        .cloned()
        .collect();

    Ok(infigraph_core::taint::format_taint_flows(&filtered))
}

pub fn tool_detect_interprocedural_taint(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let flows = infigraph_core::taint::interprocedural::detect_interprocedural_taint(
        backend, &root, max_depth,
    )?;

    let category_filter = args
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let filtered: Vec<_> = if let Some(ref c) = category_filter {
        flows
            .iter()
            .filter(|f| f.sink_category.to_lowercase() == *c)
            .cloned()
            .collect()
    } else {
        flows
    };

    Ok(infigraph_core::taint::interprocedural::format_interprocedural_flows(&filtered))
}

pub fn tool_detect_dynamic_urls(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let urls = infigraph_core::taint::dynamic_urls::detect_dynamic_urls(backend, &root)?;

    Ok(infigraph_core::taint::dynamic_urls::format_dynamic_urls(
        &urls,
    ))
}

pub fn tool_detect_path_traversal(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let root = prism.root().to_path_buf();
    let backend = prism.backend().context("not initialized")?;

    let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(5) as u32;

    let flows =
        infigraph_core::taint::path_traversal::detect_path_traversal(backend, &root, max_depth)?;

    Ok(infigraph_core::taint::path_traversal::format_path_traversal(&flows))
}
