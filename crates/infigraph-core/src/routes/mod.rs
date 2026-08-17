mod go;
mod helpers;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::graph::GraphBackend;

use go::detect_go_route;
use helpers::{detect_from_docstring, language_from_file, Lang};

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

/// Detect HTTP routes/endpoints from the indexed code graph.
///
/// A symbol is treated as a route only on real evidence:
///   * a `Route`-kind symbol — a route the extractor captured from an explicit
///     registration (Django `urlpatterns`, Express `app.get(...)`, PHP router,
///     etc.), stored as `"METHOD /path"` in its name; or
///   * a `Function`/`Method` whose captured docstring carries a route
///     decorator/annotation (`@router.post`, `@GetMapping`, `#[get]`, …); or
///   * a naming convention in languages where naming *is* the routing mechanism
///     and there is no decorator/registration syntax (Go `ServeHTTP`/`*Handler`).
///
/// A bare verb-prefixed name (`get_users`, `handle_message`) with no decorator
/// and no registration is NOT a route — guessing from the name alone produced
/// false positives (AIF3X-331).
pub fn detect_routes(backend: &dyn GraphBackend) -> Result<Vec<Route>> {
    // Use the repo-scoped `symbols_with_docstring` accessor instead of a global
    // `MATCH (s:Symbol)` — the latter returns every repo's symbols in shared-Neo4j mode
    // and leaks routes across projects. This honors the backend's repo_filter.
    // Include `Route`-kind symbols (explicit registrations) alongside functions.
    let syms = backend.symbols_with_docstring(Some(&["Function", "Method", "Route"]))?;
    let rows: Vec<Vec<String>> = syms
        .into_iter()
        .map(|s| vec![s.id, s.name, s.kind, s.file, s.docstring])
        .collect();
    Ok(detect_routes_from_rows(&rows))
}

pub fn detect_routes_from_rows(rows: &[Vec<String>]) -> Vec<Route> {
    let mut routes = Vec::new();

    for row in rows {
        let id = &row[0];
        let name = &row[1];
        let kind = &row[2];
        let file = &row[3];
        let docstring = row.get(4).map(|s| s.as_str()).unwrap_or("");

        let route = if kind == "Route" {
            // Explicit registration captured by the extractor: the name is
            // "METHOD /path" (e.g. "GET /api/users"). This is hard evidence — no
            // guessing needed.
            Some(route_from_registration(id, name, file))
        } else {
            detect_route_from_symbol(id, name, file, docstring)
        };

        if let Some(route) = route {
            routes.push(route);
        }
    }

    routes.sort_by(|a, b| a.file.cmp(&b.file).then(a.path.cmp(&b.path)));

    routes
}

/// Build a `Route` from a `Route`-kind symbol whose name is `"METHOD /path"`.
/// Falls back to `UNKNOWN` + the raw name when no leading method is present.
fn route_from_registration(id: &str, name: &str, file: &str) -> Route {
    let (method, path) = match name.split_once(' ') {
        Some((m, p)) => {
            let m = m.trim().to_uppercase();
            // Normalize framework-specific verbs: MapGet -> GET, RESOURCE stays.
            let m = m.strip_prefix("MAP").map(str::to_string).unwrap_or(m);
            (m, p.trim().to_string())
        }
        None => ("UNKNOWN".to_string(), name.to_string()),
    };
    Route {
        method,
        path,
        handler_id: id.to_string(),
        file: file.to_string(),
        framework: helpers::detect_framework_from_file(file),
    }
}

/// Try to detect a route from a single function/method symbol.
///
/// Requires real evidence: either a route decorator/annotation captured in the
/// docstring, or — for Go, where naming *is* the routing mechanism and there is
/// no decorator/registration syntax — a handler naming convention. Name-only
/// guessing is deliberately NOT applied to decorator/registration frameworks
/// (Python/JS/TS/Java/Ruby/PHP/C#/Elixir): those routes come from `Route`-kind
/// symbols or decorator docstrings, so a bare name there is a false positive.
fn detect_route_from_symbol(id: &str, name: &str, file: &str, docstring: &str) -> Option<Route> {
    let name_lower = name.to_lowercase();
    let doc_lower = docstring.to_lowercase();

    let lang = language_from_file(file);

    // Decorator/annotation captured as docstring — strongest per-function signal.
    if let Some(route) = detect_from_docstring(id, name, file, &doc_lower) {
        return Some(route);
    }

    // Naming convention only where it is the actual routing mechanism.
    match lang {
        Lang::Go => detect_go_route(id, name, &name_lower, file, &doc_lower),
        _ => None,
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

    // ── Registration evidence: Route-kind symbols (urls.py, app.get, router) ──

    #[test]
    fn test_route_kind_symbol_parsed_as_method_and_path() {
        let rows = vec![vec![
            "urls.py::api/users/".to_string(),
            "GET /api/users".to_string(),
            "Route".to_string(),
            "urls.py".to_string(),
            String::new(),
        ]];
        let routes = detect_routes_from_rows(&rows);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/api/users");
        assert_eq!(routes[0].handler_id, "urls.py::api/users/");
    }

    #[test]
    fn test_route_kind_symbol_mapget_normalized() {
        // C# minimal-api verbs come through as "MapGet /x".
        let rows = vec![vec![
            "Program.cs::MapGet__health".to_string(),
            "MapGet /health".to_string(),
            "Route".to_string(),
            "Program.cs".to_string(),
            String::new(),
        ]];
        let routes = detect_routes_from_rows(&rows);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/health");
    }

    // ── Decorator evidence: route annotation captured in the docstring ──

    #[test]
    fn test_docstring_decorator_is_a_route() {
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

    // ── Go: naming convention IS the routing mechanism (no decorator syntax) ──

    #[test]
    fn test_go_serve_http_is_a_route() {
        let route = detect_route_from_symbol(
            "server.go::MyHandler::ServeHTTP",
            "ServeHTTP",
            "server.go",
            "",
        );
        assert!(route.is_some());
    }

    #[test]
    fn test_go_handler_suffix_is_a_route() {
        let route = detect_route_from_symbol("api.go::UsersHandler", "UsersHandler", "api.go", "");
        assert!(route.is_some());
        assert!(route.unwrap().path.contains("users"));
    }

    // ── No evidence: bare verb-prefixed names are NOT routes (AIF3X-331) ──

    #[test]
    fn test_undecorated_python_get_prefix_is_not_a_route() {
        // `get_users` with no decorator and no registration must not be a route.
        let route = detect_route_from_symbol("views.py::get_users", "get_users", "views.py", "");
        assert!(route.is_none(), "name-only guess must not produce a route");
    }

    #[test]
    fn test_undecorated_handle_prefix_is_not_a_route() {
        // The report's exact false positive: handle_message -> UNKNOWN /message.
        let route =
            detect_route_from_symbol("svc.py::handle_message", "handle_message", "svc.py", "");
        assert!(route.is_none());
    }

    #[test]
    fn test_bare_js_handler_is_not_a_route() {
        let route =
            detect_route_from_symbol("api/users.ts::handler", "handler", "api/users.ts", "");
        assert!(route.is_none());
    }

    #[test]
    fn test_undecorated_java_controller_method_is_not_a_route() {
        // Without an @GetMapping in the docstring, a bare getUsers is not a route.
        let route = detect_route_from_symbol(
            "UserController.java::UserController::getUsers",
            "getUsers",
            "com/example/controller/UserController.java",
            "",
        );
        assert!(route.is_none());
    }

    #[test]
    fn test_docstring_mentioning_api_without_verb_and_path_is_not_a_route() {
        // Real false positive (AIF3X-331 re-run): a plain helper whose docstring
        // happens to say "...Responses API responses..." was previously matched
        // on the bare substring "api" alone, then the method defaulted to GET
        // with no real evidence at all, producing a fabricated
        // "GET /apply_luhn_check" route for a function with no decorator and no
        // registration.
        let route = detect_route_from_symbol(
            "utils.py::apply_luhn_check",
            "apply_luhn_check",
            "utils.py",
            "apply luhn check to llm response to mask pci data. supports chat \
             completions (modelresponse), gemini (geminichatresponse), and \
             responses api responses.",
        );
        assert!(
            route.is_none(),
            "a bare mention of \"api\" with no decorator, no HTTP verb, and no \
             extractable path must not produce a route, got: {route:?}"
        );
    }

    #[test]
    fn test_docstring_verb_without_path_is_not_a_route() {
        // An HTTP verb word alone (e.g. incidentally in prose) without any
        // extractable /path is still not enough evidence.
        let route = detect_route_from_symbol(
            "svc.py::get_settings",
            "get_settings",
            "svc.py",
            "get the current settings handler for this service.",
        );
        assert!(route.is_none());
    }

    #[test]
    fn test_plain_function_is_not_a_route() {
        let route =
            detect_route_from_symbol("utils.py::format_string", "format_string", "utils.py", "");
        assert!(route.is_none());
    }

    #[test]
    fn test_doxygen_comment_opening_token_is_not_a_route() {
        // Real false positive found in tto-engine-master: a plain C++ header
        // method with an ordinary Doxygen/Javadoc doc comment —
        //   /**
        //    * Post a calc eventMessage
        //    * @param[in] eventMessage - the message describing the event
        //    */
        //   void PostDataChangeEvent(const EventMessage& eventMessage);
        // — has its comment captured verbatim as the docstring. The comment's
        // opening "/**" token reads as an unquoted "/path"-shaped word to a
        // naive starts_with('/') check, and "Post" matches the HTTP-verb
        // keyword scan as a whole word, together fabricating
        // "POST /**" for nearly every documented function in a C++/Java
        // codebase (124 false positives observed in one real repo).
        let route = detect_route_from_symbol(
            "EventProxy.h::EventProxy::PostDataChangeEvent",
            "PostDataChangeEvent",
            "EventProxy.h",
            "/**\n * post a calc eventmessage\n *\n * @param[in] eventmessage - the message describing the event\n */",
        );
        assert!(
            route.is_none(),
            "a Doxygen comment's opening \"/**\" token must not be extracted \
             as a URL path, got: {route:?}"
        );
    }

    // ── Helpers still in use by the Go path / docstring parser ──

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
