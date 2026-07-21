mod csharp;
mod elixir;
mod generic;
mod go;
mod helpers;
mod java;
mod js_ts;
mod php;
mod python;
mod ruby;
mod rust_lang;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::graph::GraphBackend;

use helpers::{detect_from_docstring, language_from_file, Lang};

use csharp::detect_csharp_route;
use elixir::detect_elixir_route;
use generic::detect_generic_route;
use go::detect_go_route;
use java::detect_java_route;
use js_ts::detect_js_ts_route;
use php::detect_php_route;
use python::detect_python_route;
use ruby::detect_ruby_route;
use rust_lang::detect_rust_route;

/// A detected HTTP route/endpoint in the codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// HTTP method (GET, POST, PUT, DELETE, PATCH, or UNKNOWN)
    pub method: String,
    /// Inferred URL path (best-effort from symbol/docstring heuristics)
    pub path: String,
    /// Symbol ID of the handler function
    pub handler_id: String,
    /// File containing the handler
    pub file: String,
    /// Detected web framework (e.g. "flask", "express", "spring", "actix")
    pub framework: String,
}

/// Detect HTTP routes/endpoints from the indexed code graph using heuristics.
///
/// Queries symbols and applies language-aware pattern matching on names and
/// docstrings to identify likely HTTP handlers. This is intentionally broad
/// to catch routes across many web frameworks.
pub fn detect_routes(backend: &dyn GraphBackend) -> Result<Vec<Route>> {
    let rows = backend.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
         RETURN s.id, s.name, s.kind, s.file, s.docstring",
    )?;
    Ok(detect_routes_from_rows(&rows))
}

pub fn detect_routes_from_rows(rows: &[Vec<String>]) -> Vec<Route> {
    let mut routes = Vec::new();

    for row in rows {
        let id = &row[0];
        let name = &row[1];
        let _kind = &row[2];
        let file = &row[3];
        let docstring = row.get(4).map(|s| s.as_str()).unwrap_or("");

        if let Some(route) = detect_route_from_symbol(id, name, file, docstring) {
            routes.push(route);
        }
    }

    routes.sort_by(|a, b| a.file.cmp(&b.file).then(a.path.cmp(&b.path)));

    routes
}

/// Try to detect a route from a single symbol's name and docstring.
fn detect_route_from_symbol(id: &str, name: &str, file: &str, docstring: &str) -> Option<Route> {
    let name_lower = name.to_lowercase();
    let doc_lower = docstring.to_lowercase();

    // Determine language from file extension
    let lang = language_from_file(file);

    // Try docstring-based detection first (strongest signal — often contains
    // explicit route/endpoint annotations captured as docstrings)
    if let Some(route) = detect_from_docstring(id, name, file, &doc_lower) {
        return Some(route);
    }

    // Then try name-based heuristics per language
    match lang {
        Lang::Python => detect_python_route(id, name, &name_lower, file, &doc_lower),
        Lang::JavaScript | Lang::TypeScript => {
            detect_js_ts_route(id, name, &name_lower, file, &doc_lower)
        }
        Lang::Go => detect_go_route(id, name, &name_lower, file, &doc_lower),
        Lang::Java => detect_java_route(id, name, &name_lower, file, &doc_lower),
        Lang::Rust => detect_rust_route(id, name, &name_lower, file, &doc_lower),
        Lang::Ruby => detect_ruby_route(id, name, &name_lower, file, &doc_lower),
        Lang::Php => detect_php_route(id, name, &name_lower, file, &doc_lower),
        Lang::CSharp => detect_csharp_route(id, name, &name_lower, file, &doc_lower),
        Lang::Elixir => detect_elixir_route(id, name, &name_lower, file, &doc_lower),
        Lang::Other => detect_generic_route(id, name, &name_lower, file, &doc_lower),
    }
}

/// Format routes as a displayable string.
pub fn format_routes(routes: &[Route]) -> String {
    if routes.is_empty() {
        return "No HTTP routes detected.".to_string();
    }

    let mut out = format!("Detected {} HTTP route(s):\n\n", routes.len());

    let mut current_file = "";
    for route in routes {
        if route.file != current_file {
            current_file = &route.file;
            out.push_str(&format!("  {}:\n", current_file));
        }
        out.push_str(&format!(
            "    {:>7} {:30} [{:15}] [{}]\n",
            route.method, route.path, route.framework, route.handler_id
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use helpers::{camel_to_path, extract_path_from_text};

    #[test]
    fn test_python_get_prefix() {
        let route = detect_route_from_symbol("views.py::get_users", "get_users", "views.py", "");
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/users");
    }

    #[test]
    fn test_python_post_prefix() {
        let route = detect_route_from_symbol("views.py::post_order", "post_order", "views.py", "");
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/order");
    }

    #[test]
    fn test_python_handler_suffix() {
        let route =
            detect_route_from_symbol("views.py::user_handler", "user_handler", "views.py", "");
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.path, "/user");
    }

    #[test]
    fn test_go_handler_suffix() {
        let route = detect_route_from_symbol("api.go::UsersHandler", "UsersHandler", "api.go", "");
        assert!(route.is_some());
        let r = route.unwrap();
        assert!(r.path.contains("users"));
    }

    #[test]
    fn test_go_serve_http() {
        let route = detect_route_from_symbol(
            "server.go::MyHandler::ServeHTTP",
            "ServeHTTP",
            "server.go",
            "",
        );
        assert!(route.is_some());
    }

    #[test]
    fn test_js_handler() {
        let route =
            detect_route_from_symbol("api/users.ts::handler", "handler", "api/users.ts", "");
        assert!(route.is_some());
    }

    #[test]
    fn test_docstring_route() {
        let route = detect_route_from_symbol(
            "app.py::list_items",
            "list_items",
            "app.py",
            "GET /api/items endpoint",
        );
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/api/items");
    }

    #[test]
    fn test_java_controller_file() {
        let route = detect_route_from_symbol(
            "UserController.java::UserController::getUsers",
            "getUsers",
            "com/example/controller/UserController.java",
            "",
        );
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.method, "GET");
    }

    #[test]
    fn test_no_false_positive_regular_function() {
        let route =
            detect_route_from_symbol("utils.py::format_string", "format_string", "utils.py", "");
        assert!(route.is_none());
    }

    #[test]
    fn test_extract_path_from_text() {
        assert_eq!(
            extract_path_from_text("route \"/api/users\""),
            Some("/api/users".to_string())
        );
        assert_eq!(
            extract_path_from_text("GET /api/items endpoint"),
            Some("/api/items".to_string())
        );
    }

    #[test]
    fn test_camel_to_path() {
        assert_eq!(camel_to_path("users"), "users");
        assert_eq!(camel_to_path("user_profile"), "user/profile");
    }
}
