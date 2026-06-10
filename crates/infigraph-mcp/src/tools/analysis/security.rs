use anyhow::{Context, Result};
use serde_json::Value;

use infigraph_core::model::BridgeKind;

pub(crate) fn tool_detect_security_issues(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path'")?;
    let root = std::path::PathBuf::from(path)
        .canonicalize()
        .context("invalid path")?;

    let sev_filter = args
        .get("severity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase());
    let cat_filter = args
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let mut scan = infigraph_core::security::scan_project(&root)?;

    // Apply filters
    if let Some(ref sev) = sev_filter {
        scan.findings.retain(|f| f.severity.to_string() == *sev);
    }
    if let Some(ref cat) = cat_filter {
        scan.findings.retain(|f| {
            f.category.to_string().to_lowercase().replace(' ', "") == cat.replace(' ', "")
        });
    }

    Ok(infigraph_core::security::format_scan_results(&scan))
}

pub(crate) fn tool_detect_bridges(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .context("missing 'path' argument")?;
    let kind_filter = args.get("kind").and_then(|v| v.as_str());

    let result = infigraph_core::bridges::detect_bridges(&std::path::PathBuf::from(path))?;

    let bridges: Vec<_> = match kind_filter {
        Some(k) => {
            let k_upper = k.to_uppercase();
            result
                .bridges
                .iter()
                .filter(|b| b.kind.as_str() == k_upper)
                .collect()
        }
        None => result.bridges.iter().collect(),
    };

    if bridges.is_empty() {
        let filter_note = kind_filter
            .map(|k| format!(" (filter: {k})"))
            .unwrap_or_default();
        return Ok(format!("No cross-language bridges detected{filter_note}."));
    }

    let ffi = result.ffi_count();
    let jni = result.jni_count();
    let grpc = result.grpc_count();
    let pinvoke = result.pinvoke_count();
    let cgo = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Cgo)
        .count();
    let ctypes = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Ctypes)
        .count();
    let wasm = result
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Wasm)
        .count();
    let com = result.com_count();

    let mut out = format!(
        "Cross-language bridges: {} total\n  FFI={} JNI={} CGO={} gRPC={} P/Invoke={} ctypes={} WASM={} COM={}\n\n",
        result.bridges.len(), ffi, jni, cgo, grpc, pinvoke, ctypes, wasm, com
    );

    // Group by file
    let mut by_file: std::collections::HashMap<&str, Vec<_>> = std::collections::HashMap::new();
    for b in &bridges {
        by_file.entry(&b.file).or_default().push(b);
    }
    let mut files: Vec<&str> = by_file.keys().copied().collect();
    files.sort_unstable();

    for file in files {
        let file_bridges = &by_file[file];
        out.push_str(&format!("{}:\n", file));
        let mut sorted = file_bridges.to_vec();
        sorted.sort_by_key(|b| b.line);
        for b in sorted {
            let target = b.target_language.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "  L{} [{}] {} -> {} | {}\n",
                b.line,
                b.kind.as_str(),
                b.foreign_symbol,
                target,
                b.detail
            ));
        }
    }

    Ok(out)
}
