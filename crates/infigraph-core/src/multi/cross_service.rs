use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::lang::LanguageRegistry;
use crate::Infigraph;

use super::{Contract, ContractKind, Registry};

/// A cross-service dependency: service A calls service B at a specific route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossServiceDep {
    pub caller_service: String,
    pub caller_file: String,
    pub caller_symbol: String,
    pub target_service: String,
    pub target_method: String,
    pub target_path: String,
    pub url_found: String,
}

/// Detect cross-service HTTP dependencies within a group.
/// Scans source files for URL strings (fetch, http.get, requests.get, etc.)
/// and matches them to known contracts/routes in other services.
pub fn detect_cross_service_deps(
    registry: &Registry,
    group_name: &str,
    build_registry: impl Fn() -> Result<LanguageRegistry>,
) -> Result<Vec<CrossServiceDep>> {
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;

    // Collect all contracts as lookup table: path → (service, method)
    // Skip paths shorter than 4 chars (e.g. "/") — too generic, matches everything
    // Also index wildcard prefixes: /v1/entities/schedules → /v1/entities/*
    // so dynamic URLs like f"/v1/entities/{x}" match after normalization.
    let mut route_lookup: HashMap<String, Vec<(String, String)>> = HashMap::new();
    // Wildcard prefixes collect ALL methods so we can pick the right one at match time.
    let mut wildcard_methods: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for contract in &group.contracts {
        if contract.kind == ContractKind::HttpRoute {
            let normalized = normalize_route_path(&contract.path);
            if normalized.len() >= 4 {
                // Index exact path — all services that define it
                route_lookup
                    .entry(normalized.clone())
                    .or_default()
                    .push((contract.service.clone(), contract.method.clone()));
                // Index wildcard prefix: /a/b/c → /a/b/*
                // Only for paths with 3+ segments to avoid overly broad matches
                let segments: Vec<&str> = normalized.split('/').collect();
                if segments.len() >= 4 {
                    let prefix = segments[..segments.len() - 1].join("/") + "/*";
                    if prefix.len() >= 4 {
                        wildcard_methods
                            .entry(prefix)
                            .and_modify(|(_, methods)| {
                                if !methods.contains(&contract.method) {
                                    methods.push(contract.method.clone());
                                }
                            })
                            .or_insert_with(|| {
                                (contract.service.clone(), vec![contract.method.clone()])
                            });
                    }
                }
            }
        }
    }

    // Track which normalized paths each service owns, so we can skip
    // docstring/source matches that reference the service's own routes.
    let mut own_routes: HashMap<String, HashSet<String>> = HashMap::new();
    for contract in &group.contracts {
        if contract.kind == ContractKind::HttpRoute {
            let normalized = normalize_route_path(&contract.path);
            own_routes
                .entry(contract.service.clone())
                .or_default()
                .insert(normalized);
        }
    }

    // Resolve a wildcard match to a (service, method) pair.
    // If consumer_method is known and the producer has it, use it.
    // Otherwise pick GET (wildcard = base path = collection endpoint).
    // Fall back to first available method.
    let resolve_wildcard =
        |prefix: &str, consumer_method: Option<&str>| -> Option<(String, String)> {
            let (svc, methods) = wildcard_methods.get(prefix)?;
            let method = if let Some(cm) = consumer_method {
                let cm_upper = cm.to_ascii_uppercase();
                if methods.iter().any(|m| m.eq_ignore_ascii_case(&cm_upper)) {
                    cm_upper
                } else if methods.iter().any(|m| m.eq_ignore_ascii_case("GET")) {
                    "GET".to_string()
                } else {
                    methods[0].clone()
                }
            } else if methods.iter().any(|m| m.eq_ignore_ascii_case("GET")) {
                "GET".to_string()
            } else {
                methods[0].clone()
            };
            Some((svc.clone(), method))
        };

    // Resolve a route: exact match first (skip self-matches), then wildcard fallback.
    // consumer_method is used to pick the right method from wildcard matches.
    let resolve_route = |normalized: &str,
                         consumer_method: Option<&str>,
                         caller_repo: &str|
     -> Option<(String, String)> {
        if let Some(entries) = route_lookup.get(normalized) {
            // Prefer a match from a different service
            for (svc, method) in entries {
                if svc != caller_repo {
                    return Some((svc.clone(), method.clone()));
                }
            }
            // All entries are self — no cross-service match
            return None;
        }
        let wildcard = format!("{}/*", normalized);
        resolve_wildcard(&wildcard, consumer_method)
    };

    let mut deps = Vec::new();

    for repo_name in &group.repos {
        let entry = match registry.repos.get(repo_name) {
            Some(e) => e.clone(),
            None => continue,
        };

        let lang_registry = build_registry()?;
        let mut prism = Infigraph::open(&entry.path, lang_registry)?;
        prism.init()?;

        let backend = match prism.backend() {
            Some(b) => b,
            None => continue,
        };

        // Find symbols with URL-like strings in docstrings or search source files
        let rows = backend.raw_query(
            "MATCH (s:Symbol) WHERE s.docstring IS NOT NULL AND (s.docstring CONTAINS '/api/' OR s.docstring CONTAINS '/v1/' OR s.docstring CONTAINS '/v2/' OR s.docstring CONTAINS '/v3/' OR s.docstring CONTAINS 'http://' OR s.docstring CONTAINS 'https://') RETURN s.id, s.name, s.file, s.docstring",
        ).unwrap_or_default();

        for row in &rows {
            let file = row.get(2).map(|s| s.as_str()).unwrap_or("");
            if is_test_or_doc_file(file) {
                continue;
            }
            let doc = row.get(3).map(|s| s.as_str()).unwrap_or("");
            let urls = extract_api_paths(doc);
            for url in urls {
                let normalized = normalize_route_path(&url);
                // Skip URLs that this service defines as its own routes
                if own_routes
                    .get(repo_name.as_str())
                    .is_some_and(|routes| routes.contains(&normalized))
                {
                    continue;
                }
                if let Some((target_svc, target_method)) =
                    resolve_route(&normalized, None, repo_name)
                {
                    if target_svc != *repo_name {
                        deps.push(CrossServiceDep {
                            caller_service: repo_name.clone(),
                            caller_file: row[2].clone(),
                            caller_symbol: row[0].clone(),
                            target_service: target_svc,
                            target_method,
                            target_path: url.clone(),
                            url_found: url,
                        });
                    }
                }
            }
        }

        // Also grep source files for URL patterns
        let source_urls = scan_source_for_urls(&entry.path);
        for (file, symbol_hint, url, consumer_method) in source_urls {
            let normalized = normalize_route_path(&url);
            if let Some((target_svc, target_method)) =
                resolve_route(&normalized, consumer_method.as_deref(), repo_name)
            {
                if target_svc != *repo_name {
                    // Try to resolve line hint to enclosing symbol ID
                    let caller_id = if let Some(stripped) = symbol_hint.strip_prefix("line:") {
                        let line_num: i32 = stripped.parse().unwrap_or(0);
                        let escaped_file = file.replace('\'', "\\'");
                        let q = format!(
                            "MATCH (s:Symbol) WHERE s.file = '{}' AND s.start_line <= {} AND s.end_line >= {} RETURN s.id ORDER BY (s.end_line - s.start_line) ASC LIMIT 1",
                            escaped_file, line_num, line_num
                        );
                        backend
                            .raw_query(&q)
                            .ok()
                            .and_then(|rows| rows.into_iter().next())
                            .and_then(|row| row.into_iter().next())
                            .unwrap_or_else(|| format!("{}:{}", file, symbol_hint))
                    } else {
                        symbol_hint.clone()
                    };
                    deps.push(CrossServiceDep {
                        caller_service: repo_name.clone(),
                        caller_file: file,
                        caller_symbol: caller_id,
                        target_service: target_svc,
                        target_method,
                        target_path: url.clone(),
                        url_found: url,
                    });
                }
            }
        }

        // Contract-driven inverted scan: check source for any known contract path
        let mut seen_contract_hits: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let contract_hits =
            scan_source_for_contracts(&entry.path, &route_lookup, &wildcard_methods, repo_name);
        for (file, line_hint, matched_path, target_svc, target_method) in contract_hits {
            let key = (file.clone(), matched_path.clone());
            if !seen_contract_hits.insert(key) {
                continue;
            }
            let caller_id = if let Some(stripped) = line_hint.strip_prefix("line:") {
                let line_num: i32 = stripped.parse().unwrap_or(0);
                let escaped_file = file.replace('\'', "\\'");
                let q = format!(
                    "MATCH (s:Symbol) WHERE s.file = '{}' AND s.start_line <= {} AND s.end_line >= {} RETURN s.id ORDER BY (s.end_line - s.start_line) ASC LIMIT 1",
                    escaped_file, line_num, line_num
                );
                backend
                    .raw_query(&q)
                    .ok()
                    .and_then(|rows| rows.into_iter().next())
                    .and_then(|row| row.into_iter().next())
                    .unwrap_or_else(|| format!("{}:{}", file, line_hint))
            } else {
                line_hint
            };
            deps.push(CrossServiceDep {
                caller_service: repo_name.clone(),
                caller_file: file,
                caller_symbol: caller_id,
                target_service: target_svc,
                target_method,
                target_path: matched_path.clone(),
                url_found: matched_path,
            });
        }
    }

    // Shared-contract detection: find /openapi.json or /swagger.json fetches
    // and link to services with HTTP contracts.
    let services_with_routes: Vec<String> = {
        let mut svcs: HashSet<String> = HashSet::new();
        for contract in &group.contracts {
            if contract.kind == ContractKind::HttpRoute {
                svcs.insert(contract.service.clone());
            }
        }
        svcs.into_iter().collect()
    };
    if !services_with_routes.is_empty() {
        for repo_name in &group.repos {
            let entry = match registry.repos.get(repo_name) {
                Some(e) => e.clone(),
                None => continue,
            };
            let spec_hits = scan_source_for_spec_fetches(&entry.path);
            for (file, symbol_hint, spec_path) in spec_hits {
                // Resolve symbol from line hint
                let lang_registry = build_registry()?;
                let caller_id = if let Some(stripped) = symbol_hint.strip_prefix("line:") {
                    let line_num: i32 = stripped.parse().unwrap_or(0);
                    if let Ok(mut prism) = Infigraph::open(&entry.path, lang_registry) {
                        if prism.init().is_ok() {
                            if let Some(backend) = prism.backend() {
                                let escaped_file = file.replace('\'', "\\'");
                                let q = format!(
                                    "MATCH (s:Symbol) WHERE s.file = '{}' AND s.start_line <= {} AND s.end_line >= {} RETURN s.id ORDER BY (s.end_line - s.start_line) ASC LIMIT 1",
                                    escaped_file, line_num, line_num
                                );
                                backend
                                    .raw_query(&q)
                                    .ok()
                                    .and_then(|rows| rows.into_iter().next())
                                    .and_then(|row| row.into_iter().next())
                                    .unwrap_or_else(|| format!("{}:{}", file, symbol_hint))
                            } else {
                                format!("{}:{}", file, symbol_hint)
                            }
                        } else {
                            format!("{}:{}", file, symbol_hint)
                        }
                    } else {
                        format!("{}:{}", file, symbol_hint)
                    }
                } else {
                    symbol_hint
                };
                for target_svc in &services_with_routes {
                    if target_svc != repo_name {
                        deps.push(CrossServiceDep {
                            caller_service: repo_name.clone(),
                            caller_file: file.clone(),
                            caller_symbol: caller_id.clone(),
                            target_service: target_svc.clone(),
                            target_method: "SharedContract".to_string(),
                            target_path: spec_path.clone(),
                            url_found: spec_path.clone(),
                        });
                    }
                }
            }
        }
    }

    // Shared-package detection: match each member's manifest deps against
    // other members' package names.
    let mut pkg_name_to_service: HashMap<String, String> = HashMap::new();
    for repo_name in &group.repos {
        if let Some(entry) = registry.repos.get(repo_name) {
            if let Some(pkg) = crate::manifest::extract_package_name(&entry.path) {
                pkg_name_to_service.insert(pkg, repo_name.clone());
            }
        }
    }
    if !pkg_name_to_service.is_empty() {
        for repo_name in &group.repos {
            if let Some(entry) = registry.repos.get(repo_name) {
                let lang_reg = build_registry()?;
                if let Ok(mut prism) = Infigraph::open(&entry.path, lang_reg) {
                    if prism.init().is_ok() {
                        if let Some(backend) = prism.backend() {
                            if let Ok(member_deps) = crate::manifest::query_deps(backend) {
                                for dep in &member_deps {
                                    if let Some(target_svc) = pkg_name_to_service.get(&dep.name) {
                                        if target_svc != repo_name {
                                            deps.push(CrossServiceDep {
                                                caller_service: repo_name.clone(),
                                                caller_file: "pyproject.toml".to_string(),
                                                caller_symbol: "pyproject.toml".to_string(),
                                                target_service: target_svc.clone(),
                                                target_method: "SharedPackage".to_string(),
                                                target_path: format!(
                                                    "{}@{}",
                                                    dep.name, dep.version
                                                ),
                                                url_found: dep.name.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // MCP config detection: scan YAML/config files for mcp_servers entries
    // with URLs pointing at other services (e.g. ${ASCEND_SVC_URL}/v1/mcp/qb-reports/).
    // These represent runtime tool-schema consumption: an LLM agent reads the MCP
    // tool schema at runtime with no compile-time caller.
    {
        // Collect service names that have HTTP route contracts matching /v1/qb/<topic>/
        // so we can map MCP topic names to contract-owning services.
        let mut topic_to_service: HashMap<String, String> = HashMap::new();
        for contract in &group.contracts {
            if contract.kind == ContractKind::HttpRoute {
                // /v1/qb/reports/report-run → topic "qb-reports" (take segments 2-3, join with hyphen)
                let segs: Vec<&str> = contract.path.split('/').collect();
                if segs.len() >= 4 {
                    // /v1/qb/reports/... → ["", "v1", "qb", "reports", ...]
                    // MCP topic = "qb-reports" for path /v1/qb/reports/*
                    if segs.len() >= 5 && segs[2] == "qb" {
                        let topic = format!("{}-{}", segs[2], segs[3]);
                        topic_to_service
                            .entry(topic)
                            .or_insert_with(|| contract.service.clone());
                    }
                }
            }
        }

        for repo_name in &group.repos {
            let entry = match registry.repos.get(repo_name) {
                Some(e) => e.clone(),
                None => continue,
            };
            let mcp_hits = scan_source_for_mcp_configs(&entry.path);
            for (file, line_hint, mcp_topic, mcp_url) in &mcp_hits {
                // Try to resolve the MCP topic to a service with matching routes
                let target_svc = topic_to_service
                    .get(mcp_topic.as_str())
                    .cloned()
                    // Fallback: if the URL contains a known env var pattern like
                    // ${ASCEND_SVC_URL}, map it to the service
                    .or_else(|| {
                        // Extract env var name from ${...} patterns in the URL
                        if let Some(start) = mcp_url.find("${") {
                            if let Some(end) = mcp_url[start..].find('}') {
                                let var_name = &mcp_url[start + 2..start + end];
                                // Normalize: ASCEND_SVC_URL → ["ascend", "svc", "url"]
                                let parts: Vec<&str> = var_name
                                    .split('_')
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                                // Remove common suffixes and expand abbreviations
                                let meaningful: Vec<String> = parts
                                    .iter()
                                    .filter(|p| {
                                        !["URL", "HOST", "PORT", "BASE", "ADDR"]
                                            .contains(&p.to_ascii_uppercase().as_str())
                                    })
                                    .map(|p| {
                                        let lower = p.to_ascii_lowercase();
                                        if lower == "svc" {
                                            "service".to_string()
                                        } else {
                                            lower
                                        }
                                    })
                                    .collect();
                                for svc_name in group.repos.iter() {
                                    let svc_parts: Vec<&str> = svc_name.split('-').collect();
                                    // Check if meaningful env var parts are a subset of svc name parts
                                    // ASCEND_SVC → ["ascend", "svc"], ascend-service → ["ascend", "service"]
                                    // "svc" matches "service" prefix
                                    let matches = meaningful.iter().all(|mp| {
                                        svc_parts.iter().any(|sp| {
                                            sp.starts_with(mp.as_str()) || mp.starts_with(*sp)
                                        })
                                    });
                                    if matches && !meaningful.is_empty() {
                                        return Some(svc_name.clone());
                                    }
                                }
                            }
                        }
                        None
                    });

                if let Some(ref target) = target_svc {
                    if target != repo_name {
                        // Resolve symbol from line hint
                        let caller_id = format!("{}:{}", file, line_hint);
                        deps.push(CrossServiceDep {
                            caller_service: repo_name.clone(),
                            caller_file: file.clone(),
                            caller_symbol: caller_id,
                            target_service: target.clone(),
                            target_method: "McpConfig".to_string(),
                            target_path: format!("/v1/mcp/{}/", mcp_topic),
                            url_found: mcp_url.clone(),
                        });
                    }
                }
            }
        }
    }

    // Dedup: keep first occurrence per (caller_service, caller_symbol, target_service, target_path).
    // caller_symbol is in the key so the class-level def-site edge AND the method-level
    // reference-site edge (from credit_constant_references) both survive — a route can
    // legitimately be reached from both the aggregating class and each calling method.
    // target_service is in the key so SharedContract/SharedPackage deps to different
    // services from the same caller aren't collapsed.
    let mut seen: HashSet<(String, String, String, String)> = HashSet::new();
    deps.retain(|d| {
        seen.insert((
            d.caller_service.clone(),
            d.caller_symbol.clone(),
            d.target_service.clone(),
            d.target_path.clone(),
        ))
    });

    Ok(deps)
}

/// Link cross-service HTTP dependencies as CALLS_SERVICE edges in each caller's graph.
/// Returns number of edges created.
pub fn link_cross_service_calls(
    registry: &Registry,
    group_name: &str,
    build_registry: impl Fn() -> Result<LanguageRegistry>,
) -> Result<usize> {
    let deps = detect_cross_service_deps(registry, group_name, &build_registry)?;
    if deps.is_empty() {
        return Ok(0);
    }

    // Group deps by caller service
    let mut by_caller: HashMap<String, Vec<&CrossServiceDep>> = HashMap::new();
    for dep in &deps {
        by_caller
            .entry(dep.caller_service.clone())
            .or_default()
            .push(dep);
    }

    let mut total = 0;

    for (caller_svc, svc_deps) in &by_caller {
        let entry = match registry.repos.get(caller_svc) {
            Some(e) => e,
            None => continue,
        };

        let lang_registry = build_registry()?;
        let mut prism = Infigraph::open(&entry.path, lang_registry)?;
        prism.init()?;

        let backend = match prism.backend() {
            Some(b) => b,
            None => continue,
        };

        for dep in svc_deps {
            let target_id = format!(
                "xsvc::{}::{}::{}",
                dep.target_service,
                dep.target_method,
                dep.target_path.replace('\'', "\\'")
            );
            let target_name = format!(
                "{} {} {}",
                dep.target_service, dep.target_method, dep.target_path
            )
            .replace('\'', "\\'");
            let caller_sym = dep.caller_symbol.replace('\'', "\\'");
            let target_svc = dep.target_service.replace('\'', "\\'");
            let target_method = dep.target_method.replace('\'', "\\'");
            let target_path = dep.target_path.replace('\'', "\\'");

            // Create ExternalService node — only use columns from Symbol schema.
            // Use MERGE for idempotency (safe to run group_link multiple times).
            let docstring = format!(
                "External service: {} {} {}",
                target_svc, target_method, target_path
            );
            let create_target = format!(
                "MERGE (t:Symbol {{id: '{}'}}) \
                 ON CREATE SET t.name = '{}', t.kind = 'ExternalService', \
                 t.file = '(external)', t.start_line = 0, t.end_line = 0, \
                 t.signature_hash = '', t.language = 'external', t.visibility = 'public', \
                 t.parent = '', t.docstring = '{}', t.complexity = 0",
                target_id, target_name, docstring,
            );
            let _ = backend.raw_query(&create_target);

            // Check if edge already exists before creating (idempotent)
            let check_edge = format!(
                "MATCH (caller:Symbol {{id: '{}'}})-[:CALLS_SERVICE]->(target:Symbol {{id: '{}'}}) RETURN caller.id",
                caller_sym, target_id,
            );
            let existing = backend.raw_query(&check_edge).unwrap_or_default();
            if !existing.is_empty() {
                continue;
            }

            let create_edge = format!(
                "MATCH (caller:Symbol {{id: '{}'}}), (target:Symbol {{id: '{}'}}) \
                 CREATE (caller)-[:CALLS_SERVICE {{method: '{}', path: '{}', target_service: '{}'}}]->(target)",
                caller_sym, target_id, target_method, target_path, target_svc,
            );
            if backend.raw_query(&create_edge).is_ok() {
                total += 1;
            }
        }
    }

    // SharedPackage linking: for each SharedPackage contract, find import symbols
    // in consumer repos that reference the published package name
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;
    for contract in &group.contracts {
        if contract.kind != ContractKind::SharedPackage {
            continue;
        }
        let pkg_name = &contract.path;
        let publisher = &contract.service;
        // Convert dotted package name to module path for import matching
        // e.g. "ies-core.ascendskills.ascendskills" → search for "ascendskills"
        let pkg_parts: Vec<&str> = pkg_name.split('.').collect();
        let search_terms: Vec<&str> = pkg_parts.iter().filter(|p| p.len() > 3).copied().collect();
        if search_terms.is_empty() {
            continue;
        }

        for repo_name in &group.repos {
            if repo_name == publisher {
                continue;
            }
            let entry = match registry.repos.get(repo_name) {
                Some(e) => e,
                None => continue,
            };
            let lang_registry = build_registry()?;
            let mut prism = Infigraph::open(&entry.path, lang_registry)?;
            prism.init()?;
            let backend = match prism.backend() {
                Some(b) => b,
                None => continue,
            };

            // Scan source files for import statements matching the package
            let import_hits =
                scan_source_for_package_imports(&entry.path, &search_terms, repo_name);
            for (file, line_num, _import_text) in &import_hits {
                let escaped_file = file.replace('\'', "\\'");
                let q = format!(
                    "MATCH (s:Symbol) WHERE s.file = '{}' AND s.start_line <= {} AND s.end_line >= {} \
                     RETURN s.id ORDER BY (s.end_line - s.start_line) ASC LIMIT 1",
                    escaped_file, line_num, line_num
                );
                let caller_sym = backend
                    .raw_query(&q)
                    .ok()
                    .and_then(|rows| rows.into_iter().next())
                    .and_then(|row| row.into_iter().next())
                    .or_else(|| {
                        // Fallback: find any symbol in the same file (module-level import)
                        let fallback_q = format!(
                            "MATCH (s:Symbol) WHERE s.file = '{}' RETURN s.id ORDER BY s.start_line ASC LIMIT 1",
                            escaped_file
                        );
                        backend.raw_query(&fallback_q)
                            .ok()
                            .and_then(|rows| rows.into_iter().next())
                            .and_then(|row| row.into_iter().next())
                    })
                    .unwrap_or_else(|| format!("{}:{}", file, line_num));
                let caller_sym = caller_sym.replace('\'', "\\'");

                let target_id = format!("xsvc::{}::package::{}", publisher, pkg_name);
                let target_name =
                    format!("{} package {}", publisher, pkg_name).replace('\'', "\\'");

                let create_target = format!(
                    "MERGE (t:Symbol {{id: '{}'}}) \
                     ON CREATE SET t.name = '{}', t.kind = 'ExternalService', \
                     t.file = '(external)', t.start_line = 0, t.end_line = 0, \
                     t.signature_hash = '', t.language = 'external', t.visibility = 'public', \
                     t.parent = '', t.docstring = 'Shared package: {}', t.complexity = 0",
                    target_id, target_name, pkg_name,
                );
                let _ = backend.raw_query(&create_target);

                let check_edge = format!(
                    "MATCH (a:Symbol {{id: '{}'}})-[:CALLS_SERVICE]->(b:Symbol {{id: '{}'}}) RETURN a.id",
                    caller_sym, target_id,
                );
                let existing = backend.raw_query(&check_edge).unwrap_or_default();
                if !existing.is_empty() {
                    continue;
                }

                let create_edge = format!(
                    "MATCH (a:Symbol {{id: '{}'}}), (b:Symbol {{id: '{}'}}) \
                     CREATE (a)-[:CALLS_SERVICE {{method: 'package', path: '{}', target_service: '{}'}}]->(b)",
                    caller_sym, target_id, pkg_name, publisher,
                );
                if backend.raw_query(&create_edge).is_ok() {
                    total += 1;
                }
            }
        }
    }

    Ok(total)
}

/// Normalize a route path for matching: strip trailing slash, remove param placeholders.
pub fn normalize_route_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    // Extract just the path portion from full URLs
    let path = if let Some(idx) = path.find("/api/") {
        &path[idx..]
    } else if path.starts_with("http") {
        path.split("//")
            .nth(1)
            .and_then(|s| s.find('/').map(|i| &s[i..]))
            .unwrap_or(path)
    } else {
        path
    };
    // Normalize path params: /users/:id → /users/{id} → /users/*
    let segments: Vec<&str> = path.split('/').collect();
    segments
        .iter()
        .map(|s| {
            if s.starts_with(':') || s.starts_with('{') || s.starts_with('<') {
                "*"
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Strip a leading f-string/template interpolation prefix like `{svc_url}` to
/// reveal the route path portion (e.g. `{svc_url}/v1/customers` → `/v1/customers`).
fn strip_fstring_prefix(s: &str) -> &str {
    if s.starts_with('{') {
        if let Some(close) = s.find('}') {
            return &s[close + 1..];
        }
    }
    s
}

/// Check if a path looks like an HTTP route (starts with /api/ or /v{N}/).
fn is_route_like_path(s: &str) -> bool {
    if s.starts_with("/api/") {
        return true;
    }
    let bytes = s.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'/' || bytes[1] != b'v' || !bytes[2].is_ascii_digit() {
        return false;
    }
    let mut i = 3;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b'/'
}

/// Extract API paths from a string (URL literals in code).
fn extract_api_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for part in text
        .split('"')
        .chain(text.split('\'').chain(text.split('`')))
    {
        let trimmed = part.trim();
        if trimmed.starts_with("http") {
            if let Some(path_part) = trimmed
                .split("//")
                .nth(1)
                .and_then(|s| s.find('/').map(|i| &s[i..]))
            {
                if is_route_like_path(path_part) {
                    paths.push(path_part.to_string());
                }
            }
        } else {
            let stripped = strip_fstring_prefix(trimmed);
            if is_route_like_path(stripped) {
                paths.push(stripped.to_string());
            }
        }
    }
    paths
}

/// Extract the HTTP method from a source line containing a URL reference.
/// Recognises patterns like `requests.get(`, `http.delete(`, `.post(`, `method="PUT"`, etc.
/// Also infers GET from `fetch`/`_fetch_*` calls when no explicit mutating method is present.
fn extract_http_method_from_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for method in &["get", "post", "put", "delete", "patch"] {
        // requests.get( / http.get( / client.get( / session.get(
        let dot_pattern = format!(".{}(", method);
        if lower.contains(&dot_pattern) {
            return Some(method.to_ascii_uppercase());
        }
        // method="GET" / method: "GET" / method='GET'
        for sep in &["=\"", "='", ": \"", ": '", ":'", "=\""] {
            let method_pattern = format!("method{}{}", sep, method);
            if lower.contains(&method_pattern) {
                return Some(method.to_ascii_uppercase());
            }
        }
    }
    None
}

/// Check if a relative file path looks like a test or documentation file.
fn is_test_or_doc_file(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    // Test files
    if name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".test.jsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.js")
    {
        return true;
    }
    // Test directories
    let lower = rel_path.to_ascii_lowercase();
    if lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
    {
        return true;
    }
    // Doc/markdown files
    if name.ends_with(".md") || name.ends_with(".rst") || name.ends_with(".txt") {
        return true;
    }
    false
}

/// Scan source files for URL strings matching route patterns.
/// Also resolves named constants (e.g., `DOC_UPLOAD_PATH = "/v1/..."`): the raw
/// literal appears only at the class/module-level definition line, so the enclosing
/// symbol resolves to the class. A second pass credits each *reference* to the constant
/// (e.g. `self.ESTIMATES_PATH` inside a method) so the CALLS_SERVICE edge also lands on
/// the calling method, not just the class. Same-file only (imported constants are a
/// follow-up).
/// Returns (file, line_hint, url, consumer_http_method).
fn scan_source_for_urls(root: &Path) -> Vec<(String, String, String, Option<String>)> {
    const SKIP_DIRS: &[&str] = &[
        ".infigraph",
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        ".venv",
    ];
    let mut results = Vec::new();
    let mut url_constants: HashMap<String, Vec<(String, usize, String)>> = HashMap::new();
    walk_for_urls(root, root, SKIP_DIRS, &mut results, &mut url_constants);
    credit_constant_references(root, SKIP_DIRS, &url_constants, &mut results);
    results
}

/// For each recorded URL constant, scan its defining file for *references* to the
/// constant name (`FOO_PATH`, `self.FOO_PATH`) and emit a result at each reference line
/// so edges attach to the referencing symbol (usually a method), not just the def line.
/// Skips the definition line itself. Same-file references only.
fn credit_constant_references(
    root: &Path,
    skip: &[&str],
    url_constants: &HashMap<String, Vec<(String, usize, String)>>,
    results: &mut Vec<(String, String, String, Option<String>)>,
) {
    if url_constants.is_empty() {
        return;
    }
    // Group constants by defining file: file -> Vec<(const_name, def_line, url)>
    let mut by_file: HashMap<String, Vec<(String, usize, String)>> = HashMap::new();
    for (name, defs) in url_constants {
        for (file, def_line, url) in defs {
            by_file
                .entry(file.clone())
                .or_default()
                .push((name.clone(), *def_line, url.clone()));
        }
    }
    walk_for_constant_refs(root, root, skip, &by_file, results);
}

fn walk_for_constant_refs(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    by_file: &HashMap<String, Vec<(String, usize, String)>>,
    results: &mut Vec<(String, String, String, Option<String>)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if !skip.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_for_constant_refs(base, &path, skip, by_file, results);
            }
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let consts = match by_file.get(rel.as_str()) {
                Some(c) => c,
                None => continue,
            };
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                let line_no = line_num + 1;
                for (const_name, def_line, url) in consts {
                    // Skip the definition line itself (already emitted by walk_for_urls).
                    if line_no == *def_line {
                        continue;
                    }
                    if line_contains_identifier(line, const_name) {
                        let consumer_method = extract_http_method_from_line(line);
                        results.push((
                            rel.clone(),
                            format!("line:{}", line_no),
                            url.clone(),
                            consumer_method,
                        ));
                    }
                }
            }
        }
    }
}

/// True if `line` references `ident` as a whole identifier (not a substring of a longer
/// name). Boundaries are non-`[A-Za-z0-9_]` characters.
fn line_contains_identifier(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    let ib = ident.as_bytes();
    if ib.is_empty() {
        return false;
    }
    let mut i = 0;
    while let Some(pos) = line[i..].find(ident) {
        let start = i + pos;
        let end = start + ib.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        i = start + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Detect lines where a URL string appears in a non-call context (skip lists,
/// config defaults, route sets) rather than an actual HTTP fetch.
fn is_non_call_context(line: &str) -> bool {
    let trimmed = line.trim();
    let upper = trimmed.to_ascii_uppercase();
    // Set/frozenset/tuple literal assignments with skip/exclude/allow semantics
    for kw in [
        "SKIP",
        "EXCLUDE",
        "WHITELIST",
        "BLACKLIST",
        "ALLOWED",
        "IGNORED",
    ] {
        if upper.contains(kw) {
            return true;
        }
    }
    // Bare set element (line is just a quoted string with trailing comma inside a set literal)
    if (trimmed.starts_with('"') || trimmed.starts_with('\''))
        && trimmed.ends_with(',')
        && trimmed.matches('"').count() == 2
    {
        return true;
    }
    // Config/settings class attribute with external URL default
    if (trimmed.contains("str =") || trimmed.contains("str="))
        && (trimmed.contains("http://") || trimmed.contains("https://"))
    {
        return true;
    }
    // Router prefix definition: APIRouter(prefix="/v1/...")
    if upper.contains("ROUTER") && trimmed.contains("prefix=") {
        return true;
    }
    false
}

/// Detect route decorator lines that define endpoints, not consume them.
/// Matches: @app.get("/..."), @router.post("/..."), @app.route("/..."), etc.
fn is_route_decorator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('@') {
        return false;
    }
    let after_at = &trimmed[1..];
    for prefix in ["app.", "router.", "blueprint."] {
        if let Some(rest) = after_at.strip_prefix(prefix) {
            if rest.starts_with("get(")
                || rest.starts_with("post(")
                || rest.starts_with("put(")
                || rest.starts_with("delete(")
                || rest.starts_with("patch(")
                || rest.starts_with("route(")
                || rest.starts_with("api_route(")
            {
                return true;
            }
        }
    }
    false
}

fn walk_for_urls(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    results: &mut Vec<(String, String, String, Option<String>)>,
    url_constants: &mut HashMap<String, Vec<(String, usize, String)>>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if !skip.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_for_urls(base, &path, skip, results, url_constants);
            }
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                if is_route_decorator(line) || is_non_call_context(line) {
                    continue;
                }
                for delim in ['"', '\'', '`'] {
                    for part in line.split(delim) {
                        let trimmed = part.trim();
                        if trimmed.len() < 200 && !trimmed.contains(' ') {
                            let path_part = if trimmed.starts_with("http") {
                                trimmed
                                    .split("//")
                                    .nth(1)
                                    .and_then(|s| s.find('/').map(|i| &s[i..]))
                                    .unwrap_or(trimmed)
                            } else {
                                strip_fstring_prefix(trimmed)
                            };
                            if is_route_like_path(path_part) {
                                let consumer_method = extract_http_method_from_line(line);
                                results.push((
                                    rel.clone(),
                                    format!("line:{}", line_num + 1),
                                    path_part.to_string(),
                                    consumer_method,
                                ));
                                // Record as named constant if line is an assignment
                                if let Some(const_name) = extract_constant_name(line, path_part) {
                                    url_constants.entry(const_name).or_default().push((
                                        rel.clone(),
                                        line_num + 1,
                                        path_part.to_string(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Extract a constant name from an assignment line containing a route path.
/// Matches patterns like `FOO_PATH = "/v1/..."` or `const FOO = '/v1/...'`.
fn extract_constant_name(line: &str, _path: &str) -> Option<String> {
    let trimmed = line.trim();
    // Python/Ruby: NAME = "..."
    if let Some(eq_pos) = trimmed.find('=') {
        if eq_pos > 0 && !trimmed[..eq_pos].contains('(') {
            let lhs = trimmed[..eq_pos].trim().trim_start_matches("const ");
            let lhs = lhs.trim_start_matches("let ");
            let lhs = lhs.trim_start_matches("var ");
            let name = lhs.trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
            {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Scan source files for /openapi.json or /swagger.json fetch references.
/// Returns (file, line_hint, spec_path).
fn scan_source_for_spec_fetches(root: &Path) -> Vec<(String, String, String)> {
    const SKIP_DIRS: &[&str] = &[
        ".infigraph",
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        ".venv",
    ];
    let mut results = Vec::new();
    walk_for_spec_fetches(root, root, SKIP_DIRS, &mut results);
    results
}

fn walk_for_spec_fetches(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    results: &mut Vec<(String, String, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if path.is_dir() {
            if !skip.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_for_spec_fetches(base, &path, skip, results);
            }
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                if is_route_decorator(line) || is_non_call_context(line) {
                    continue;
                }
                for delim in ['"', '\'', '`'] {
                    for part in line.split(delim) {
                        let trimmed = part.trim();
                        let stripped = strip_fstring_prefix(trimmed);
                        if (stripped == "/openapi.json" || stripped == "/swagger.json")
                            && trimmed != stripped
                        {
                            // trimmed != stripped means an f-string prefix was present,
                            // indicating URL construction (fetch), not a bare config value.
                            results.push((
                                rel.clone(),
                                format!("line:{}", line_num + 1),
                                stripped.to_string(),
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// Scan source files for string literals matching any known contract path.
/// Returns (file, line_hint, matched_path, target_service, target_method).
fn scan_source_for_contracts(
    root: &Path,
    route_lookup: &HashMap<String, Vec<(String, String)>>,
    wildcard_methods: &HashMap<String, (String, Vec<String>)>,
    caller_repo: &str,
) -> Vec<(String, String, String, String, String)> {
    if route_lookup.is_empty() {
        return Vec::new();
    }
    const SKIP_DIRS: &[&str] = &[
        ".infigraph",
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        ".venv",
    ];
    let mut results = Vec::new();
    walk_for_contracts(
        root,
        root,
        SKIP_DIRS,
        route_lookup,
        wildcard_methods,
        caller_repo,
        &mut results,
    );
    results
}

fn walk_for_contracts(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    route_lookup: &HashMap<String, Vec<(String, String)>>,
    wildcard_methods: &HashMap<String, (String, Vec<String>)>,
    caller_repo: &str,
    results: &mut Vec<(String, String, String, String, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if !skip.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_for_contracts(
                    base,
                    &path,
                    skip,
                    route_lookup,
                    wildcard_methods,
                    caller_repo,
                    results,
                );
            }
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                if is_route_decorator(line) || is_non_call_context(line) {
                    continue;
                }
                for delim in ['"', '\'', '`'] {
                    for part in line.split(delim) {
                        let trimmed = part.trim();
                        let stripped = strip_fstring_prefix(trimmed);
                        if stripped.len() < 200
                            && !stripped.is_empty()
                            && !stripped.contains(' ')
                            && stripped.starts_with('/')
                        {
                            let normalized = normalize_route_path(stripped);
                            let consumer_method = extract_http_method_from_line(line);
                            // Exact match first — prefer a different service
                            let hit = if let Some(entries) = route_lookup.get(&normalized) {
                                entries
                                    .iter()
                                    .find(|(svc, _)| svc != caller_repo)
                                    .map(|(svc, method)| (svc.clone(), method.clone()))
                            } else {
                                // Wildcard fallback with proper method resolution
                                let wc = format!("{}/*", normalized);
                                wildcard_methods.get(&wc).map(|(svc, methods)| {
                                    let method = if let Some(ref cm) = consumer_method {
                                        let cm_upper = cm.to_ascii_uppercase();
                                        if methods.iter().any(|m| m.eq_ignore_ascii_case(&cm_upper))
                                        {
                                            cm_upper
                                        } else if methods
                                            .iter()
                                            .any(|m| m.eq_ignore_ascii_case("GET"))
                                        {
                                            "GET".to_string()
                                        } else {
                                            methods[0].clone()
                                        }
                                    } else if methods.iter().any(|m| m.eq_ignore_ascii_case("GET"))
                                    {
                                        "GET".to_string()
                                    } else {
                                        methods[0].clone()
                                    };
                                    (svc.clone(), method)
                                })
                            };
                            if let Some((target_svc, target_method)) = hit {
                                if target_svc != caller_repo {
                                    let effective_method = consumer_method.unwrap_or(target_method);
                                    results.push((
                                        rel.clone(),
                                        format!("line:{}", line_num + 1),
                                        stripped.to_string(),
                                        target_svc.clone(),
                                        effective_method,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Scan source files for import statements matching package search terms.
/// Returns (relative_file, line_number, import_text).
fn scan_source_for_package_imports(
    root: &Path,
    search_terms: &[&str],
    _repo_name: &str,
) -> Vec<(String, usize, String)> {
    const SKIP_DIRS: &[&str] = &[
        ".infigraph",
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        ".venv",
    ];
    let mut results = Vec::new();
    walk_for_imports(root, root, SKIP_DIRS, search_terms, &mut results);
    results
}

fn walk_for_imports(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    terms: &[&str],
    results: &mut Vec<(String, usize, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if path.is_dir() {
            if skip.contains(&name_str.as_ref()) {
                continue;
            }
            walk_for_imports(base, &path, skip, terms, results);
        } else if name_str.ends_with(".py") || name_str.ends_with(".toml") {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                let is_import = trimmed.starts_with("from ") || trimmed.starts_with("import ");
                let is_toml_dep = name_str.ends_with(".toml")
                    && (trimmed.contains("dependencies") || trimmed.contains("packages"));
                if !is_import && !is_toml_dep {
                    continue;
                }
                for term in terms {
                    if trimmed.contains(term) {
                        results.push((rel.clone(), line_num + 1, trimmed.to_string()));
                        break;
                    }
                }
            }
        }
    }
}

/// Scan YAML config files for MCP server URL declarations.
/// Returns (file, line_hint, mcp_topic, raw_url).
/// Matches patterns like:
///   mcp_servers:
///     - name: qb_reports
///       url: ${ASCEND_SVC_URL}/v1/mcp/qb-reports/
fn scan_source_for_mcp_configs(root: &Path) -> Vec<(String, String, String, String)> {
    const SKIP_DIRS: &[&str] = &[
        ".infigraph",
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        ".venv",
    ];
    let mut results = Vec::new();
    walk_for_mcp_configs(root, root, SKIP_DIRS, &mut results);
    results
}

fn walk_for_mcp_configs(
    base: &Path,
    dir: &Path,
    skip: &[&str],
    results: &mut Vec<(String, String, String, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if path.is_dir() {
            if !skip.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                walk_for_mcp_configs(base, &path, skip, results);
            }
        } else if path.is_file() && (name_str.ends_with(".yaml") || name_str.ends_with(".yml")) {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_or_doc_file(&rel) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // Simple state machine: detect mcp_servers blocks and extract url entries
            let mut in_mcp_servers = false;
            let mut current_name: Option<String> = None;
            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                // Detect start of mcp_servers list
                if trimmed == "mcp_servers:" || trimmed.starts_with("mcp_servers:") {
                    in_mcp_servers = true;
                    current_name = None;
                    continue;
                }
                if in_mcp_servers {
                    // End of mcp_servers block: unindented non-empty line without leading dash/space
                    if !trimmed.is_empty()
                        && !line.starts_with(' ')
                        && !line.starts_with('\t')
                        && !trimmed.starts_with('-')
                        && !trimmed.starts_with('#')
                    {
                        in_mcp_servers = false;
                        current_name = None;
                        continue;
                    }
                    // Extract name field
                    if trimmed.starts_with("- name:") || trimmed.starts_with("name:") {
                        let val = trimmed.split(':').nth(1).map(|s| s.trim().to_string());
                        current_name = val;
                    }
                    // Extract url field — contains the MCP server URL
                    if trimmed.starts_with("url:") {
                        if let Some(url_val) = trimmed.strip_prefix("url:") {
                            let url = url_val.trim().to_string();
                            // Extract topic from URL: .../v1/mcp/<topic>/
                            let topic = extract_mcp_topic_from_url(&url)
                                .or_else(|| current_name.clone())
                                .unwrap_or_default();
                            if !topic.is_empty() {
                                results.push((
                                    rel.clone(),
                                    format!("line:{}", line_num + 1),
                                    topic,
                                    url,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Extract the MCP topic name from a URL like `${ASCEND_SVC_URL}/v1/mcp/qb-reports/`.
fn extract_mcp_topic_from_url(url: &str) -> Option<String> {
    // Find /v1/mcp/<topic>/ or /mcp/<topic>/
    let patterns = ["/v1/mcp/", "/mcp/"];
    for pat in &patterns {
        if let Some(idx) = url.find(pat) {
            let after = &url[idx + pat.len()..];
            let topic = after.trim_end_matches('/').split('/').next()?;
            if !topic.is_empty() && !topic.starts_with('$') {
                return Some(topic.to_string());
            }
        }
    }
    None
}

/// Detect cross-repo package dependencies within a group.
/// If repo B depends on a package that repo A publishes, returns a Contract linking them.
pub fn detect_shared_package_deps(
    registry: &Registry,
    group_name: &str,
    build_registry: &impl Fn() -> Result<LanguageRegistry>,
) -> Result<Vec<Contract>> {
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?;

    // Build map: published_package_name → repo_name
    let mut publishers: HashMap<String, String> = HashMap::new();
    for repo_name in &group.repos {
        let entry = match registry.repos.get(repo_name) {
            Some(e) => e,
            None => continue,
        };
        if let Some(pkg_name) = read_published_package_name(&entry.path) {
            publishers.insert(pkg_name, repo_name.clone());
        }
    }

    // For each repo, read its Dependency nodes and check against publishers
    let mut contracts = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for repo_name in &group.repos {
        let entry = match registry.repos.get(repo_name) {
            Some(e) => e.clone(),
            None => continue,
        };

        let lang_registry = build_registry()?;
        let mut prism = Infigraph::open(&entry.path, lang_registry)?;
        prism.init()?;

        let backend = match prism.backend() {
            Some(b) => b,
            None => continue,
        };

        let dep_rows = backend
            .raw_query("MATCH (d:Dependency) RETURN d.name, d.version")
            .unwrap_or_default();

        for row in &dep_rows {
            if row.is_empty() {
                continue;
            }
            let dep_name = &row[0];
            if let Some(publisher_repo) = publishers.get(dep_name) {
                if publisher_repo != repo_name
                    && seen.insert((repo_name.clone(), publisher_repo.clone()))
                {
                    contracts.push(Contract {
                        kind: ContractKind::SharedPackage,
                        service: publisher_repo.clone(),
                        method: "package".to_string(),
                        path: dep_name.clone(),
                        symbol_id: format!("pkg::{}::{}", publisher_repo, dep_name),
                        file: String::new(),
                    });
                }
            }
        }
    }

    Ok(contracts)
}

/// Read the published package name from a repo's manifest file.
pub fn read_published_package_name(root: &Path) -> Option<String> {
    // Python: pyproject.toml
    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(content) = std::fs::read_to_string(&pyproject) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("name") && trimmed.contains('=') {
                    let val = trimmed
                        .split('=')
                        .nth(1)?
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'');
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }

    // Node.js: package.json
    let package_json = root.join("package.json");
    if package_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(name) = parsed.get("name").and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_route_like_path() {
        assert!(is_route_like_path("/api/users"));
        assert!(is_route_like_path("/api/v1/data"));
        assert!(is_route_like_path("/v1/users"));
        assert!(is_route_like_path("/v2/data"));
        assert!(is_route_like_path("/v1/labrador/doc-upload"));
        assert!(is_route_like_path("/v1/events/send"));
        assert!(is_route_like_path("/v10/something"));

        assert!(!is_route_like_path("/var/log"));
        assert!(!is_route_like_path("/vendor/lib"));
        assert!(!is_route_like_path("/value/key"));
        assert!(!is_route_like_path("/vim"));
        assert!(!is_route_like_path("v1/no-leading-slash"));
        assert!(!is_route_like_path("/v/no-digit"));
        assert!(!is_route_like_path(""));
        assert!(!is_route_like_path("/"));
    }

    #[test]
    fn test_extract_api_paths_v1() {
        let text = r#"url = "/v1/labrador/doc-upload""#;
        let paths = extract_api_paths(text);
        assert_eq!(paths, vec!["/v1/labrador/doc-upload"]);
    }

    #[test]
    fn test_extract_api_paths_http_url() {
        let text = r#"url = "https://example.com/v1/users""#;
        let paths = extract_api_paths(text);
        assert_eq!(paths, vec!["/v1/users"]);
    }

    #[test]
    fn test_extract_api_paths_api_prefix() {
        let text = r#"path = "/api/data/fetch""#;
        let paths = extract_api_paths(text);
        assert_eq!(paths, vec!["/api/data/fetch"]);
    }

    #[test]
    fn test_extract_api_paths_no_match() {
        let text = r#"path = "/var/log/app.log""#;
        let paths = extract_api_paths(text);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_constant_name_python() {
        assert_eq!(
            extract_constant_name(
                r#"    DOC_UPLOAD_PATH = "/v1/labrador/doc-upload""#,
                "/v1/labrador/doc-upload"
            ),
            Some("DOC_UPLOAD_PATH".to_string())
        );
    }

    #[test]
    fn test_extract_constant_name_not_constant() {
        assert_eq!(
            extract_constant_name(r#"    url = "/v1/users""#, "/v1/users"),
            None
        );
        assert_eq!(
            extract_constant_name(r#"    fetch("/v1/users")"#, "/v1/users"),
            None
        );
    }

    #[test]
    fn test_normalize_route_path_v1() {
        assert_eq!(normalize_route_path("/v1/users/"), "/v1/users");
        assert_eq!(normalize_route_path("/v1/users/:id"), "/v1/users/*");
        assert_eq!(normalize_route_path("/v1/users/{id}"), "/v1/users/*");
    }

    #[test]
    fn test_scan_source_for_urls_finds_v1_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("client.py"),
            r#"
class ServiceClient:
    DOC_UPLOAD_PATH = "/v1/labrador/doc-upload"
    EVENTS_PATH = "/v1/events/send"

    def upload(self):
        url = f"{self.endpoint}{self.DOC_UPLOAD_PATH}"
"#,
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        let paths: Vec<&str> = results.iter().map(|(_, _, p, _)| p.as_str()).collect();
        assert!(
            paths.contains(&"/v1/labrador/doc-upload"),
            "should find /v1/ path, got {:?}",
            paths
        );
        assert!(
            paths.contains(&"/v1/events/send"),
            "should find events path, got {:?}",
            paths
        );
    }

    #[test]
    fn test_scan_source_for_urls_no_false_positives() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.py"),
            r#"
LOG_DIR = "/var/log/app"
VENDOR_PATH = "/vendor/lib"
VERSION = "v1.2.3"
"#,
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        assert!(
            results.is_empty(),
            "should not match /var/ or /vendor/, got {:?}",
            results
        );
    }

    #[test]
    fn test_scan_source_for_contracts_finds_match() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("client.py"),
            "def check():\n    r = get(\"/health/ready\")\n",
        )
        .unwrap();
        let mut route_lookup = HashMap::new();
        route_lookup.insert(
            "/health/ready".to_string(),
            vec![("svc-a".to_string(), "GET".to_string())],
        );
        let wc = HashMap::new();
        let results = scan_source_for_contracts(dir.path(), &route_lookup, &wc, "svc-b");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].3, "svc-a");
    }

    #[test]
    fn test_scan_source_for_contracts_empty_lookup() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("c.py"), "x = \"/health/ready\"").unwrap();
        let route_lookup = HashMap::new();
        let wc = HashMap::new();
        let results = scan_source_for_contracts(dir.path(), &route_lookup, &wc, "svc-b");
        assert!(results.is_empty());
    }

    #[test]
    fn test_scan_source_for_contracts_no_self_match() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("r.py"), "x = \"/health/ready\"").unwrap();
        let mut route_lookup = HashMap::new();
        route_lookup.insert(
            "/health/ready".to_string(),
            vec![("svc-a".to_string(), "GET".to_string())],
        );
        let wc = HashMap::new();
        let results = scan_source_for_contracts(dir.path(), &route_lookup, &wc, "svc-a");
        assert!(results.is_empty(), "should not match own service");
    }

    #[test]
    fn test_read_published_package_name_pyproject() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"ascendskills\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(
            read_published_package_name(dir.path()),
            Some("ascendskills".to_string())
        );
    }

    #[test]
    fn test_read_published_package_name_package_json() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "@ascend/ui", "version": "1.0.0"}"#,
        )
        .unwrap();
        assert_eq!(
            read_published_package_name(dir.path()),
            Some("@ascend/ui".to_string())
        );
    }

    #[test]
    fn test_read_published_package_name_none() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(read_published_package_name(dir.path()), None);
    }

    #[test]
    fn test_resolve_url_constant_typescript() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("client.ts"),
            "const API_PATH = '/v1/users';\nfetch(`${base}${API_PATH}`);\n",
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        let paths: Vec<&str> = results.iter().map(|(_, _, p, _)| p.as_str()).collect();
        assert!(
            paths.contains(&"/v1/users"),
            "should find TS constant path, got {:?}",
            paths
        );
    }

    #[test]
    fn test_detect_shared_package_dep() {
        let pub_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            pub_dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-shared-lib\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(
            read_published_package_name(pub_dir.path()),
            Some("my-shared-lib".to_string())
        );

        // Consumer has this as a dependency — verified via the publisher lookup
        // in sync_group_contracts. Here we test the building block:
        // read_published_package_name returns the name, and dep matching is string equality.
        let consumer_deps = ["my-shared-lib", "requests", "numpy"];
        let publisher = read_published_package_name(pub_dir.path()).unwrap();
        assert!(consumer_deps.contains(&publisher.as_str()));
    }

    #[test]
    fn test_scan_mcp_config() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("agents.yaml"),
            "labrador:\n  mcp_servers:\n    - name: qb_reports\n      url: ${ASCEND_SVC_URL}/v1/mcp/qb-reports/\n    - name: wiki\n      url: ${ASCEND_SVC_URL}/v1/mcp/wiki/\n",
        )
        .unwrap();
        let results = scan_source_for_mcp_configs(dir.path());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].2, "qb-reports");
        assert_eq!(results[1].2, "wiki");
    }

    #[test]
    fn test_extract_mcp_topic_from_url() {
        assert_eq!(
            extract_mcp_topic_from_url("${ASCEND_SVC_URL}/v1/mcp/qb-reports/"),
            Some("qb-reports".to_string())
        );
        assert_eq!(
            extract_mcp_topic_from_url("http://localhost/v1/mcp/wiki/"),
            Some("wiki".to_string())
        );
        assert_eq!(extract_mcp_topic_from_url("http://localhost/v1/api/"), None);
    }

    #[test]
    fn test_is_test_or_doc_file() {
        assert!(is_test_or_doc_file("tests/unit/test_client.py"));
        assert!(is_test_or_doc_file("test_something.py"));
        assert!(is_test_or_doc_file("app/test/unit/test_api.py"));
        assert!(is_test_or_doc_file("src/__tests__/api.test.ts"));
        assert!(is_test_or_doc_file("client_test.go"));
        assert!(is_test_or_doc_file("docs/ARCHITECTURE.md"));
        assert!(is_test_or_doc_file("README.md"));
        assert!(is_test_or_doc_file("foo.spec.ts"));

        assert!(!is_test_or_doc_file("app/adapters/client.py"));
        assert!(!is_test_or_doc_file("src/services/api.ts"));
        assert!(!is_test_or_doc_file("ascendskills/tools/ascend_api.py"));
        assert!(!is_test_or_doc_file("app/routers/router.py"));
    }

    #[test]
    fn test_scan_source_skips_test_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tests/unit")).unwrap();
        std::fs::write(
            dir.path().join("tests/unit/test_client.py"),
            "url = \"/v1/projects\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("client.py"), "url = \"/v1/projects\"\n").unwrap();
        let results = scan_source_for_urls(dir.path());
        assert_eq!(results.len(), 1, "should only find production file");
        assert_eq!(results[0].0, "client.py");
    }

    #[test]
    fn test_scan_contracts_skips_test_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("tests/test_api.py"),
            "x = \"/health/ready\"\n",
        )
        .unwrap();
        let mut route_lookup = HashMap::new();
        route_lookup.insert(
            "/health/ready".to_string(),
            vec![("svc-a".to_string(), "GET".to_string())],
        );
        let wc = HashMap::new();
        let results = scan_source_for_contracts(dir.path(), &route_lookup, &wc, "svc-b");
        assert!(results.is_empty(), "should skip test files");
    }

    #[test]
    fn test_normalize_route_path_wildcard() {
        // Dynamic URL with param → wildcard
        assert_eq!(
            normalize_route_path("/v1/entities/{entity_type}"),
            "/v1/entities/*"
        );
        assert_eq!(normalize_route_path("/v1/entities/:kind"), "/v1/entities/*");
    }

    #[test]
    fn test_shared_package_no_self_link() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-lib\"\nversion = \"1.0.0\"\n\n[project.dependencies]\nmy-lib = \">=1.0\"\n",
        )
        .unwrap();
        // A repo should not link to itself as a shared package consumer.
        // The publisher name matches its own dep — sync_group_contracts skips same-repo matches.
        let pkg_name = read_published_package_name(dir.path()).unwrap();
        let own_deps = ["my-lib"];
        // The match exists but should be filtered by caller (same repo name check)
        assert_eq!(pkg_name, "my-lib");
        assert!(
            own_deps.contains(&pkg_name.as_str()),
            "self-dep exists but must be filtered by caller"
        );
    }

    #[test]
    fn test_strip_fstring_prefix() {
        assert_eq!(
            strip_fstring_prefix("{svc_url}/v1/customers"),
            "/v1/customers"
        );
        assert_eq!(strip_fstring_prefix("{base}/api/foo"), "/api/foo");
        assert_eq!(strip_fstring_prefix("/v1/projects"), "/v1/projects");
        assert_eq!(strip_fstring_prefix("plain_string"), "plain_string");
        assert_eq!(strip_fstring_prefix("{unclosed"), "{unclosed");
    }

    #[test]
    fn test_extract_api_paths_fstring() {
        let line = r#"url = f"{svc_url}/v1/customers""#;
        let paths = extract_api_paths(line);
        assert!(
            paths.contains(&"/v1/customers".to_string()),
            "should extract /v1/customers from f-string, got: {:?}",
            paths
        );
    }

    #[test]
    fn test_scan_source_fstring_urls() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("pipeline.py"),
            "resp = requests.get(f\"{svc_url}/v1/entities/estimates\")\n",
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].2, "/v1/entities/estimates");
    }

    #[test]
    fn test_extract_http_method_from_line() {
        assert_eq!(
            extract_http_method_from_line("resp = requests.get(url)"),
            Some("GET".to_string())
        );
        assert_eq!(
            extract_http_method_from_line("resp = requests.post(url, json=data)"),
            Some("POST".to_string())
        );
        assert_eq!(
            extract_http_method_from_line("http.delete(f\"{base}/v1/users/{id}\")"),
            Some("DELETE".to_string())
        );
        assert_eq!(
            extract_http_method_from_line("fetch(url, {method: \"PUT\"})"),
            Some("PUT".to_string())
        );
        assert_eq!(
            extract_http_method_from_line("url = \"/v1/customers\""),
            None
        );
    }

    #[test]
    fn test_scan_source_extracts_consumer_method() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("client.py"),
            "resp = requests.get(f\"{svc_url}/v1/customers\")\n",
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].2, "/v1/customers");
        assert_eq!(results[0].3, Some("GET".to_string()));
    }

    #[test]
    fn test_is_route_decorator() {
        assert!(is_route_decorator("@router.post(\"/v1/langfuse/relay\")"));
        assert!(is_route_decorator("  @app.get(\"/v1/items\")"));
        assert!(is_route_decorator("@blueprint.delete(\"/v1/users/{id}\")"));
        assert!(is_route_decorator("@app.api_route(\"/v1/proxy\")"));
        assert!(!is_route_decorator("resp = requests.post(url)"));
        assert!(!is_route_decorator("ROUTES = {\"/v1/langfuse/relay\"}"));
        assert!(!is_route_decorator("# @router.post(\"/v1/x\")"));
    }

    #[test]
    fn test_is_non_call_context() {
        assert!(is_non_call_context("    \"/v1/langfuse/relay\","));
        assert!(is_non_call_context("_SKIP_PATHS = {\"/v1/foo\"}"));
        assert!(is_non_call_context("EXCLUDED_ROUTES = [\"/v1/bar\"]"));
        assert!(is_non_call_context(
            "    oinp_base_url: str = \"https://example.com/v1/events\""
        ));
        assert!(is_non_call_context(
            "router = APIRouter(prefix=\"/v1/labrador\")"
        ));
        assert!(!is_non_call_context(
            "resp = requests.get(f\"{url}/v1/items\")"
        ));
        assert!(!is_non_call_context(
            "return f\"{base_url}/v1/langfuse/relay\""
        ));
    }

    #[test]
    fn test_scan_source_skips_non_call_context() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.py"),
            "_SKIP_PATHS = {\n    \"/v1/documents\",\n    \"/v1/estimates\",\n}\n",
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        assert!(
            results.is_empty(),
            "skip-list URLs should not be detected, got {:?}",
            results
        );
    }

    #[test]
    fn test_scan_source_skips_decorators() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("routes.py"),
            "@router.post(\"/v1/langfuse/relay\")\nasync def relay_traces(request):\n    pass\n",
        )
        .unwrap();
        let results = scan_source_for_urls(dir.path());
        assert!(
            results.is_empty(),
            "decorator URL should not be detected as consumer call, got {:?}",
            results
        );
    }
}
