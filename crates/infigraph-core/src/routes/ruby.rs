use super::helpers::*;
use super::Route;

pub(super) fn detect_ruby_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Rails controller: file in app/controllers/, methods index/show/create/update/destroy
    let file_lower = file.to_lowercase();
    let is_rails_controller =
        file_lower.contains("app/controllers/") || file_lower.contains("app\\controllers\\");

    let rails_actions = [
        ("index", "GET"),
        ("show", "GET"),
        ("new", "GET"),
        ("create", "POST"),
        ("edit", "GET"),
        ("update", "PUT"),
        ("destroy", "DELETE"),
    ];

    if is_rails_controller {
        for (action, method) in &rails_actions {
            if name_lower == *action {
                let controller = file_lower
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches("_controller.rb")
                    .trim_end_matches(".rb");
                return Some(Route {
                    method: method.to_string(),
                    path: format!(
                        "/{}/{}",
                        controller,
                        if *action == "index" { "" } else { action }
                    )
                    .trim_end_matches('/')
                    .to_string(),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "rails".to_string(),
                });
            }
        }
    }

    // Sinatra: docstring or file mentions sinatra
    if doc_lower.contains("sinatra") || file_lower.contains("sinatra") {
        let method = infer_method_from_name(name_lower);
        return Some(Route {
            method,
            path: format!("/{}", name_lower.replace('_', "/")),
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "sinatra".to_string(),
        });
    }

    // Generic Ruby: _handler/_action/_endpoint suffix in route/api files
    if file_lower.contains("route") || file_lower.contains("api") || file_lower.contains("endpoint")
    {
        let suffixes = ["_handler", "_action", "_endpoint"];
        for suffix in &suffixes {
            if let Some(base) = name_lower.strip_suffix(suffix) {
                return Some(Route {
                    method: infer_method_from_name(base),
                    path: format!("/{}", base.replace('_', "/")),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "generic_ruby".to_string(),
                });
            }
        }
    }

    let _ = (name, doc_lower);
    None
}
