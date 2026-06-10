use super::helpers::*;
use super::Route;

pub(super) fn detect_elixir_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    let file_lower = file.to_lowercase();

    // Phoenix controller: file in lib/*/controllers/ or web/controllers/
    let is_phoenix_controller = file_lower.contains("/controllers/")
        && (file_lower.ends_with("_controller.ex") || file_lower.ends_with("_controller.exs"));

    let phoenix_actions = [
        ("index", "GET"),
        ("show", "GET"),
        ("new", "GET"),
        ("create", "POST"),
        ("edit", "GET"),
        ("update", "PUT"),
        ("delete", "DELETE"),
    ];

    if is_phoenix_controller {
        for (action, method) in &phoenix_actions {
            if name_lower == *action {
                return Some(Route {
                    method: method.to_string(),
                    path: format!("/{}", action),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "phoenix".to_string(),
                });
            }
        }
    }

    // Plug: docstring or name references plug
    if doc_lower.contains("plug") || doc_lower.contains("phoenix") {
        let method = infer_method_from_name(name_lower);
        return Some(Route {
            method,
            path: format!("/{}", name_lower.replace('_', "/")),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: if doc_lower.contains("phoenix") {
                "phoenix".to_string()
            } else {
                "plug".to_string()
            },
        });
    }

    let _ = (name, doc_lower);
    None
}
