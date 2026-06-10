use super::helpers::*;
use super::Route;

pub(super) fn detect_csharp_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    let file_lower = file.to_lowercase();

    // ASP.NET: attribute routing in docstring
    if doc_lower.contains("[httpget")
        || doc_lower.contains("[httppost")
        || doc_lower.contains("[httpput")
        || doc_lower.contains("[httpdelete")
        || doc_lower.contains("[httppatch")
        || doc_lower.contains("[route(")
        || doc_lower.contains("apicontroller")
    {
        let method = if doc_lower.contains("[httppost") {
            "POST".to_string()
        } else if doc_lower.contains("[httpput") {
            "PUT".to_string()
        } else if doc_lower.contains("[httpdelete") {
            "DELETE".to_string()
        } else if doc_lower.contains("[httppatch") {
            "PATCH".to_string()
        } else {
            "GET".to_string()
        };
        let path = extract_path_from_text(doc_lower)
            .unwrap_or_else(|| format!("/{}", camel_to_path(name_lower)));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "aspnet".to_string(),
        });
    }

    // Controller file: file ends with Controller.cs
    if file_lower.ends_with("controller.cs")
        || file_lower.contains("controllers/")
        || file_lower.contains("controllers\\")
    {
        let method_prefixes = [
            ("Get", "GET"),
            ("List", "GET"),
            ("Find", "GET"),
            ("Post", "POST"),
            ("Create", "POST"),
            ("Add", "POST"),
            ("Put", "PUT"),
            ("Update", "PUT"),
            ("Delete", "DELETE"),
            ("Remove", "DELETE"),
            ("Patch", "PATCH"),
        ];
        for (prefix, method) in &method_prefixes {
            if name.starts_with(prefix) && name.len() > prefix.len() {
                let rest = &name[prefix.len()..];
                return Some(Route {
                    method: method.to_string(),
                    path: format!("/{}", camel_to_path(&rest.to_lowercase())),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: "aspnet".to_string(),
                });
            }
        }
    }

    let _ = (name_lower, doc_lower);
    None
}
