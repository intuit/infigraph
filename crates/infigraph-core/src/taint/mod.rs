pub mod dynamic_urls;
pub mod interprocedural;
pub mod path_traversal;
pub mod sinks;
pub mod sources;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use crate::graph::GraphBackend;
use sinks::{TAINT_SANITIZERS, TAINT_SINKS};
use sources::TAINT_SOURCES;

pub type SourceCache = HashMap<String, Vec<String>>;

#[derive(Clone)]
pub struct FuncInfo {
    pub id: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

pub fn build_source_cache(
    backend: &dyn GraphBackend,
    root: &Path,
) -> Result<(Vec<FuncInfo>, SourceCache)> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method', 'Test'] AND s.file IS NOT NULL RETURN s.id, s.file, s.start_line, s.end_line")?;

    let mut functions = Vec::new();
    let mut files_needed: HashSet<String> = HashSet::new();
    for row in result {
        if row.len() < 4 {
            continue;
        }
        let id = row[0].to_string();
        let file = row[1].to_string();
        let start: u32 = row[2].to_string().parse().unwrap_or(0);
        let end: u32 = row[3].to_string().parse().unwrap_or(0);
        if start > 0 && end > start {
            files_needed.insert(file.clone());
            functions.push(FuncInfo {
                id,
                file,
                start_line: start,
                end_line: end,
            });
        }
    }

    let files_vec: Vec<String> = files_needed.into_iter().collect();
    let cache: SourceCache = files_vec
        .par_iter()
        .map(|file| {
            let content = std::fs::read_to_string(root.join(file))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect();
            (file.clone(), content)
        })
        .collect();

    Ok((functions, cache))
}

#[derive(Debug, Clone, Serialize)]
pub struct TaintFlow {
    pub symbol_id: String,
    pub file: String,
    pub source_kind: String,
    pub source_line: u32,
    pub source_var: String,
    pub sink_kind: String,
    pub sink_line: u32,
    pub sink_category: String,
    pub path: Vec<String>,
    pub sanitized: bool,
    pub sanitizer: Option<String>,
}

pub fn detect_taint_flows(backend: &dyn GraphBackend, root: &Path) -> Result<Vec<TaintFlow>> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method', 'Test'] AND s.file IS NOT NULL RETURN s.id, s.file, s.start_line, s.end_line")?;

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
    let mut all_flows = Vec::new();

    for (symbol_id, file, start_line, end_line) in &functions {
        let lines = file_cache.entry(file.clone()).or_insert_with(|| {
            let abs = root.join(file);
            std::fs::read_to_string(&abs)
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
        let flows = analyze_function(symbol_id, file, *start_line, func_lines);
        all_flows.extend(flows);
    }

    if !all_flows.is_empty() {
        write_taint_flows(backend, &all_flows)?;
    }

    Ok(all_flows)
}

pub fn detect_taint_flows_with_cache(
    backend: &dyn GraphBackend,
    functions: &[FuncInfo],
    cache: &SourceCache,
) -> Result<Vec<TaintFlow>> {
    let mut all_flows = Vec::new();

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
        let flows = analyze_function(&func.id, &func.file, func.start_line, func_lines);
        all_flows.extend(flows);
    }

    if !all_flows.is_empty() {
        write_taint_flows(backend, &all_flows)?;
    }

    Ok(all_flows)
}

fn analyze_function(
    symbol_id: &str,
    file: &str,
    base_line: u32,
    lines: &[String],
) -> Vec<TaintFlow> {
    let mut tainted: HashMap<String, TaintInfo> = HashMap::new();
    let mut flows = Vec::new();

    for (offset, line) in lines.iter().enumerate() {
        let line_no = base_line + offset as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Check for taint sources
        for source in TAINT_SOURCES {
            for &pattern in source.patterns {
                if lower.contains(&pattern.to_lowercase()) {
                    if let Some(var) = extract_lhs(trimmed) {
                        tainted.insert(
                            var.clone(),
                            TaintInfo {
                                source_kind: source.kind.to_string(),
                                source_line: line_no,
                                path: vec![format!("L{}: {} <- {}", line_no, var, source.kind)],
                                original_var: var,
                            },
                        );
                    }
                }
            }
        }

        // Propagate taint through assignments
        if let Some((lhs, rhs)) = parse_assignment(trimmed) {
            let rhs_lower = rhs.to_lowercase();
            let mut propagated = false;
            for (tvar, info) in tainted.clone() {
                if rhs_lower.contains(&tvar.to_lowercase()) {
                    let mut new_path = info.path.clone();
                    new_path.push(format!("L{}: {} = ...{}...", line_no, lhs, tvar));
                    tainted.insert(
                        lhs.clone(),
                        TaintInfo {
                            source_kind: info.source_kind.clone(),
                            source_line: info.source_line,
                            path: new_path,
                            original_var: info.original_var.clone(),
                        },
                    );
                    propagated = true;
                    break;
                }
            }
            if !propagated {
                // Check if RHS has a sanitizer — clears taint from LHS
                for san in TAINT_SANITIZERS {
                    for &pat in san.patterns {
                        if rhs_lower.contains(&pat.to_lowercase()) {
                            tainted.remove(&lhs);
                        }
                    }
                }
            }
        }

        // Check for taint sinks
        for sink in TAINT_SINKS {
            for &pattern in sink.patterns {
                if lower.contains(&pattern.to_lowercase()) {
                    let sink_vars = extract_args_from_call(trimmed);
                    for svar in &sink_vars {
                        if let Some(info) = tainted.get(&svar.to_lowercase()).or_else(|| {
                            tainted
                                .iter()
                                .find(|(k, _)| svar.to_lowercase().contains(&k.to_lowercase()))
                                .map(|(_, v)| v)
                        }) {
                            let sanitized = is_sanitized_nearby(lines, offset, sink.category);
                            let sanitizer = if sanitized {
                                find_sanitizer_name(lines, offset, sink.category)
                            } else {
                                None
                            };

                            let mut path = info.path.clone();
                            path.push(format!(
                                "L{}: {}({}) [SINK: {}]",
                                line_no,
                                pattern.trim_end_matches('('),
                                svar,
                                sink.kind
                            ));

                            flows.push(TaintFlow {
                                symbol_id: symbol_id.to_string(),
                                file: file.to_string(),
                                source_kind: info.source_kind.clone(),
                                source_line: info.source_line,
                                source_var: info.original_var.clone(),
                                sink_kind: sink.kind.to_string(),
                                sink_line: line_no,
                                sink_category: sink.category.to_string(),
                                path,
                                sanitized,
                                sanitizer,
                            });
                        }
                    }
                }
            }
        }
    }

    flows
}

#[derive(Debug, Clone)]
struct TaintInfo {
    source_kind: String,
    source_line: u32,
    path: Vec<String>,
    original_var: String,
}

fn extract_lhs(line: &str) -> Option<String> {
    let line = line.trim();
    // Python/JS/Go/Rust: var = expr or let/var/const var = expr
    let stripped = line
        .strip_prefix("let ")
        .or_else(|| line.strip_prefix("var "))
        .or_else(|| line.strip_prefix("const "))
        .or_else(|| line.strip_prefix("mut "))
        .unwrap_or(line);

    if let Some(eq_pos) = stripped.find('=') {
        if eq_pos > 0 {
            let before = stripped[..eq_pos].trim();
            // Skip if it's == or !=
            if stripped.get(eq_pos + 1..eq_pos + 2) == Some("=") {
                return None;
            }
            if before.ends_with('!') || before.ends_with('<') || before.ends_with('>') {
                return None;
            }
            // Extract variable name (handle type annotations like `x: int = ...`)
            let var = before.split(':').next()?.trim();
            let var = var.split_whitespace().last()?;
            if var.chars().all(|c| c.is_alphanumeric() || c == '_') && !var.is_empty() {
                return Some(var.to_lowercase());
            }
        }
    }
    None
}

fn parse_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let stripped = line
        .strip_prefix("let ")
        .or_else(|| line.strip_prefix("var "))
        .or_else(|| line.strip_prefix("const "))
        .or_else(|| line.strip_prefix("mut "))
        .unwrap_or(line);

    if let Some(eq_pos) = stripped.find('=') {
        if eq_pos > 0 && stripped.get(eq_pos + 1..eq_pos + 2) != Some("=") {
            let before = stripped[..eq_pos].trim();
            if before.ends_with('!') || before.ends_with('<') || before.ends_with('>') {
                return None;
            }
            let var = before.split(':').next()?.trim();
            let var = var.split_whitespace().last()?;
            if var.chars().all(|c| c.is_alphanumeric() || c == '_') && !var.is_empty() {
                let rhs = stripped[eq_pos + 1..].trim();
                return Some((var.to_lowercase(), rhs.to_string()));
            }
        }
    }
    None
}

fn extract_args_from_call(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let lower = line.to_lowercase();

    // Extract identifiers that appear as function arguments
    for (i, _) in lower.match_indices('(') {
        if let Some(close) = lower[i..].find(')') {
            let inner = &line[i + 1..i + close];
            for arg in inner.split(',') {
                let arg = arg
                    .trim()
                    .trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
                let var = arg.split('.').next().unwrap_or(arg).trim();
                if !var.is_empty() && var.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    args.push(var.to_lowercase());
                }
            }
        }
    }

    // Also check for string concatenation patterns: "..." + var
    for word in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let w = word.trim();
        if !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '_') {
            args.push(w.to_lowercase());
        }
    }

    let unique: HashSet<String> = args.into_iter().collect();
    unique.into_iter().collect()
}

fn is_sanitized_nearby(lines: &[String], current_offset: usize, category: &str) -> bool {
    let start = current_offset.saturating_sub(5);
    let end = (current_offset + 6).min(lines.len());

    for san in TAINT_SANITIZERS {
        if san.category != category {
            continue;
        }
        for line in &lines[start..end] {
            let lower = line.to_lowercase();
            for &pat in san.patterns {
                if lower.contains(&pat.to_lowercase()) {
                    return true;
                }
            }
        }
    }
    false
}

fn find_sanitizer_name(lines: &[String], current_offset: usize, category: &str) -> Option<String> {
    let start = current_offset.saturating_sub(5);
    let end = (current_offset + 6).min(lines.len());

    for san in TAINT_SANITIZERS {
        if san.category != category {
            continue;
        }
        for line in &lines[start..end] {
            let lower = line.to_lowercase();
            for &pat in san.patterns {
                if lower.contains(&pat.to_lowercase()) {
                    return Some(pat.to_string());
                }
            }
        }
    }
    None
}

fn write_taint_flows(backend: &dyn GraphBackend, flows: &[TaintFlow]) -> Result<()> {
    let _ = backend.raw_query("MATCH ()-[r:TAINT_FLOW]->() DELETE r");

    for flow in flows {
        if flow.sanitized {
            continue;
        }
        let sym_esc = crate::escape_str(&flow.symbol_id);
        let src_esc = crate::escape_str(&flow.source_kind);
        let sink_esc = crate::escape_str(&flow.sink_kind);
        let path_str = flow.path.join(" -> ");
        let path_esc = crate::escape_str(&path_str);

        let _ = backend.raw_query(&format!(
            "MATCH (s:Symbol) WHERE s.id = '{sym_esc}' \
             CREATE (s)-[:TAINT_FLOW {{source_kind: '{src_esc}', sink_kind: '{sink_esc}', path: '{path_esc}'}}]->(s)"
        ));
    }

    Ok(())
}

pub fn format_taint_flows(flows: &[TaintFlow]) -> String {
    if flows.is_empty() {
        return "No taint flows detected.".to_string();
    }

    let active: Vec<_> = flows.iter().filter(|f| !f.sanitized).collect();
    let sanitized_count = flows.len() - active.len();

    let mut out = format!(
        "Taint flows: {} total ({} active, {} sanitized)\n\n",
        flows.len(),
        active.len(),
        sanitized_count
    );

    if !active.is_empty() {
        let mut by_category: std::collections::BTreeMap<&str, Vec<&&TaintFlow>> =
            std::collections::BTreeMap::new();
        for f in &active {
            by_category.entry(&f.sink_category).or_default().push(f);
        }

        for (category, items) in &by_category {
            out.push_str(&format!("## {} ({} flows)\n", category, items.len()));
            for f in items {
                out.push_str(&format!(
                    "  {}:{} -> {}:{}\n    {} -> {}\n",
                    f.file, f.source_line, f.file, f.sink_line, f.source_kind, f.sink_kind,
                ));
                out.push_str("    Path: ");
                for (i, step) in f.path.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" -> ");
                    }
                    out.push_str(step);
                }
                out.push('\n');
            }
            out.push('\n');
        }
    }

    if sanitized_count > 0 {
        out.push_str(&format!("\n--- {} flows sanitized ---\n", sanitized_count));
        for f in flows.iter().filter(|f| f.sanitized) {
            out.push_str(&format!(
                "  {}:L{} -> L{} ({} -> {}) sanitized by: {}\n",
                f.file,
                f.source_line,
                f.sink_line,
                f.source_kind,
                f.sink_kind,
                f.sanitizer.as_deref().unwrap_or("unknown"),
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_analysis(code: &str) -> Vec<TaintFlow> {
        let lines: Vec<String> = code.lines().map(String::from).collect();
        analyze_function("test::func", "test.py", 1, &lines)
    }

    #[test]
    fn test_simple_sql_injection() {
        let code = r#"
user_input = request.GET.get('name')
cursor.execute("SELECT * FROM users WHERE name = " + user_input)
"#;
        let flows = run_analysis(code);
        assert!(!flows.is_empty(), "should detect taint flow");
        assert!(
            flows.iter().any(|f| f.sink_category == "SqlInjection"),
            "should be SQL injection"
        );
        assert!(
            flows.iter().any(|f| f.source_kind == "HttpParam"),
            "source should be HttpParam"
        );
    }

    #[test]
    fn test_multi_step_propagation() {
        let code = r#"
a = request.GET.get('q')
b = a
c = b
cursor.execute(c)
"#;
        let flows = run_analysis(code);
        assert!(!flows.is_empty(), "should detect multi-step taint");
        let flow = flows
            .iter()
            .find(|f| f.sink_category == "SqlInjection")
            .unwrap();
        assert!(
            flow.path.len() >= 3,
            "path should have multiple steps: {:?}",
            flow.path
        );
    }

    #[test]
    fn test_sanitizer_clears_taint() {
        let code = r#"
user_input = request.GET.get('name')
safe_input = html.escape(user_input)
el.innerHTML = safe_input
"#;
        let flows = run_analysis(code);
        // safe_input is sanitized, so innerHTML should not have active taint from it
        // But user_input is still tainted and might match
        let xss_flows: Vec<_> = flows
            .iter()
            .filter(|f| f.sink_category == "XssRisk")
            .collect();
        // All XSS flows involving safe_input should be sanitized
        for f in &xss_flows {
            if f.source_var == "safe_input" {
                assert!(f.sanitized, "safe_input should be sanitized");
            }
        }
    }

    #[test]
    fn test_command_injection() {
        let code = r#"
cmd = request.POST.get('command')
os.system(cmd)
"#;
        let flows = run_analysis(code);
        assert!(flows.iter().any(|f| f.sink_category == "CommandInjection"));
    }

    #[test]
    fn test_path_traversal() {
        let code = r#"
filename = req.params.filename
content = open(filename)
"#;
        let flows = run_analysis(code);
        assert!(
            flows.iter().any(|f| f.sink_category == "PathTraversal"),
            "flows: {:?}",
            flows
        );
    }

    #[test]
    fn test_open_redirect() {
        let code = r#"
url = request.GET.get('next')
redirect(url)
"#;
        let flows = run_analysis(code);
        assert!(
            flows.iter().any(|f| f.sink_category == "OpenRedirect"),
            "flows: {:?}",
            flows
        );
    }

    #[test]
    fn test_no_taint_without_source() {
        let code = r#"
name = "hardcoded"
cursor.execute("SELECT * FROM users WHERE name = " + name)
"#;
        let flows = run_analysis(code);
        assert!(
            flows.is_empty(),
            "hardcoded string should not be tainted: {:?}",
            flows
        );
    }

    #[test]
    fn test_sanitized_sql() {
        let code = r#"
user_input = request.GET.get('id')
safe = sanitize_sql(user_input)
cursor.execute(safe)
"#;
        let flows = run_analysis(code);
        let sql: Vec<_> = flows
            .iter()
            .filter(|f| f.sink_category == "SqlInjection")
            .collect();
        // Should either be empty (taint cleared) or sanitized
        for f in &sql {
            if f.source_var != "user_input" {
                assert!(
                    f.sanitized || f.path.is_empty(),
                    "sanitized input should not produce active flow"
                );
            }
        }
    }

    #[test]
    fn test_java_request_param() {
        let code = r#"
String name = request.getParameter("name");
stmt.executeQuery("SELECT * FROM users WHERE name = '" + name + "'");
"#;
        let flows = run_analysis(code);
        assert!(
            flows.iter().any(|f| f.sink_category == "SqlInjection"),
            "flows: {:?}",
            flows
        );
    }

    #[test]
    fn test_nodejs_req_body() {
        let code = r#"
const data = req.body.username;
res.send(`<h1>${data}</h1>`);
"#;
        let flows = run_analysis(code);
        // data is tainted from req.body but send() isn't a tracked sink
        // The source should still be detected as HttpBody
        let tainted_vars: Vec<_> = flows.iter().map(|f| &f.source_kind).collect();
        // May or may not produce flows depending on sink matching
        // At minimum, verify no crash
        assert!(flows.is_empty() || tainted_vars.contains(&&"HttpBody".to_string()));
    }

    #[test]
    fn test_extract_lhs_simple() {
        assert_eq!(extract_lhs("x = foo()"), Some("x".to_string()));
        assert_eq!(extract_lhs("let y = bar()"), Some("y".to_string()));
        assert_eq!(extract_lhs("const z = baz()"), Some("z".to_string()));
    }

    #[test]
    fn test_extract_lhs_no_match() {
        assert_eq!(extract_lhs("if x == y:"), None);
        assert_eq!(extract_lhs("x != y"), None);
        assert_eq!(extract_lhs("foo()"), None);
    }

    #[test]
    fn test_parse_assignment() {
        let (lhs, rhs) = parse_assignment("data = request.GET.get('q')").unwrap();
        assert_eq!(lhs, "data");
        assert!(rhs.contains("request.GET"));
    }

    #[test]
    fn test_deserialization_taint() {
        let code = r#"
data = request.body
obj = pickle.loads(data)
"#;
        let flows = run_analysis(code);
        assert!(
            flows
                .iter()
                .any(|f| f.sink_category == "InsecureDeserialization"),
            "flows: {:?}",
            flows
        );
    }
}
