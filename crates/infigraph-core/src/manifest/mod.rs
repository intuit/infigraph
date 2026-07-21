/// Manifest parser: reads package manifests and lockfiles, extracts dependencies,
/// stores them as Dependency nodes with DEPENDS_ON edges in the graph.
///
/// Supported: package.json, Cargo.toml, go.mod, pom.xml, build.gradle,
///            requirements.txt, pyproject.toml, Gemfile, composer.json,
///            packages.config, *.csproj, pubspec.yaml
use std::path::Path;

use anyhow::Result;

use crate::graph::GraphBackend;

#[derive(Debug, Clone)]
pub struct DepEntry {
    pub name: String,
    pub version: String,
    pub ecosystem: String,
    pub is_dev: bool,
}

#[derive(Debug, Default)]
pub struct ManifestResult {
    pub ecosystem: String,
    pub manifest_file: String,
    pub deps: Vec<DepEntry>,
    pub doc_urls: Vec<String>,
}

/// Scan a project root for manifests, parse them, store deps in graph.
pub fn index_manifests(root: &Path, backend: &dyn GraphBackend) -> Result<Vec<ManifestResult>> {
    let mut results = Vec::new();

    let candidates = [
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "requirements.txt",
        "pyproject.toml",
        "Gemfile",
        "composer.json",
        "packages.config",
        "pubspec.yaml",
    ];

    for name in &candidates {
        let path = root.join(name);
        if path.exists() {
            if let Ok(result) = parse_manifest(&path) {
                store_manifest(backend, &result)?;
                results.push(result);
            }
        }
    }

    // Also scan for *.csproj files (can be nested)
    scan_csproj(root, backend, &mut results)?;

    Ok(results)
}

/// Query dependencies stored in graph for a project.
pub fn query_deps(backend: &dyn GraphBackend) -> Result<Vec<DepEntry>> {
    let q = "MATCH (d:Dependency) RETURN d.name, d.version, d.ecosystem, d.is_dev ORDER BY d.ecosystem, d.name";
    let rows = backend.raw_query(q)?;

    let mut deps = Vec::new();
    for row in &rows {
        if row.len() >= 4 {
            deps.push(DepEntry {
                name: row[0].trim_matches('"').to_string(),
                version: row[1].trim_matches('"').to_string(),
                ecosystem: row[2].trim_matches('"').to_string(),
                is_dev: row[3] == "True" || row[3] == "true",
            });
        }
    }
    Ok(deps)
}

/// Extract the package's own name from a manifest file at `root`.
/// Checks pyproject.toml, package.json, Cargo.toml (in that order).
pub fn extract_package_name(root: &Path) -> Option<String> {
    for (file, extractor) in [
        (
            "pyproject.toml",
            extract_name_pyproject as fn(&str) -> Option<String>,
        ),
        ("package.json", extract_name_package_json),
        ("Cargo.toml", extract_name_cargo_toml),
    ] {
        let path = root.join(file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some(name) = extractor(&content) {
                return Some(name);
            }
        }
    }
    None
}

fn extract_name_pyproject(content: &str) -> Option<String> {
    let v: toml::Value = content.parse().ok()?;
    v.get("project")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

fn extract_name_package_json(content: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    v.get("name")?.as_str().map(|s| s.to_string())
}

fn extract_name_cargo_toml(content: &str) -> Option<String> {
    let v: toml::Value = content.parse().ok()?;
    v.get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

fn parse_manifest(path: &Path) -> Result<ManifestResult> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let content = std::fs::read_to_string(path)?;

    match name.as_ref() {
        "package.json" => parse_package_json(&content, path),
        "Cargo.toml" => parse_cargo_toml(&content, path),
        "go.mod" => parse_go_mod(&content, path),
        "pom.xml" => parse_pom_xml(&content, path),
        "build.gradle" | "build.gradle.kts" => parse_gradle(&content, path),
        "requirements.txt" => parse_requirements_txt(&content, path),
        "pyproject.toml" => parse_pyproject_toml(&content, path),
        "Gemfile" => parse_gemfile(&content, path),
        "composer.json" => parse_composer_json(&content, path),
        "packages.config" => parse_packages_config(&content, path),
        "pubspec.yaml" => parse_pubspec_yaml(&content, path),
        _ => anyhow::bail!("unknown manifest: {}", name),
    }
}

fn parse_package_json(content: &str, path: &Path) -> Result<ManifestResult> {
    let v: serde_json::Value = serde_json::from_str(content)?;
    let mut deps = Vec::new();

    if let Some(obj) = v.get("dependencies").and_then(|d| d.as_object()) {
        for (name, ver) in obj {
            deps.push(DepEntry {
                name: name.clone(),
                version: ver.as_str().unwrap_or("*").to_string(),
                ecosystem: "npm".to_string(),
                is_dev: false,
            });
        }
    }
    if let Some(obj) = v.get("devDependencies").and_then(|d| d.as_object()) {
        for (name, ver) in obj {
            deps.push(DepEntry {
                name: name.clone(),
                version: ver.as_str().unwrap_or("*").to_string(),
                ecosystem: "npm".to_string(),
                is_dev: true,
            });
        }
    }
    if let Some(obj) = v.get("peerDependencies").and_then(|d| d.as_object()) {
        for (name, ver) in obj {
            deps.push(DepEntry {
                name: name.clone(),
                version: ver.as_str().unwrap_or("*").to_string(),
                ecosystem: "npm".to_string(),
                is_dev: false,
            });
        }
    }

    let mut doc_urls = Vec::new();
    if let Some(s) = v.get("homepage").and_then(|h| h.as_str()) {
        if !s.is_empty() {
            doc_urls.push(s.to_string());
        }
    }
    if let Some(repo) = v.get("repository") {
        if let Some(s) = repo.as_str() {
            if !s.is_empty() {
                doc_urls.push(s.to_string());
            }
        } else if let Some(s) = repo.get("url").and_then(|u| u.as_str()) {
            if !s.is_empty() {
                doc_urls.push(s.to_string());
            }
        }
    }
    if let Some(bugs) = v.get("bugs") {
        if let Some(s) = bugs.as_str() {
            if !s.is_empty() {
                doc_urls.push(s.to_string());
            }
        } else if let Some(s) = bugs.get("url").and_then(|u| u.as_str()) {
            if !s.is_empty() {
                doc_urls.push(s.to_string());
            }
        }
    }

    Ok(ManifestResult {
        ecosystem: "npm".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls,
    })
}

fn parse_cargo_toml(content: &str, path: &Path) -> Result<ManifestResult> {
    let v: toml::Value = content.parse()?;
    let mut deps = Vec::new();

    for (section, is_dev) in &[
        ("dependencies", false),
        ("dev-dependencies", true),
        ("build-dependencies", true),
    ] {
        if let Some(table) = v.get(section).and_then(|d| d.as_table()) {
            for (name, val) in table {
                let version = match val {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Table(t) => t
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("*")
                        .to_string(),
                    _ => "*".to_string(),
                };
                // Skip workspace = true entries (no version)
                if val.as_table().and_then(|t| t.get("workspace")).is_some() {
                    continue;
                }
                deps.push(DepEntry {
                    name: name.clone(),
                    version,
                    ecosystem: "cargo".to_string(),
                    is_dev: *is_dev,
                });
            }
        }
    }

    let mut doc_urls = Vec::new();
    if let Some(pkg) = v.get("package") {
        for key in &["homepage", "repository", "documentation"] {
            if let Some(s) = pkg.get(*key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    doc_urls.push(s.to_string());
                }
            }
        }
    }

    Ok(ManifestResult {
        ecosystem: "cargo".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls,
    })
}

fn parse_go_mod(content: &str, path: &Path) -> Result<ManifestResult> {
    let mut deps = Vec::new();
    let mut in_require = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("require (") || line == "require (" {
            in_require = true;
            continue;
        }
        if in_require && line == ")" {
            in_require = false;
            continue;
        }
        // Single-line: require module v1.2.3
        let parts: Vec<&str> = if in_require {
            line.split_whitespace().collect()
        } else if let Some(stripped) = line.strip_prefix("require ") {
            stripped.split_whitespace().collect()
        } else {
            continue;
        };

        if parts.len() >= 2 {
            let is_indirect = parts
                .get(2)
                .map(|s| s.contains("indirect"))
                .unwrap_or(false);
            deps.push(DepEntry {
                name: parts[0].to_string(),
                version: parts[1].to_string(),
                ecosystem: "go".to_string(),
                is_dev: is_indirect,
            });
        }
    }

    Ok(ManifestResult {
        ecosystem: "go".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_pom_xml(content: &str, path: &Path) -> Result<ManifestResult> {
    // Simple regex-based extraction (no full XML parse needed)
    let dep_re = regex::Regex::new(
        r"<dependency>\s*<groupId>([^<]+)</groupId>\s*<artifactId>([^<]+)</artifactId>\s*(?:<version>([^<]+)</version>\s*)?(?:<scope>([^<]+)</scope>\s*)?"
    ).unwrap();

    let mut deps = Vec::new();
    for cap in dep_re.captures_iter(content) {
        let group = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let artifact = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let version = cap.get(3).map(|m| m.as_str()).unwrap_or("*");
        let scope = cap.get(4).map(|m| m.as_str()).unwrap_or("compile");
        let is_dev = matches!(scope, "test" | "provided");
        deps.push(DepEntry {
            name: format!("{}:{}", group.trim(), artifact.trim()),
            version: version.trim().to_string(),
            ecosystem: "maven".to_string(),
            is_dev,
        });
    }

    let mut doc_urls = Vec::new();
    let url_re = regex::Regex::new(r"<url>\s*([^<]+?)\s*</url>").unwrap();
    let scm_re = regex::Regex::new(r"<scm>\s*<url>\s*([^<]+?)\s*</url>").unwrap();
    if let Some(cap) = url_re.captures(content) {
        let u = cap[1].trim();
        if !u.is_empty() {
            doc_urls.push(u.to_string());
        }
    }
    if let Some(cap) = scm_re.captures(content) {
        let u = cap[1].trim();
        if !u.is_empty() {
            doc_urls.push(u.to_string());
        }
    }

    Ok(ManifestResult {
        ecosystem: "maven".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls,
    })
}

fn parse_gradle(content: &str, path: &Path) -> Result<ManifestResult> {
    // Match: implementation 'group:artifact:version' or testImplementation("...")
    let re = regex::Regex::new(
        r#"(?:implementation|api|compileOnly|runtimeOnly|testImplementation|testCompileOnly|annotationProcessor)\s*[("']([^"'()]+)[)"']"#
    ).unwrap();

    let mut deps = Vec::new();
    for cap in re.captures_iter(content) {
        let spec = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let is_dev = cap
            .get(0)
            .map(|m| m.as_str().starts_with("test"))
            .unwrap_or(false);
        let parts: Vec<&str> = spec.split(':').collect();
        let name = if parts.len() >= 2 {
            format!("{}:{}", parts[0], parts[1])
        } else {
            spec.to_string()
        };
        let version = parts.get(2).unwrap_or(&"*").to_string();
        deps.push(DepEntry {
            name,
            version,
            ecosystem: "gradle".to_string(),
            is_dev,
        });
    }

    Ok(ManifestResult {
        ecosystem: "gradle".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_requirements_txt(content: &str, path: &Path) -> Result<ManifestResult> {
    let mut deps = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        // Handle: name==1.0, name>=1.0, name~=1.0, name
        let (name, version) = if let Some(idx) = line.find(['=', '>', '<', '~', '!']) {
            (
                line[..idx].trim().to_string(),
                line[idx..].trim().to_string(),
            )
        } else {
            (line.to_string(), "*".to_string())
        };
        if !name.is_empty() {
            deps.push(DepEntry {
                name,
                version,
                ecosystem: "pip".to_string(),
                is_dev: false,
            });
        }
    }

    Ok(ManifestResult {
        ecosystem: "pip".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_pyproject_toml(content: &str, path: &Path) -> Result<ManifestResult> {
    let v: toml::Value = content.parse()?;
    let mut deps = Vec::new();

    // PEP 621: [project] dependencies
    if let Some(arr) = v
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for dep in arr {
            if let Some(s) = dep.as_str() {
                let (name, ver) = split_pep508(s);
                deps.push(DepEntry {
                    name,
                    version: ver,
                    ecosystem: "pip".to_string(),
                    is_dev: false,
                });
            }
        }
    }
    // Poetry: [tool.poetry.dependencies]
    if let Some(table) = v
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, val) in table {
            if name == "python" {
                continue;
            }
            let version = match val {
                toml::Value::String(s) => s.clone(),
                toml::Value::Table(t) => t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*")
                    .to_string(),
                _ => "*".to_string(),
            };
            deps.push(DepEntry {
                name: name.clone(),
                version,
                ecosystem: "pip".to_string(),
                is_dev: false,
            });
        }
    }
    if let Some(table) = v
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dev-dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, val) in table {
            let version = match val {
                toml::Value::String(s) => s.clone(),
                _ => "*".to_string(),
            };
            deps.push(DepEntry {
                name: name.clone(),
                version,
                ecosystem: "pip".to_string(),
                is_dev: true,
            });
        }
    }

    let mut doc_urls = Vec::new();
    if let Some(urls) = v
        .get("project")
        .and_then(|p| p.get("urls"))
        .and_then(|u| u.as_table())
    {
        for (_key, val) in urls {
            if let Some(s) = val.as_str() {
                if !s.is_empty() {
                    doc_urls.push(s.to_string());
                }
            }
        }
    }

    Ok(ManifestResult {
        ecosystem: "pip".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls,
    })
}

fn parse_gemfile(content: &str, path: &Path) -> Result<ManifestResult> {
    let re = regex::Regex::new(r#"gem\s+['"]([^'"]+)['"](?:\s*,\s*['"]([^'"]+)['"])?"#).unwrap();
    let mut deps = Vec::new();
    let mut in_test_group = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("group :test") || trimmed.starts_with("group :development") {
            in_test_group = true;
        }
        if trimmed == "end" {
            in_test_group = false;
        }
        if let Some(cap) = re.captures(trimmed) {
            let name = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            let version = cap.get(2).map(|m| m.as_str()).unwrap_or("*").to_string();
            deps.push(DepEntry {
                name,
                version,
                ecosystem: "gem".to_string(),
                is_dev: in_test_group,
            });
        }
    }

    Ok(ManifestResult {
        ecosystem: "gem".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_composer_json(content: &str, path: &Path) -> Result<ManifestResult> {
    let v: serde_json::Value = serde_json::from_str(content)?;
    let mut deps = Vec::new();

    for (key, is_dev) in &[("require", false), ("require-dev", true)] {
        if let Some(obj) = v.get(*key).and_then(|d| d.as_object()) {
            for (name, ver) in obj {
                if name == "php" {
                    continue;
                }
                deps.push(DepEntry {
                    name: name.clone(),
                    version: ver.as_str().unwrap_or("*").to_string(),
                    ecosystem: "composer".to_string(),
                    is_dev: *is_dev,
                });
            }
        }
    }

    Ok(ManifestResult {
        ecosystem: "composer".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_packages_config(content: &str, path: &Path) -> Result<ManifestResult> {
    let re = regex::Regex::new(r#"<package\s+id="([^"]+)"\s+version="([^"]+)""#).unwrap();
    let dev_re = regex::Regex::new(r#"developmentDependency="true""#).unwrap();
    let mut deps = Vec::new();

    for line in content.lines() {
        if let Some(cap) = re.captures(line) {
            let is_dev = dev_re.is_match(line);
            deps.push(DepEntry {
                name: cap[1].to_string(),
                version: cap[2].to_string(),
                ecosystem: "nuget".to_string(),
                is_dev,
            });
        }
    }

    Ok(ManifestResult {
        ecosystem: "nuget".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn parse_pubspec_yaml(content: &str, path: &Path) -> Result<ManifestResult> {
    // Simple line-based parse for pubspec.yaml dependencies sections
    let mut deps = Vec::new();
    let mut in_deps = false;
    let mut in_dev_deps = false;
    let dep_re = regex::Regex::new(r"^\s{2}(\w[\w_-]*):\s*(.*)$").unwrap();

    for line in content.lines() {
        if line.starts_with("dependencies:") {
            in_deps = true;
            in_dev_deps = false;
            continue;
        }
        if line.starts_with("dev_dependencies:") {
            in_dev_deps = true;
            in_deps = false;
            continue;
        }
        if !line.starts_with(' ') && !line.is_empty() {
            in_deps = false;
            in_dev_deps = false;
        }

        if in_deps || in_dev_deps {
            if let Some(cap) = dep_re.captures(line) {
                let name = cap[1].to_string();
                let raw_ver = cap[2].trim().to_string();
                let version = if raw_ver.is_empty() || raw_ver == "any" {
                    "*".to_string()
                } else {
                    raw_ver
                };
                if name != "flutter" && name != "sdk" {
                    deps.push(DepEntry {
                        name,
                        version,
                        ecosystem: "pub".to_string(),
                        is_dev: in_dev_deps,
                    });
                }
            }
        }
    }

    Ok(ManifestResult {
        ecosystem: "pub".to_string(),
        manifest_file: path.to_string_lossy().replace('\\', "/"),
        deps,
        doc_urls: Vec::new(),
    })
}

fn scan_csproj(
    root: &Path,
    backend: &dyn GraphBackend,
    results: &mut Vec<ManifestResult>,
) -> Result<()> {
    let re =
        regex::Regex::new(r#"<PackageReference\s+Include="([^"]+)"\s+Version="([^"]+)""#).unwrap();
    scan_csproj_dir(root, &re, backend, results)
}

fn scan_csproj_dir(
    dir: &Path,
    re: &regex::Regex,
    backend: &dyn GraphBackend,
    results: &mut Vec<ManifestResult>,
) -> Result<()> {
    let ignore = [".git", "node_modules", "target", "bin", "obj"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if path.is_dir() && !ignore.contains(&name_str.as_ref()) {
            scan_csproj_dir(&path, re, backend, results)?;
        } else if path
            .extension()
            .map(|e| e == "csproj" || e == "fsproj" || e == "vbproj")
            .unwrap_or(false)
        {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let mut deps = Vec::new();
                for cap in re.captures_iter(&content) {
                    deps.push(DepEntry {
                        name: cap[1].to_string(),
                        version: cap[2].to_string(),
                        ecosystem: "nuget".to_string(),
                        is_dev: false,
                    });
                }
                if !deps.is_empty() {
                    let result = ManifestResult {
                        ecosystem: "nuget".to_string(),
                        manifest_file: path.to_string_lossy().replace('\\', "/"),
                        deps,
                        doc_urls: Vec::new(),
                    };
                    let _ = store_manifest(backend, &result);
                    results.push(result);
                }
            }
        }
    }
    Ok(())
}

fn store_manifest(backend: &dyn GraphBackend, result: &ManifestResult) -> Result<()> {
    for dep in &result.deps {
        let id = format!("{}::{}", dep.ecosystem, dep.name);
        let check = format!(
            "MATCH (d:Dependency) WHERE d.id = '{}' RETURN d.id",
            escape(&id)
        );
        let existing = backend.raw_query(&check)?;
        if existing.is_empty() {
            let insert = format!(
                "CREATE (d:Dependency {{id: '{}', name: '{}', version: '{}', ecosystem: '{}', is_dev: {}}})",
                escape(&id), escape(&dep.name), escape(&dep.version), escape(&dep.ecosystem), dep.is_dev
            );
            let _ = backend.raw_query(&insert);
        } else {
            let update = format!(
                "MATCH (d:Dependency) WHERE d.id = '{}' SET d.version = '{}', d.is_dev = {}",
                escape(&id),
                escape(&dep.version),
                dep.is_dev
            );
            let _ = backend.raw_query(&update);
        }

        let rel = format!(
            "MATCH (m:Module), (d:Dependency) WHERE m.file CONTAINS '{}' AND d.id = '{}' \
             CREATE (m)-[:DEPENDS_ON {{is_dev: {}}}]->(d)",
            escape(result.manifest_file.rsplit('/').next().unwrap_or("")),
            escape(&id),
            dep.is_dev
        );
        let _ = backend.raw_query(&rel);
    }
    Ok(())
}

fn split_pep508(s: &str) -> (String, String) {
    if let Some(idx) = s.find(['=', '>', '<', '~', '!', '[', ';']) {
        (s[..idx].trim().to_string(), s[idx..].trim().to_string())
    } else {
        (s.trim().to_string(), "*".to_string())
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::KuzuBackend;

    #[test]
    fn test_manifest_upsert_updates_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("graph");
        let store = crate::graph::GraphStore::open(&db_path).unwrap();
        let backend = KuzuBackend::from_store(store);
        let result1 = ManifestResult {
            manifest_file: "pyproject.toml".to_string(),
            ecosystem: "pypi".to_string(),
            deps: vec![DepEntry {
                name: "requests".to_string(),
                version: "1.0".to_string(),
                ecosystem: "pypi".to_string(),
                is_dev: false,
            }],
            doc_urls: Vec::new(),
        };
        store_manifest(&backend, &result1).unwrap();

        let conn = backend.inner().connection().unwrap();
        let gq = crate::graph::GraphQuery::new(&conn);
        let rows = gq
            .raw_query("MATCH (d:Dependency) WHERE d.id = 'pypi::requests' RETURN d.version")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "1.0");

        let result2 = ManifestResult {
            manifest_file: "pyproject.toml".to_string(),
            ecosystem: "pypi".to_string(),
            deps: vec![DepEntry {
                name: "requests".to_string(),
                version: "2.0".to_string(),
                ecosystem: "pypi".to_string(),
                is_dev: false,
            }],
            doc_urls: Vec::new(),
        };
        store_manifest(&backend, &result2).unwrap();

        let rows2 = gq
            .raw_query("MATCH (d:Dependency) WHERE d.id = 'pypi::requests' RETURN d.version")
            .unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0][0], "2.0");
    }

    #[test]
    fn test_package_json_extracts_doc_urls() {
        let content = r#"{"name":"foo","version":"1.0","homepage":"https://example.com/docs","repository":{"url":"https://github.com/org/foo"},"bugs":{"url":"https://github.com/org/foo/issues"},"dependencies":{}}"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("package.json");
        std::fs::write(&path, content).unwrap();
        let result = parse_manifest(&path).unwrap();
        assert_eq!(result.doc_urls.len(), 3);
        assert!(result
            .doc_urls
            .contains(&"https://example.com/docs".to_string()));
        assert!(result
            .doc_urls
            .contains(&"https://github.com/org/foo".to_string()));
        assert!(result
            .doc_urls
            .contains(&"https://github.com/org/foo/issues".to_string()));
    }

    #[test]
    fn test_cargo_toml_extracts_doc_urls() {
        let content = r#"
[package]
name = "foo"
version = "0.1.0"
homepage = "https://example.com"
repository = "https://github.com/org/foo"
documentation = "https://docs.rs/foo"

[dependencies]
"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("Cargo.toml");
        std::fs::write(&path, content).unwrap();
        let result = parse_manifest(&path).unwrap();
        assert_eq!(result.doc_urls.len(), 3);
        assert!(result.doc_urls.contains(&"https://example.com".to_string()));
        assert!(result
            .doc_urls
            .contains(&"https://github.com/org/foo".to_string()));
        assert!(result.doc_urls.contains(&"https://docs.rs/foo".to_string()));
    }

    #[test]
    fn test_pyproject_extracts_project_urls() {
        let content = r#"
[project]
name = "foo"

[project.urls]
Documentation = "https://docs.example.com"
Repository = "https://github.com/org/foo"
"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("pyproject.toml");
        std::fs::write(&path, content).unwrap();
        let result = parse_manifest(&path).unwrap();
        assert_eq!(result.doc_urls.len(), 2);
        assert!(result
            .doc_urls
            .contains(&"https://docs.example.com".to_string()));
        assert!(result
            .doc_urls
            .contains(&"https://github.com/org/foo".to_string()));
    }

    #[test]
    fn test_requirements_txt_no_doc_urls() {
        let content = "requests==2.28\nflask>=2.0\n";
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("requirements.txt");
        std::fs::write(&path, content).unwrap();
        let result = parse_manifest(&path).unwrap();
        assert!(result.doc_urls.is_empty());
        assert_eq!(result.deps.len(), 2);
    }
}
