use super::helpers::*;
use super::Route;

pub(super) fn detect_go_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Go convention: Handler suffix, ServeHTTP method
    if name == "ServeHTTP" {
        let parts: Vec<&str> = id.rsplitn(2, "::").collect();
        let parent = parts.last().unwrap_or(&"");
        let parent_name = parent.rsplit("::").next().unwrap_or(parent);
        let path = format!(
            "/{}",
            parent_name.to_lowercase().trim_end_matches("handler")
        );
        return Some(Route {
            method: "UNKNOWN".to_string(),
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "net/http".to_string(),
        });
    }

    // Functions ending in Handler
    if name.ends_with("Handler") && name.len() > "Handler".len() {
        let base = &name[..name.len() - "Handler".len()];
        let method = infer_method_from_name(&base.to_lowercase());
        let path = format!("/{}", camel_to_path(&base.to_lowercase()));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_go_framework(doc_lower),
        });
    }

    // Functions starting with Handle
    if name.starts_with("Handle") && name.len() > "Handle".len() {
        let base = &name["Handle".len()..];
        let method = infer_method_from_name(&base.to_lowercase());
        let path = format!("/{}", camel_to_path(&base.to_lowercase()));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_go_framework(doc_lower),
        });
    }

    // Docstring mentions http.HandleFunc or similar
    if doc_lower.contains("handlefunc")
        || doc_lower.contains("http.handle")
        || doc_lower.contains("gin.")
        || doc_lower.contains("echo.")
        || doc_lower.contains("chi.")
    {
        return Some(Route {
            method: "UNKNOWN".to_string(),
            path: format!("/{}", camel_to_path(name_lower)),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_go_framework(doc_lower),
        });
    }

    // Go file naming: handler.go, routes.go, api.go
    let file_lower = file.to_lowercase();
    let is_handler_file = file_lower.ends_with("handler.go")
        || file_lower.ends_with("handlers.go")
        || file_lower.ends_with("routes.go")
        || file_lower.ends_with("api.go");

    if is_handler_file && name.starts_with(|c: char| c.is_uppercase()) {
        // Exported functions in handler files are likely handlers
        let method = infer_method_from_name(name_lower);
        let path = format!("/{}", camel_to_path(name_lower));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_go_framework(doc_lower),
        });
    }

    None
}
