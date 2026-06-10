use super::helpers::*;
use super::Route;

pub(super) fn detect_rust_route(
    id: &str,
    _name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Docstring/attribute-based: #[get], #[post], etc.
    if doc_lower.contains("#[get")
        || doc_lower.contains("#[post")
        || doc_lower.contains("#[put")
        || doc_lower.contains("#[delete")
        || doc_lower.contains("#[patch")
        || doc_lower.contains("actix")
        || doc_lower.contains("axum")
        || doc_lower.contains("rocket")
    {
        let method = if doc_lower.contains("#[post") || doc_lower.contains("post") {
            "POST"
        } else if doc_lower.contains("#[put") || doc_lower.contains("put") {
            "PUT"
        } else if doc_lower.contains("#[delete") || doc_lower.contains("delete") {
            "DELETE"
        } else if doc_lower.contains("#[patch") {
            "PATCH"
        } else {
            "GET"
        };

        let path = extract_path_from_text(doc_lower)
            .unwrap_or_else(|| format!("/{}", name_lower.replace('_', "/")));

        return Some(Route {
            method: method.to_string(),
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_rust_framework(doc_lower),
        });
    }

    // Name-based patterns for handler files
    let file_lower = file.to_lowercase();
    let is_handler_file = file_lower.contains("handler")
        || file_lower.contains("route")
        || file_lower.contains("api");

    if is_handler_file {
        let method_prefixes = [
            ("get_", "GET"),
            ("post_", "POST"),
            ("put_", "PUT"),
            ("delete_", "DELETE"),
            ("create_", "POST"),
            ("update_", "PUT"),
            ("remove_", "DELETE"),
            ("list_", "GET"),
            ("handle_", "UNKNOWN"),
        ];

        for (prefix, method) in &method_prefixes {
            if let Some(rest) = name_lower.strip_prefix(prefix) {
                return Some(Route {
                    method: method.to_string(),
                    path: format!("/{}", rest.replace('_', "/")),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: detect_rust_framework(doc_lower),
                });
            }
        }
    }

    None
}
