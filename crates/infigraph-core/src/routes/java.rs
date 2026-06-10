use super::helpers::*;
use super::Route;

pub(super) fn detect_java_route(
    id: &str,
    name: &str,
    name_lower: &str,
    file: &str,
    doc_lower: &str,
) -> Option<Route> {
    // Spring-style: method names or docstrings containing Mapping
    if doc_lower.contains("mapping")
        || doc_lower.contains("@get")
        || doc_lower.contains("@post")
        || doc_lower.contains("@put")
        || doc_lower.contains("@delete")
        || doc_lower.contains("@patch")
        || doc_lower.contains("@requestmapping")
        || doc_lower.contains("@getmapping")
        || doc_lower.contains("@postmapping")
        || doc_lower.contains("@putmapping")
        || doc_lower.contains("@deletemapping")
    {
        let method = if doc_lower.contains("@post") || doc_lower.contains("@postmapping") {
            "POST"
        } else if doc_lower.contains("@put") || doc_lower.contains("@putmapping") {
            "PUT"
        } else if doc_lower.contains("@delete") || doc_lower.contains("@deletemapping") {
            "DELETE"
        } else if doc_lower.contains("@patch") || doc_lower.contains("@patchmapping") {
            "PATCH"
        } else {
            "GET"
        };

        let path = extract_path_from_text(doc_lower)
            .unwrap_or_else(|| format!("/{}", camel_to_path(name_lower)));

        return Some(Route {
            method: method.to_string(),
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: detect_java_framework(doc_lower),
        });
    }

    // JAX-RS: @GET, @POST, @Path
    if doc_lower.contains("@path")
        || doc_lower.contains("jax-rs")
        || doc_lower.contains("javax.ws.rs")
    {
        let method = infer_method_from_name(name_lower);
        let path = extract_path_from_text(doc_lower)
            .unwrap_or_else(|| format!("/{}", camel_to_path(name_lower)));
        return Some(Route {
            method,
            path,
            handler_id: id.to_string(),
            file: file.to_string(),
            framework: "jaxrs".to_string(),
        });
    }

    // File in a controller package
    let file_lower = file.to_lowercase();
    let is_controller_file = file_lower.contains("controller")
        || file_lower.contains("resource")
        || file_lower.contains("endpoint");

    if is_controller_file {
        // Methods in controller files that follow REST naming patterns
        let method_prefixes = [
            ("get", "GET"),
            ("find", "GET"),
            ("list", "GET"),
            ("create", "POST"),
            ("save", "POST"),
            ("update", "PUT"),
            ("delete", "DELETE"),
            ("remove", "DELETE"),
        ];

        for (prefix, method) in &method_prefixes {
            if name_lower.starts_with(prefix)
                && name.len() > prefix.len()
                && name
                    .as_bytes()
                    .get(prefix.len())
                    .is_some_and(|b| b.is_ascii_uppercase())
            {
                let rest = &name[prefix.len()..];
                return Some(Route {
                    method: method.to_string(),
                    path: format!("/{}", camel_to_path(&rest.to_lowercase())),
                    handler_id: id.to_string(),
                    file: file.to_string(),
                    framework: detect_java_framework(doc_lower),
                });
            }
        }
    }

    None
}
