use super::helpers::*;
use super::Route;

pub(super) fn detect_python_route(
    id: &str,
    _name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Python naming patterns for HTTP handlers
    let method_prefixes = [
        ("get_", "GET"),
        ("post_", "POST"),
        ("put_", "PUT"),
        ("delete_", "DELETE"),
        ("patch_", "PATCH"),
        ("handle_", "UNKNOWN"),
        ("on_get", "GET"),
        ("on_post", "POST"),
        ("on_put", "PUT"),
        ("on_delete", "DELETE"),
    ];

    for (prefix, method) in &method_prefixes {
        if let Some(path_part) = name_lower.strip_prefix(prefix) {
            let path = if path_part.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", path_part.replace('_', "/"))
            };
            return Some(Route {
                method: method.to_string(),
                path,
                handler_id: id.to_string(),
                file: file.to_string(),
                framework: detect_python_framework(doc_lower),
            });
        }
    }

    // Django class-based views: methods named get, post, put, delete, patch
    let exact_methods = [
        ("get", "GET"),
        ("post", "POST"),
        ("put", "PUT"),
        ("delete", "DELETE"),
        ("patch", "PATCH"),
    ];

    // Only match exact method names when they're methods (contain :: separator)
    if id.contains("::") {
        for (exact, method) in &exact_methods {
            if name_lower == *exact {
                // Infer path from the class name (parent in the id)
                let parts: Vec<&str> = id.rsplitn(2, "::").collect();
                let parent = parts.last().unwrap_or(&"");
                let parent_name = parent.rsplit("::").next().unwrap_or(parent);
                let path = format!(
                    "/{}",
                    parent_name
                        .to_lowercase()
                        .trim_end_matches("view")
                        .trim_end_matches("viewset")
                        .trim_end_matches("handler")
                );
                return Some(Route {
                    method: method.to_string(),
                    path,
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "django".to_string(),
                });
            }
        }
    }

    // Docstring mentions flask/fastapi/django route keywords
    if doc_lower.contains("flask") || doc_lower.contains("fastapi") || doc_lower.contains("django")
    {
        return Some(Route {
            method: "UNKNOWN".to_string(),
            path: format!("/{}", name_lower.replace('_', "/")),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_python_framework(doc_lower),
        });
    }

    // Names ending in _handler, _view, _endpoint
    if name_lower.ends_with("_handler")
        || name_lower.ends_with("_view")
        || name_lower.ends_with("_endpoint")
        || name_lower.ends_with("_api")
    {
        let base = name_lower
            .trim_end_matches("_handler")
            .trim_end_matches("_view")
            .trim_end_matches("_endpoint")
            .trim_end_matches("_api");
        return Some(Route {
            method: infer_method_from_name(base),
            path: format!("/{}", base.replace('_', "/")),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_python_framework(doc_lower),
        });
    }

    None
}
