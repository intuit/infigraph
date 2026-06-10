use super::helpers::*;
use super::Route;

pub(super) fn detect_js_ts_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Express-style: function names get, post, put, delete (as methods)
    let exact_methods = [
        ("get", "GET"),
        ("post", "POST"),
        ("put", "PUT"),
        ("delete", "DELETE"),
        ("patch", "PATCH"),
    ];

    if id.contains("::") {
        for (exact, method) in &exact_methods {
            if name_lower == *exact {
                let parts: Vec<&str> = id.rsplitn(2, "::").collect();
                let parent = parts.last().unwrap_or(&"");
                let parent_name = parent.rsplit("::").next().unwrap_or(parent);
                let path = format!(
                    "/{}",
                    parent_name
                        .to_lowercase()
                        .trim_end_matches("router")
                        .trim_end_matches("controller")
                );
                return Some(Route {
                    method: method.to_string(),
                    path,
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: detect_js_framework(file, doc_lower),
                });
            }
        }
    }

    // "handler" is a common name for serverless/API route handlers
    if name_lower == "handler" || name_lower == "default" {
        // Next.js API routes: the file path IS the route
        let method = if doc_lower.contains("post") {
            "POST"
        } else if doc_lower.contains("put") {
            "PUT"
        } else if doc_lower.contains("delete") {
            "DELETE"
        } else {
            "GET"
        };
        let path = infer_path_from_file(file);
        return Some(Route {
            method: method.to_string(),
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: if file.contains("/api/") || file.contains("pages/") {
                "nextjs".to_string()
            } else {
                detect_js_framework(file, doc_lower)
            },
        });
    }

    // NestJS-style: methods with names like getUsers, createUser, deleteUser
    let method_prefixes = [
        ("get", "GET"),
        ("find", "GET"),
        ("list", "GET"),
        ("fetch", "GET"),
        ("create", "POST"),
        ("add", "POST"),
        ("update", "PUT"),
        ("edit", "PUT"),
        ("remove", "DELETE"),
        ("delete", "DELETE"),
        ("handle", "UNKNOWN"),
    ];

    // Only if the file looks like a controller/route file
    let file_lower = file.to_lowercase();
    let is_route_file = file_lower.contains("controller")
        || file_lower.contains("route")
        || file_lower.contains("handler")
        || file_lower.contains("api")
        || file_lower.contains("endpoint");

    if is_route_file {
        for (prefix, method) in &method_prefixes {
            if name_lower.starts_with(prefix) && name_lower.len() > prefix.len() {
                let rest = &name_lower[prefix.len()..];
                // Ensure it's camelCase boundary (next char should be uppercase in original)
                if name.len() > prefix.len() && name.as_bytes()[prefix.len()].is_ascii_uppercase() {
                    let path = format!("/{}", camel_to_path(rest));
                    return Some(Route {
                        method: method.to_string(),
                        path,
                        handler_id: id.to_string(),
                        file: file.to_string(),
                        framework: detect_js_framework(file, doc_lower),
                    });
                }
            }
        }
    }

    // Names ending with Handler, Controller, Route
    if name_lower.ends_with("handler")
        || name_lower.ends_with("controller")
        || name_lower.ends_with("route")
    {
        let base = name_lower
            .trim_end_matches("handler")
            .trim_end_matches("controller")
            .trim_end_matches("route");
        if !base.is_empty() {
            return Some(Route {
                method: "UNKNOWN".to_string(),
                path: format!("/{}", camel_to_path(base)),
                handler_id: id.to_string(),
                file: file.to_string(),
                framework: detect_js_framework(file, doc_lower),
            });
        }
    }

    None
}
