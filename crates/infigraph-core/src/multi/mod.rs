mod bridge;
pub mod combined;
mod cross_service;
pub mod grpc;

pub use bridge::*;
pub use cross_service::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::lang::LanguageRegistry;
use crate::Infigraph;

#[cfg(feature = "neo4j")]
use crate::graph::GraphBackend;

#[cfg(feature = "postgres")]
use crate::meta::PostgresMetaStore;

/// Global registry stored at ~/.infigraph/registry.json
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub repos: HashMap<String, RepoEntry>,
    pub groups: HashMap<String, Group>,
}

/// A registered repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub path: PathBuf,
    pub languages: Vec<String>,
    pub symbol_count: u64,
    pub module_count: u64,
    #[serde(default)]
    pub last_indexed_commit: Option<String>,
}

/// A group of repositories (e.g., microservice architecture).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub org: String,
    pub repos: Vec<String>,
    pub contracts: Vec<Contract>,
}

/// A contract extracted from a service (HTTP route, gRPC endpoint, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub kind: ContractKind,
    pub service: String,
    pub method: String,
    pub path: String,
    pub symbol_id: String,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContractKind {
    HttpRoute,
    GrpcService,
    EventPublish,
    EventSubscribe,
    SharedPackage,
}

impl Registry {
    /// Load registry — from Postgres in remote mode, JSON file otherwise.
    pub fn load() -> Result<Self> {
        #[cfg(feature = "postgres")]
        {
            if is_remote_mode() {
                let pg = PostgresMetaStore::connect_from_env_cached()?;
                pg.init_schema()?;
                return pg.load_registry();
            }
        }
        let path = registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)?;
        let registry: Registry = serde_json::from_str(&data)?;
        Ok(registry)
    }

    /// Save registry — to Postgres in remote mode, JSON file otherwise.
    pub fn save(&self) -> Result<()> {
        #[cfg(feature = "postgres")]
        {
            if is_remote_mode() {
                let pg = PostgresMetaStore::connect_from_env_cached()?;
                pg.init_schema()?;
                return pg.save_registry(self);
            }
        }
        let path = registry_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        Ok(())
    }

    /// Register a repository after indexing.
    pub fn register_repo(&mut self, name: &str, path: &Path, prism: &Infigraph) -> Result<()> {
        let stats = prism.stats()?;
        let langs: Vec<String> = prism
            .registry()
            .languages()
            .map(|p| p.name.clone())
            .collect();

        let abs_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let commit = git_head_commit(path);

        self.repos.insert(
            name.to_string(),
            RepoEntry {
                name: name.to_string(),
                path: abs_path,
                languages: langs,
                symbol_count: stats.symbols,
                module_count: stats.modules,
                last_indexed_commit: commit,
            },
        );
        self.save()
    }

    /// Create a new group. If `org` is non-empty, the group key becomes `org/name`
    /// to prevent collisions when multiple teams share one Postgres/Neo4j instance.
    pub fn create_group(&mut self, name: &str) -> Result<()> {
        self.create_group_with_org(name, &default_org())
    }

    pub fn create_group_with_org(&mut self, name: &str, org: &str) -> Result<()> {
        let key = qualified_group_name(org, name);
        if self.groups.contains_key(&key) {
            anyhow::bail!("group '{}' already exists", key);
        }
        self.groups.insert(
            key,
            Group {
                name: name.to_string(),
                org: org.to_string(),
                repos: Vec::new(),
                contracts: Vec::new(),
            },
        );
        self.save()
    }

    /// Add a repo to a group.
    pub fn group_add(&mut self, group_name: &str, repo_name: &str) -> Result<()> {
        let group = self
            .groups
            .get_mut(group_name)
            .context(format!("group '{}' not found", group_name))?;
        if !self.repos.contains_key(repo_name) {
            anyhow::bail!("repo '{}' not registered. Run index first.", repo_name);
        }
        if group.repos.contains(&repo_name.to_string()) {
            return Ok(());
        }
        group.repos.push(repo_name.to_string());
        self.save()
    }

    /// Remove a repo from a group.
    pub fn group_remove(&mut self, group_name: &str, repo_name: &str) -> Result<()> {
        let group = self
            .groups
            .get_mut(group_name)
            .context(format!("group '{}' not found", group_name))?;
        group.repos.retain(|r| r != repo_name);
        self.save()
    }

    /// Search across all repos in a group.
    pub fn group_query(
        &self,
        group_name: &str,
        cypher: &str,
        build_registry: impl Fn() -> Result<LanguageRegistry>,
    ) -> Result<Vec<(String, Vec<Vec<String>>)>> {
        let group = self
            .groups
            .get(group_name)
            .context(format!("group '{}' not found", group_name))?;

        let mut results = Vec::new();
        for repo_name in &group.repos {
            let entry = self
                .repos
                .get(repo_name)
                .context(format!("repo '{}' not in registry", repo_name))?;

            let registry = build_registry()?;
            let mut prism = Infigraph::open(&entry.path, registry)?;
            prism.init()?;

            let backend = prism.backend().context("graph not initialized")?;
            let query_result = backend.raw_query(cypher);

            match query_result {
                Ok(rows) => {
                    if !rows.is_empty() {
                        results.push((repo_name.clone(), rows));
                    }
                }
                Err(e) => {
                    eprintln!("warning: query failed for repo '{}': {}", repo_name, e);
                }
            }
        }
        Ok(results)
    }
}

/// Extract HTTP route contracts from a project's graph.
///
/// Sources (in priority order):
/// 1. Route symbols (kind='Route') — from call-expression routing (Express, Gin, etc.)
/// 2. Decorated functions — docstring contains route decorator (@app.route, #[get], etc.)
/// 3. Heuristic detect_routes fallback
pub fn extract_contracts(prism: &Infigraph, service_name: &str) -> Result<Vec<Contract>> {
    let mut contracts = Vec::new();
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Route symbols (call-expression routes: Express, Gin, Django, etc.)
    let route_rows = raw_query_prism(
        prism,
        "MATCH (s:Symbol) WHERE s.kind = 'Route' RETURN s.id, s.name, s.kind, s.file, s.docstring",
    )?;
    for row in &route_rows {
        let (method, path) = parse_route_name(&row[1]);
        let key = format!("{} {}", method, path);
        if seen_paths.insert(key) {
            contracts.push(Contract {
                kind: ContractKind::HttpRoute,
                service: service_name.to_string(),
                method,
                path,
                symbol_id: row[0].clone(),
                file: row[3].clone(),
            });
        }
    }

    // 2. Decorated functions with route info in docstring
    let decorated_rows = raw_query_prism(prism,
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND s.docstring IS NOT NULL AND (s.docstring CONTAINS '@app.route' OR s.docstring CONTAINS '@app.get' OR s.docstring CONTAINS '@app.post' OR s.docstring CONTAINS '@router.get' OR s.docstring CONTAINS '@router.post' OR s.docstring CONTAINS '@router.put' OR s.docstring CONTAINS '@router.delete' OR s.docstring CONTAINS '@router.patch' OR s.docstring CONTAINS '#[get' OR s.docstring CONTAINS '#[post' OR s.docstring CONTAINS '@GetMapping' OR s.docstring CONTAINS '@PostMapping' OR s.docstring CONTAINS '@RequestMapping' OR s.docstring CONTAINS 'MapGet' OR s.docstring CONTAINS 'MapPost') RETURN s.id, s.name, s.kind, s.file, s.docstring",
    )?;
    let mut prefix_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let project_root = prism.root();
    for row in &decorated_rows {
        let doc = row.get(4).map(|s| s.as_str()).unwrap_or("");
        let (method, path) = parse_route_from_docstring(doc);
        if !path.is_empty() {
            let file = &row[3];
            let full_path = if doc.contains("@router.") {
                let prefix = prefix_cache
                    .entry(file.clone())
                    .or_insert_with(|| extract_router_prefix(project_root, file));
                format!("{}{}", prefix, path)
            } else {
                path
            };
            let key = format!("{} {}", method, full_path);
            if seen_paths.insert(key) {
                contracts.push(Contract {
                    kind: ContractKind::HttpRoute,
                    service: service_name.to_string(),
                    method,
                    path: full_path,
                    symbol_id: row[0].clone(),
                    file: file.clone(),
                });
            }
        }
    }

    Ok(contracts)
}

/// Extract the APIRouter/Router prefix from a Python/Go/etc source file.
/// Looks for patterns like `APIRouter(prefix="/v1/labrador")` or `router.prefix("/api")`.
fn extract_router_prefix(project_root: &Path, file: &str) -> String {
    let full_path = project_root.join(file);
    if let Ok(content) = std::fs::read_to_string(&full_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            // Python FastAPI: APIRouter(prefix="/v1/labrador", ...)
            if let Some(idx) = trimmed.find("prefix=") {
                let after = &trimmed[idx + 7..];
                let after = after.trim_start_matches('\"').trim_start_matches('\'');
                if let Some(end) = after.find(['"', '\'']) {
                    let prefix = &after[..end];
                    if prefix.starts_with('/') {
                        return prefix.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// Parse "GET /api/users" or "MAPGET /api/users" into (method, path).
fn parse_route_name(name: &str) -> (String, String) {
    let parts: Vec<&str> = name.splitn(2, ' ').collect();
    if parts.len() == 2 {
        let method = parts[0].trim().to_uppercase();
        // Normalize MapGet -> GET, MapPost -> POST, etc.
        let method = if method.starts_with("MAP") {
            method.trim_start_matches("MAP").to_string()
        } else {
            method
        };
        (method, parts[1].trim().to_string())
    } else {
        ("UNKNOWN".to_string(), name.to_string())
    }
}

/// Extract method and path from decorator docstrings.
fn parse_route_from_docstring(doc: &str) -> (String, String) {
    // @app.route("/api/users", methods=["GET"])
    // @app.get("/api/users")
    // #[get("/api/payments")]
    // @GetMapping("/api/users")
    let doc_lower = doc.to_lowercase();

    // Extract path from quotes
    let path = doc
        .split('"')
        .chain(doc.split('\''))
        .find(|s| s.starts_with('/'))
        .unwrap_or("")
        .to_string();

    // Extract method
    let method = if doc_lower.contains("methods") {
        // methods=["GET", "POST"] — take first
        if doc_lower.contains("\"get\"") || doc_lower.contains("'get'") {
            "GET"
        } else if doc_lower.contains("\"post\"") || doc_lower.contains("'post'") {
            "POST"
        } else if doc_lower.contains("\"put\"") || doc_lower.contains("'put'") {
            "PUT"
        } else if doc_lower.contains("\"delete\"") || doc_lower.contains("'delete'") {
            "DELETE"
        } else if doc_lower.contains("\"patch\"") || doc_lower.contains("'patch'") {
            "PATCH"
        } else {
            "UNKNOWN"
        }
    } else if doc_lower.contains("@app.get")
        || doc_lower.contains("@router.get")
        || doc_lower.contains("#[get")
        || doc_lower.contains("getmapping")
        || doc_lower.contains("mapget")
    {
        "GET"
    } else if doc_lower.contains("@app.post")
        || doc_lower.contains("@router.post")
        || doc_lower.contains("#[post")
        || doc_lower.contains("postmapping")
        || doc_lower.contains("mappost")
    {
        "POST"
    } else if doc_lower.contains("@app.put")
        || doc_lower.contains("@router.put")
        || doc_lower.contains("#[put")
        || doc_lower.contains("putmapping")
        || doc_lower.contains("mapput")
    {
        "PUT"
    } else if doc_lower.contains("@app.delete")
        || doc_lower.contains("@router.delete")
        || doc_lower.contains("#[delete")
        || doc_lower.contains("deletemapping")
        || doc_lower.contains("mapdelete")
    {
        "DELETE"
    } else if doc_lower.contains("@app.patch")
        || doc_lower.contains("@router.patch")
        || doc_lower.contains("#[patch")
        || doc_lower.contains("patchmapping")
        || doc_lower.contains("mappatch")
    {
        "PATCH"
    } else {
        "UNKNOWN"
    };

    (method.to_string(), path)
}

/// Sync contracts for all repos in a group.
pub fn sync_group_contracts(
    registry: &mut Registry,
    group_name: &str,
    build_registry: impl Fn() -> Result<LanguageRegistry>,
) -> Result<usize> {
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?
        .clone();

    let mut all_contracts = Vec::new();

    // First pass: build publisher map (lightweight, no graph open)
    let mut publishers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for repo_name in &group.repos {
        let entry = match registry.repos.get(repo_name) {
            Some(e) => e,
            None => continue,
        };
        if let Some(pkg_name) = cross_service::read_published_package_name(&entry.path) {
            publishers.insert(pkg_name, repo_name.clone());
        }
    }

    // Second pass: extract routes + collect deps (one graph open per repo)
    let mut dep_map: Vec<(String, Vec<String>)> = Vec::new();
    for repo_name in &group.repos {
        let entry = registry
            .repos
            .get(repo_name)
            .context(format!("repo '{}' not in registry", repo_name))?
            .clone();

        let lang_registry = build_registry()?;
        let mut prism = Infigraph::open(&entry.path, lang_registry)?;
        prism.init()?;

        let contracts = extract_contracts(&prism, repo_name)?;
        all_contracts.extend(contracts);

        // Collect dependency names while graph is open
        let dep_rows =
            raw_query_prism(&prism, "MATCH (d:Dependency) RETURN d.name").unwrap_or_default();
        let dep_names: Vec<String> = dep_rows
            .into_iter()
            .filter_map(|r| r.into_iter().next())
            .collect();
        dep_map.push((repo_name.clone(), dep_names));
    }

    // Match deps against publishers
    let mut seen_pkg: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for (repo_name, dep_names) in &dep_map {
        for dep_name in dep_names {
            if let Some(publisher_repo) = publishers.get(dep_name) {
                if publisher_repo != repo_name
                    && seen_pkg.insert((repo_name.clone(), publisher_repo.clone()))
                {
                    all_contracts.push(Contract {
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

    let count = all_contracts.len();
    let group = registry
        .groups
        .get_mut(group_name)
        .context("group not found")?;
    group.contracts = all_contracts;
    registry.save()?;

    Ok(count)
}

/// Index all repos in a group. Returns Vec of (repo_name, indexed_files, total_files).
pub fn index_group(
    registry: &mut Registry,
    group_name: &str,
    full: bool,
    build_registry: impl Fn() -> Result<LanguageRegistry> + Send + Sync,
) -> Result<Vec<(String, usize, usize)>> {
    let group = registry
        .groups
        .get(group_name)
        .context(format!("group '{}' not found", group_name))?
        .clone();

    // Collect entries upfront so we don't borrow registry during indexing
    let entries: Vec<(String, RepoEntry)> = group
        .repos
        .iter()
        .map(|name| {
            let entry = registry
                .repos
                .get(name)
                .context(format!("repo '{}' not in registry", name))?
                .clone();
            Ok((name.clone(), entry))
        })
        .collect::<Result<Vec<_>>>()?;

    if full {
        for (_, entry) in &entries {
            let tg_dir = entry.path.join(".infigraph");
            if tg_dir.exists() {
                // Preserve session data across full reindex
                let sess_dir = tg_dir.join("sessions");
                let sess_bak = entry.path.join(".infigraph-sessions-backup");
                let had_sessions = sess_dir.exists();
                if had_sessions {
                    let _ = std::fs::rename(&sess_dir, &sess_bak);
                }
                std::fs::remove_dir_all(&tg_dir)?;
                if had_sessions {
                    std::fs::create_dir_all(&tg_dir)?;
                    let _ = std::fs::rename(&sess_bak, &sess_dir);
                }
            }
        }
    }

    // Skip repos whose HEAD commit hasn't changed since last index (incremental).
    // In Neo4j mode, also verify the graph actually has data for "unchanged" repos —
    // if Neo4j storage was wiped, Postgres still thinks repos are indexed.
    let mut to_index: Vec<(String, RepoEntry)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    #[cfg(feature = "neo4j")]
    let neo4j_backend = if !full
        && std::env::var("INFIGRAPH_BACKEND")
            .map(|v| v == "neo4j")
            .unwrap_or(false)
    {
        crate::graph::Neo4jBackend::connect_from_env().ok()
    } else {
        None
    };
    #[cfg(not(feature = "neo4j"))]
    let neo4j_backend: Option<()> = None;

    if full {
        to_index = entries;
    } else {
        for (name, entry) in entries {
            let current_commit = git_head_commit(&entry.path);
            let unchanged = match (&entry.last_indexed_commit, &current_commit) {
                (Some(prev), Some(curr)) => prev == curr,
                _ => false,
            };
            if unchanged {
                #[allow(unused_mut)]
                let mut graph_empty = false;
                #[cfg(feature = "neo4j")]
                if let Some(ref neo) = neo4j_backend {
                    let ns = if group.org.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", group.org, name)
                    };
                    let escaped_ns = crate::escape_str(&ns);
                    let cypher = format!(
                        "MATCH (s:Symbol) WHERE s.id STARTS WITH '{escaped_ns}/' RETURN count(s) AS c"
                    );
                    if let Ok(rows) = neo.raw_query(&cypher) {
                        let count: u64 = rows
                            .first()
                            .and_then(|r| r.first())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        if count == 0 {
                            graph_empty = true;
                        }
                    }
                }
                let _ = &neo4j_backend; // suppress unused warning when neo4j feature off
                if graph_empty {
                    eprintln!(
                        "[group] repo '{}' commit unchanged but graph empty — forcing re-index",
                        name
                    );
                    to_index.push((name, entry));
                } else {
                    skipped.push(name);
                }
            } else {
                to_index.push((name, entry));
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "[group] skipping {} unchanged repos: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }

    // Neo4j backend supports concurrent writes — safe to parallelize
    let use_parallel = std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false);

    let org = group.org.clone();
    let index_one =
        |repo_name: &str, entry: &RepoEntry| -> Result<(String, usize, usize, Infigraph)> {
            let lang_registry = build_registry()?;
            let mut prism = Infigraph::open(&entry.path, lang_registry)?;
            prism.init()?;
            if prism.backend().is_some() {
                let ns = if org.is_empty() {
                    repo_name.to_string()
                } else {
                    format!("{org}/{repo_name}")
                };
                prism.set_namespace(&ns);
            }
            let result = prism.index()?;
            Ok((
                repo_name.to_string(),
                result.indexed_files,
                result.total_files,
                prism,
            ))
        };

    let indexed: Vec<Result<(String, usize, usize, Infigraph)>> = if use_parallel {
        use rayon::prelude::*;
        eprintln!(
            "[group] parallel indexing {} repos via Neo4j backend",
            to_index.len()
        );
        to_index
            .par_iter()
            .map(|(name, entry)| index_one(name, entry))
            .collect()
    } else {
        to_index
            .iter()
            .map(|(name, entry)| index_one(name, entry))
            .collect()
    };

    // Post-index: register repos + manifest indexing (sequential)
    let mut results = Vec::new();
    for item in indexed {
        match item {
            Ok((repo_name, indexed_files, total_files, prism)) => {
                results.push((repo_name.clone(), indexed_files, total_files));

                // Index manifests so Dependency nodes exist for SharedPackage detection
                if let Some(backend) = prism.backend() {
                    if let Err(e) = crate::manifest::index_manifests(prism.root(), backend) {
                        eprintln!("[group] manifest indexing failed for '{}': {e}", repo_name);
                    }
                }

                let entry = registry.repos.get(&repo_name).cloned();
                if let Some(entry) = entry {
                    registry.register_repo(&repo_name, &entry.path, &prism)?;
                }
            }
            Err(e) => {
                eprintln!("[group] indexing failed: {e}");
            }
        }
    }

    Ok(results)
}

fn registry_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
        .context("cannot determine home directory")?;
    Ok(home.join(".infigraph").join("registry.json"))
}

/// Default org from `INFIGRAPH_ORG` env var. Empty string means no org scoping.
pub fn default_org() -> String {
    std::env::var("INFIGRAPH_ORG").unwrap_or_default()
}

/// Build the group key: `org/name` when org is non-empty, `name` otherwise.
pub fn qualified_group_name(org: &str, name: &str) -> String {
    if org.is_empty() {
        name.to_string()
    } else {
        format!("{org}/{name}")
    }
}

#[cfg(feature = "postgres")]
fn is_remote_mode() -> bool {
    std::env::var("INFIGRAPH_BACKEND")
        .map(|v| v == "neo4j")
        .unwrap_or(false)
}

pub fn git_head_commit(repo_path: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn raw_query_prism(prism: &Infigraph, cypher: &str) -> Result<Vec<Vec<String>>> {
    prism
        .backend()
        .context("graph not initialized")?
        .raw_query(cypher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_group_name_no_org() {
        assert_eq!(qualified_group_name("", "backend"), "backend");
    }

    #[test]
    fn test_qualified_group_name_with_org() {
        assert_eq!(
            qualified_group_name("aif-dev3x", "backend"),
            "aif-dev3x/backend"
        );
    }

    #[test]
    fn test_create_group_with_org_stores_qualified_key() {
        let mut registry = Registry::default();
        registry.create_group_with_org("backend", "team-a").unwrap();
        assert!(registry.groups.contains_key("team-a/backend"));
        let group = &registry.groups["team-a/backend"];
        assert_eq!(group.name, "backend");
        assert_eq!(group.org, "team-a");
    }

    #[test]
    fn test_create_group_with_empty_org_stores_plain_key() {
        let mut registry = Registry::default();
        registry.create_group_with_org("backend", "").unwrap();
        assert!(registry.groups.contains_key("backend"));
        let group = &registry.groups["backend"];
        assert_eq!(group.org, "");
    }

    #[test]
    fn test_org_isolation_different_orgs_same_name() {
        let mut registry = Registry::default();
        registry.create_group_with_org("backend", "team-a").unwrap();
        registry.create_group_with_org("backend", "team-b").unwrap();
        assert_eq!(registry.groups.len(), 2);
        assert!(registry.groups.contains_key("team-a/backend"));
        assert!(registry.groups.contains_key("team-b/backend"));
    }

    #[test]
    fn test_create_group_duplicate_with_org_fails() {
        let mut registry = Registry::default();
        registry.create_group_with_org("backend", "team-a").unwrap();
        let result = registry.create_group_with_org("backend", "team-a");
        assert!(result.is_err());
    }

    #[test]
    fn test_default_create_group_uses_default_org() {
        let mut registry = Registry::default();
        registry.create_group("my-group").unwrap();
        assert!(registry.groups.contains_key("my-group"));
        assert_eq!(registry.groups["my-group"].org, "");
    }

    #[test]
    fn test_group_serde_round_trip_with_org() {
        let group = Group {
            name: "backend".to_string(),
            org: "team-x".to_string(),
            repos: vec!["svc-a".to_string()],
            contracts: vec![],
        };
        let json = serde_json::to_string(&group).unwrap();
        let loaded: Group = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.org, "team-x");
        assert_eq!(loaded.name, "backend");
    }

    #[test]
    fn test_group_serde_backwards_compat_missing_org() {
        let json = r#"{"name":"old-group","repos":["svc"],"contracts":[]}"#;
        let loaded: Group = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.org, "");
        assert_eq!(loaded.name, "old-group");
    }
}
