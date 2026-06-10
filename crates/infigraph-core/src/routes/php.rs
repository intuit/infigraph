use super::helpers::*;
use super::Route;

pub(super) fn detect_php_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    let file_lower = file.to_lowercase();

    // Laravel: app/Http/Controllers/, RESTful resource methods
    let is_laravel_controller =
        file_lower.contains("http/controllers") || file_lower.contains("http\\controllers");

    let laravel_actions = [
        ("index", "GET"),
        ("show", "GET"),
        ("create", "GET"),
        ("store", "POST"),
        ("edit", "GET"),
        ("update", "PUT"),
        ("destroy", "DELETE"),
    ];

    if is_laravel_controller {
        for (action, method) in &laravel_actions {
            if name_lower == *action {
                return Some(Route {
                    method: method.to_string(),
                    path: format!("/{}", action),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "laravel".to_string(),
                });
            }
        }
    }

    // Symfony: docstring contains @Route or #[Route
    if doc_lower.contains("@route")
        || doc_lower.contains("#[route")
        || doc_lower.contains("symfony")
    {
        let method = infer_method_from_name(name_lower);
        let path = extract_path_from_text(doc_lower)
            .unwrap_or_else(|| format!("/{}", name_lower.replace('_', "/")));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "symfony".to_string(),
        });
    }

    // Slim: docstring mentions slim
    if doc_lower.contains("slim") {
        let method = infer_method_from_name(name_lower);
        return Some(Route {
            method,
            path: format!("/{}", name_lower.replace('_', "/")),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "slim".to_string(),
        });
    }

    let _ = (name, doc_lower);
    None
}
