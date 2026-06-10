mod handlers_analysis;
mod handlers_chat;
mod handlers_git;
mod handlers_symbol;

use std::path::PathBuf;
use std::thread;

use anyhow::Result;
use serde_json::{json, Value};
use tiny_http::{Header, Response, Server};

use infigraph_core::Infigraph;
use infigraph_languages::bundled_registry;

use handlers_analysis::*;
use handlers_chat::api_chat;
use handlers_git::api_git_summary;
use handlers_symbol::*;

/// Start the web UI server on the given port. Runs in a background thread.
pub fn start_ui_server(port: u16) -> bool {
    let addr = format!("0.0.0.0:{}", port);
    // Pre-check: try binding before spawning thread so caller knows outcome
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(_) => return false,
    };
    thread::spawn(move || {
        let server = server;

        for mut request in server.incoming_requests() {
            let url = request.url().to_string();
            let method = request.method().to_string();

            // Strip query string for route matching
            let route = url.split('?').next().unwrap_or(&url);

            let response = match (method.as_str(), route) {
                ("GET", "/") => serve_html(INDEX_HTML, "text/html"),
                ("GET", "/api/health") => serve_json(json!({"status": "ok"})),

                // API endpoints
                ("POST", "/api/index") => handle_api_post(&mut request, api_index),
                ("POST", "/api/search") => handle_api_post(&mut request, api_search),
                ("POST", "/api/query") => handle_api_post(&mut request, api_query),
                ("POST", "/api/architecture") => handle_api_post(&mut request, api_architecture),
                ("POST", "/api/dead-code") => handle_api_post(&mut request, api_dead_code),
                ("POST", "/api/symbols") => handle_api_post(&mut request, api_symbols),
                ("POST", "/api/symbol-context") => {
                    handle_api_post(&mut request, api_symbol_context)
                }
                ("POST", "/api/snippet") => handle_api_post(&mut request, api_snippet),
                ("POST", "/api/graph-data") => handle_api_post(&mut request, api_graph_data),
                ("POST", "/api/stats") => handle_api_post(&mut request, api_stats),
                ("POST", "/api/cluster") => handle_api_post(&mut request, api_cluster),
                ("POST", "/api/chat") => handle_api_post(&mut request, api_chat),
                ("POST", "/api/routes") => handle_api_post(&mut request, api_routes),
                ("POST", "/api/groups") => handle_api_post(&mut request, api_groups),
                ("POST", "/api/contracts") => handle_api_post(&mut request, api_contracts),
                ("POST", "/api/complexity") => handle_api_post(&mut request, api_complexity),
                ("POST", "/api/security") => handle_api_post(&mut request, api_security),
                ("POST", "/api/bridges") => handle_api_post(&mut request, api_bridges),
                ("POST", "/api/clones") => handle_api_post(&mut request, api_clones),
                ("POST", "/api/git-summary") => handle_api_post(&mut request, api_git_summary),

                _ => serve_html("404 Not Found", "text/plain"),
            };

            let _ = request.respond(response);
        }
    });
    true
}

fn serve_html(body: &str, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let data = body.as_bytes().to_vec();
    let header = Header::from_bytes("Content-Type", content_type).unwrap();
    Response::from_data(data).with_header(header)
}

fn serve_json(value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_string(&value).unwrap_or_default();
    let data = body.into_bytes();
    let ct = Header::from_bytes("Content-Type", "application/json").unwrap();
    let cors = Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap();
    Response::from_data(data).with_header(ct).with_header(cors)
}

fn handle_api_post(
    request: &mut tiny_http::Request,
    handler: fn(&Value) -> Value,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let params: Value = serde_json::from_str(&body).unwrap_or(json!({}));
    let result = handler(&params);
    serve_json(result)
}

pub(super) fn open_prism(params: &Value) -> Result<Infigraph> {
    let path = params.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let registry = bundled_registry()?;
    let mut prism = Infigraph::open(&PathBuf::from(path), registry)?;
    prism.init()?;
    Ok(prism)
}

// The full HTML UI -- embedded as a const string
const INDEX_HTML: &str = include_str!("../ui.html");
