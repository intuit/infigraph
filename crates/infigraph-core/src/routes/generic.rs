use super::helpers::*;
use super::Route;

pub(super) fn detect_generic_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    _doc_lower: &str,
) -> Option<Route> {
    let file_lower = file.to_lowercase();

    // Generic: function in a route/handler/api/controller file
    let is_route_file = file_lower.contains("route")
        || file_lower.contains("handler")
        || file_lower.contains("controller")
        || file_lower.contains("endpoint");

    if !is_route_file {
        return None;
    }

    // Common HTTP handler name patterns
    let method_prefixes = [
        ("get_", "GET"),
        ("post_", "POST"),
        ("put_", "PUT"),
        ("delete_", "DELETE"),
        ("handle_", "UNKNOWN"),
    ];

    for (prefix, method) in &method_prefixes {
        if let Some(rest) = name_lower.strip_prefix(prefix) {
            return Some(Route {
                method: method.to_string(),
                path: format!("/{}", rest.replace('_', "/")),
                handler_id: id.to_string(),
                file: file.to_string(),
                framework: "generic".to_string(),
            });
        }
    }

    // Ends with Handler/handler
    if name.ends_with("Handler") || name.ends_with("handler") {
        let base = name.trim_end_matches("Handler").trim_end_matches("handler");
        if !base.is_empty() {
            return Some(Route {
                method: "UNKNOWN".to_string(),
                path: format!("/{}", camel_to_path(&base.to_lowercase())),
                handler_id: id.to_string(),
                file: file.to_string(),
                framework: "generic".to_string(),
            });
        }
    }

    None
}
