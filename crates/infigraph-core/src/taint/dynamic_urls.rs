use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::{FuncInfo, SourceCache};
use crate::graph::GraphBackend;
use crate::routes::Route;

#[derive(Debug, Clone, Serialize)]
pub struct DynamicUrl {
    pub symbol_id: String,
    pub file: String,
    pub line: u32,
    pub url_template: String,
    pub http_client: String,
    pub matched_route: Option<MatchedRoute>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedRoute {
    pub handler_id: String,
    pub method: String,
    pub path: String,
    pub framework: String,
}

static HTTP_CLIENT_PATTERNS: &[(&str, &[&str])] = &[
    ("fetch", &["fetch(", "fetch ("]),
    (
        "axios",
        &[
            "axios.get(",
            "axios.post(",
            "axios.put(",
            "axios.delete(",
            "axios.patch(",
            "axios(",
        ],
    ),
    (
        "requests",
        &[
            "requests.get(",
            "requests.post(",
            "requests.put(",
            "requests.delete(",
            "requests.patch(",
        ],
    ),
    (
        "http_client",
        &[
            "HttpClient(",
            "http.get(",
            "http.post(",
            "http.put(",
            "http.delete(",
        ],
    ),
    ("urllib", &["urllib.request.urlopen(", "urlopen("]),
    (
        "okhttp",
        &["OkHttpClient(", ".newCall(", "Request.Builder()"],
    ),
    (
        "resttemplate",
        &[
            "restTemplate.getForObject(",
            "restTemplate.postForObject(",
            "restTemplate.exchange(",
        ],
    ),
    (
        "webclient",
        &["WebClient.create(", "webClient.get()", "webClient.post()"],
    ),
    (
        "httpclient_dotnet",
        &[
            "HttpClient.GetAsync(",
            "HttpClient.PostAsync(",
            "HttpClient.SendAsync(",
        ],
    ),
    ("net_http", &["http.Get(", "http.Post(", "http.NewRequest("]),
    (
        "reqwest",
        &["reqwest::get(", "reqwest::Client::new(", ".send().await"],
    ),
];

pub fn detect_dynamic_urls(backend: &dyn GraphBackend, root: &Path) -> Result<Vec<DynamicUrl>> {
    let route_rows = backend
        .raw_query(
            "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
         RETURN s.id, s.name, s.kind, s.file, s.docstring",
        )
        .unwrap_or_default();
    let routes = crate::routes::detect_routes_from_rows(&route_rows);

    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND s.file IS NOT NULL RETURN s.id, s.file, s.start_line, s.end_line")?;

    let mut functions: Vec<(String, String, u32, u32)> = Vec::new();
    for row in result {
        if row.len() < 4 {
            continue;
        }
        let id = row[0].to_string();
        let file = row[1].to_string();
        let start: u32 = row[2].to_string().parse().unwrap_or(0);
        let end: u32 = row[3].to_string().parse().unwrap_or(0);
        if start > 0 && end > start {
            functions.push((id, file, start, end));
        }
    }

    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut urls = Vec::new();

    for (symbol_id, file, start_line, end_line) in &functions {
        let lines = file_cache.entry(file.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(file))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        });

        let start_idx = (*start_line as usize).saturating_sub(1);
        let end_idx = (*end_line as usize).min(lines.len());
        if start_idx >= end_idx {
            continue;
        }

        let func_lines = &lines[start_idx..end_idx];
        let detected = find_urls_in_function(symbol_id, file, *start_line, func_lines, &routes);
        urls.extend(detected);
    }

    if !urls.is_empty() {
        write_calls_service_edges(backend, &urls)?;
    }

    Ok(urls)
}

pub fn detect_dynamic_urls_with_cache(
    backend: &dyn GraphBackend,
    functions: &[FuncInfo],
    cache: &SourceCache,
) -> Result<Vec<DynamicUrl>> {
    let route_rows = backend
        .raw_query(
            "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
         RETURN s.id, s.name, s.kind, s.file, s.docstring",
        )
        .unwrap_or_default();
    let routes = crate::routes::detect_routes_from_rows(&route_rows);

    let mut urls = Vec::new();
    for func in functions {
        let lines = match cache.get(&func.file) {
            Some(l) => l,
            None => continue,
        };
        let start_idx = (func.start_line as usize).saturating_sub(1);
        let end_idx = (func.end_line as usize).min(lines.len());
        if start_idx >= end_idx {
            continue;
        }

        let func_lines = &lines[start_idx..end_idx];
        let detected =
            find_urls_in_function(&func.id, &func.file, func.start_line, func_lines, &routes);
        urls.extend(detected);
    }

    if !urls.is_empty() {
        write_calls_service_edges(backend, &urls)?;
    }

    Ok(urls)
}

fn find_urls_in_function(
    symbol_id: &str,
    file: &str,
    base_line: u32,
    lines: &[String],
    routes: &[Route],
) -> Vec<DynamicUrl> {
    let mut urls = Vec::new();
    let mut string_vars: HashMap<String, String> = HashMap::new();

    for (offset, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        let line_no = base_line + offset as u32;

        // Track string variable assignments for constant propagation
        if let Some((var, val)) = extract_string_assignment(trimmed) {
            string_vars.insert(var, val);
        }

        // Check for HTTP client calls
        for &(client, patterns) in HTTP_CLIENT_PATTERNS {
            for &pat in patterns {
                if lower.contains(&pat.to_lowercase()) {
                    if let Some(url) = extract_url_from_line(trimmed, &string_vars) {
                        let template = url_to_template(&url);
                        let matched = match_route(&template, routes);

                        urls.push(DynamicUrl {
                            symbol_id: symbol_id.to_string(),
                            file: file.to_string(),
                            line: line_no,
                            url_template: template,
                            http_client: client.to_string(),
                            matched_route: matched,
                        });
                    }
                    break;
                }
            }
        }
    }

    urls
}

fn extract_string_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let stripped = line
        .strip_prefix("let ")
        .or_else(|| line.strip_prefix("var "))
        .or_else(|| line.strip_prefix("const "))
        .or_else(|| line.strip_prefix("String "))
        .or_else(|| line.strip_prefix("final "))
        .unwrap_or(line);

    if let Some(eq_pos) = stripped.find('=') {
        if eq_pos > 0 && stripped.get(eq_pos + 1..eq_pos + 2) != Some("=") {
            let var = stripped[..eq_pos].split_whitespace().last()?;
            let rhs = stripped[eq_pos + 1..].trim();
            // Only track string literals
            if (rhs.starts_with('"') && rhs.ends_with('"'))
                || (rhs.starts_with('\'') && rhs.ends_with('\''))
                || (rhs.starts_with('`') && rhs.ends_with('`'))
                || rhs.starts_with("f\"")
                || rhs.starts_with("f'")
            {
                let val = rhs.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
                let val = val.strip_prefix("f").unwrap_or(val);
                return Some((var.to_string(), val.to_string()));
            }
        }
    }
    None
}

fn extract_url_from_line(line: &str, vars: &HashMap<String, String>) -> Option<String> {
    // Look for string literals containing URL-like patterns
    let url_indicators = ["http://", "https://", "/api/", "/v1/", "/v2/", "/graphql"];

    // Direct string literal in call
    for delim in ['"', '\'', '`'] {
        let mut search_from = 0;
        while let Some(start) = line[search_from..].find(delim) {
            let abs_start = search_from + start + 1;
            if abs_start >= line.len() {
                break;
            }
            if let Some(end) = line[abs_start..].find(delim) {
                let candidate = &line[abs_start..abs_start + end];
                if url_indicators.iter().any(|ind| candidate.contains(ind))
                    || candidate.starts_with('/')
                {
                    return Some(candidate.to_string());
                }
            }
            search_from = abs_start;
        }
    }

    // Template literals with interpolation: `${base}/api/users/${id}`
    if let Some(start) = line.find('`') {
        if let Some(end) = line[start + 1..].find('`') {
            let template = &line[start + 1..start + 1 + end];
            if url_indicators.iter().any(|ind| template.contains(ind)) || template.starts_with('/')
            {
                return Some(template.to_string());
            }
        }
    }

    // f-string: f"/api/users/{user_id}"
    if let Some(fstart) = line.find("f\"").or_else(|| line.find("f'")) {
        let delim = line.as_bytes()[fstart + 1] as char;
        let inner_start = fstart + 2;
        if let Some(end) = line[inner_start..].find(delim) {
            let template = &line[inner_start..inner_start + end];
            if url_indicators.iter().any(|ind| template.contains(ind)) || template.starts_with('/')
            {
                return Some(template.to_string());
            }
        }
    }

    // String concatenation with known variables
    if line.contains('+') || line.contains("format!(") || line.contains("String.format(") {
        for (var, val) in vars {
            if line.contains(var.as_str()) && (val.contains('/') || val.contains("http")) {
                return Some(val.clone());
            }
        }
    }

    None
}

fn url_to_template(url: &str) -> String {
    let mut template = String::new();
    let mut in_var = false;

    for ch in url.chars() {
        if ch == '{' || ch == '$' {
            if !in_var {
                template.push('{');
                in_var = true;
            }
        } else if in_var && (ch == '}' || ch == '/' || ch == '?' || ch == '&') {
            template.push('}');
            in_var = false;
            if ch != '}' {
                template.push(ch);
            }
        } else if in_var {
            // Skip variable name details
        } else {
            template.push(ch);
        }
    }
    if in_var {
        template.push('}');
    }

    // Normalize: collapse consecutive {}'s
    template.replace("{}", "{id}")
}

fn match_route(template: &str, routes: &[Route]) -> Option<MatchedRoute> {
    let template_path = template.split('?').next().unwrap_or(template);
    let template_path = template_path.split("://").last().unwrap_or(template_path);
    // Strip host if present
    let template_path = if template_path.contains('/') && !template_path.starts_with('/') {
        template_path
            .split_once('/')
            .map(|(_, p)| format!("/{}", p))
            .unwrap_or_else(|| template_path.to_string())
    } else {
        template_path.to_string()
    };

    let template_segments: Vec<&str> = template_path.split('/').filter(|s| !s.is_empty()).collect();

    for route in routes {
        let route_segments: Vec<&str> = route.path.split('/').filter(|s| !s.is_empty()).collect();

        if template_segments.len() != route_segments.len() {
            continue;
        }

        let mut matched = true;
        for (ts, rs) in template_segments.iter().zip(route_segments.iter()) {
            let ts_is_param = ts.starts_with('{') || ts.starts_with(':') || ts.starts_with('<');
            let rs_is_param = rs.starts_with('{') || rs.starts_with(':') || rs.starts_with('<');
            if ts_is_param || rs_is_param {
                continue; // Parameter segments always match
            }
            if ts.to_lowercase() != rs.to_lowercase() {
                matched = false;
                break;
            }
        }

        if matched {
            return Some(MatchedRoute {
                handler_id: route.handler_id.clone(),
                method: route.method.clone(),
                path: route.path.clone(),
                framework: route.framework.clone(),
            });
        }
    }

    None
}

fn write_calls_service_edges(backend: &dyn GraphBackend, urls: &[DynamicUrl]) -> Result<()> {
    backend.raw_query("BEGIN TRANSACTION")?;

    for url in urls {
        if let Some(ref matched) = url.matched_route {
            let src_esc = crate::escape_str(&url.symbol_id);
            let tgt_esc = crate::escape_str(&matched.handler_id);
            let method_esc = crate::escape_str(&matched.method);
            let path_esc = crate::escape_str(&url.url_template);

            let _ = backend.raw_query(&format!(
                "MATCH (s:Symbol), (t:Symbol) WHERE s.id = '{src_esc}' AND t.id = '{tgt_esc}' \
                 CREATE (s)-[:CALLS_SERVICE {{method: '{method_esc}', path: '{path_esc}', target_service: ''}}]->(t)"
            ));
        }
    }

    backend.raw_query("COMMIT")?;

    Ok(())
}

pub fn format_dynamic_urls(urls: &[DynamicUrl]) -> String {
    if urls.is_empty() {
        return "No dynamic URL constructions detected.".to_string();
    }

    let matched_count = urls.iter().filter(|u| u.matched_route.is_some()).count();

    let mut out = format!(
        "Dynamic URLs: {} total ({} matched to routes, {} unmatched)\n\n",
        urls.len(),
        matched_count,
        urls.len() - matched_count
    );

    let mut by_client: std::collections::BTreeMap<&str, Vec<&DynamicUrl>> =
        std::collections::BTreeMap::new();
    for u in urls {
        by_client.entry(&u.http_client).or_default().push(u);
    }

    for (client, items) in &by_client {
        out.push_str(&format!("## {} ({} calls)\n", client, items.len()));
        for u in items {
            out.push_str(&format!("  {}:{} — {}\n", u.file, u.line, u.url_template));
            if let Some(ref m) = u.matched_route {
                out.push_str(&format!(
                    "    -> {} {} ({}) [{}]\n",
                    m.method, m.path, m.handler_id, m.framework
                ));
            }
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_url_from_string_literal() {
        let vars = HashMap::new();
        let line = r#"response = requests.get("https://api.example.com/api/v1/users")"#;
        let url = extract_url_from_line(line, &vars);
        assert!(url.is_some(), "should extract URL");
        assert!(url.unwrap().contains("/api/v1/users"));
    }

    #[test]
    fn test_extract_url_template_literal() {
        let vars = HashMap::new();
        let line = "const res = fetch(`/api/users/${userId}`)";
        let url = extract_url_from_line(line, &vars);
        assert!(url.is_some(), "should extract template URL");
    }

    #[test]
    fn test_extract_url_fstring() {
        let vars = HashMap::new();
        let line = r#"response = requests.get(f"/api/users/{user_id}")"#;
        let url = extract_url_from_line(line, &vars);
        assert!(url.is_some(), "should extract f-string URL");
    }

    #[test]
    fn test_url_to_template() {
        assert_eq!(url_to_template("/api/users/${userId}"), "/api/users/{id}");
        assert_eq!(url_to_template("/api/v1/items"), "/api/v1/items");
    }

    #[test]
    fn test_match_route_exact() {
        let routes = vec![Route {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            handler_id: "app.py::get_users".to_string(),
            file: "app.py".to_string(),
            framework: "flask".to_string(),
        }];
        let matched = match_route("/api/users", &routes);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().handler_id, "app.py::get_users");
    }

    #[test]
    fn test_match_route_with_param() {
        let routes = vec![Route {
            method: "GET".to_string(),
            path: "/api/users/:id".to_string(),
            handler_id: "app.py::get_user".to_string(),
            file: "app.py".to_string(),
            framework: "express".to_string(),
        }];
        let matched = match_route("/api/users/{id}", &routes);
        assert!(matched.is_some());
    }

    #[test]
    fn test_match_route_no_match() {
        let routes = vec![Route {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            handler_id: "app.py::get_users".to_string(),
            file: "app.py".to_string(),
            framework: "flask".to_string(),
        }];
        let matched = match_route("/api/products", &routes);
        assert!(matched.is_none());
    }

    #[test]
    fn test_extract_string_assignment() {
        let (var, val) =
            extract_string_assignment(r#"const base_url = "https://api.example.com""#).unwrap();
        assert_eq!(var, "base_url");
        assert_eq!(val, "https://api.example.com");
    }

    #[test]
    fn test_no_url_in_plain_code() {
        let vars = HashMap::new();
        let line = "x = compute(a, b, c)";
        assert!(extract_url_from_line(line, &vars).is_none());
    }
}
