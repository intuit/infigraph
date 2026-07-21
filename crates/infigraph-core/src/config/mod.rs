use anyhow::Result;
use serde::Serialize;
use std::path::Path;

use crate::graph::GraphBackend;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigBinding {
    pub symbol_id: String,
    pub kind: &'static str,
    pub key: String,
    pub value: String,
    pub profile: String,
    pub source_file: String,
}

struct ConditionalPattern {
    kind: &'static str,
    patterns: &'static [&'static str],
}

static CONDITIONAL_PATTERNS: &[ConditionalPattern] = &[
    // Spring profiles
    ConditionalPattern {
        kind: "Profile",
        patterns: &[
            "@Profile(",
            "@ConditionalOnProperty(",
            "@ConditionalOnBean(",
            "@ConditionalOnMissingBean(",
            "@ConditionalOnClass(",
            "@ConditionalOnExpression(",
        ],
    },
    // Spring qualifiers
    ConditionalPattern {
        kind: "Qualifier",
        patterns: &["@Qualifier(", "@Primary", "@Named("],
    },
    // .NET environment
    ConditionalPattern {
        kind: "Environment",
        patterns: &[
            "[Environment(",
            "IsDevelopment()",
            "IsProduction()",
            "IsStaging()",
            "ASPNETCORE_ENVIRONMENT",
        ],
    },
    // Python/Django
    ConditionalPattern {
        kind: "DjangoSetting",
        patterns: &[
            "settings.DEBUG",
            "settings.DATABASES",
            "settings.INSTALLED_APPS",
            "settings.MIDDLEWARE",
            "getattr(settings,",
            "os.environ.get(",
            "os.getenv(",
        ],
    },
    // Rails
    ConditionalPattern {
        kind: "RailsEnv",
        patterns: &[
            "Rails.env.production?",
            "Rails.env.development?",
            "Rails.env.test?",
            "Rails.env.staging?",
            "Rails.application.config.",
        ],
    },
    // Go build tags
    ConditionalPattern {
        kind: "BuildTag",
        patterns: &["//go:build ", "// +build "],
    },
    // Rust feature gates
    ConditionalPattern {
        kind: "FeatureGate",
        patterns: &[
            "#[cfg(feature",
            "#[cfg(target_os",
            "#[cfg(test)]",
            "#[cfg(not(",
            "#[cfg_attr(",
        ],
    },
    // Node.js / NestJS
    ConditionalPattern {
        kind: "EnvConfig",
        patterns: &[
            "process.env.",
            "ConfigService.get(",
            "ConfigService.getOrThrow(",
            "@Optional()",
        ],
    },
];

pub fn detect_config_bindings(backend: &dyn GraphBackend) -> Result<Vec<ConfigBinding>> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.docstring IS NOT NULL AND s.docstring <> '' RETURN s.id, s.docstring, s.file")?;

    let mut bindings = Vec::new();

    for row in result {
        if row.len() < 3 {
            continue;
        }
        let symbol_id = row[0].to_string();
        let docstring = row[1].to_string();
        let file = row[2].to_string();

        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    let detail = extract_config_detail(&docstring, pattern);
                    let (key, value) = parse_config_kv(&detail, pattern);
                    bindings.push(ConfigBinding {
                        symbol_id: symbol_id.clone(),
                        kind: cp.kind,
                        key,
                        value,
                        profile: extract_profile(&detail, cp.kind),
                        source_file: file.clone(),
                    });
                    break;
                }
            }
        }
    }

    if !bindings.is_empty() {
        write_config_bindings(backend, &bindings)?;
    }

    Ok(bindings)
}

fn extract_config_detail(docstring: &str, pattern: &str) -> String {
    for line in docstring.lines() {
        if line.contains(pattern) {
            return line.trim().to_string();
        }
    }
    pattern.to_string()
}

fn parse_config_kv(detail: &str, pattern: &str) -> (String, String) {
    if let Some(start) = detail.find(pattern) {
        let after = &detail[start + pattern.len()..];
        let inner = after
            .trim_start_matches(['(', '"', '\''])
            .split([')', '"', '\''])
            .next()
            .unwrap_or("");
        if inner.contains('=') {
            let parts: Vec<&str> = inner.splitn(2, '=').collect();
            return (
                parts[0].trim().to_string(),
                parts
                    .get(1)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
            );
        }
        return (inner.to_string(), String::new());
    }
    (detail.to_string(), String::new())
}

fn extract_profile(detail: &str, kind: &str) -> String {
    match kind {
        "Profile" => {
            if let Some(start) = detail.find("@Profile(") {
                let after = &detail[start + 9..];
                let inner = after
                    .trim_start_matches(['"', '\''])
                    .split(['"', '\'', ')'])
                    .next()
                    .unwrap_or("default");
                return inner.to_string();
            }
            "default".to_string()
        }
        "RailsEnv" => {
            if detail.contains("production") {
                "production".to_string()
            } else if detail.contains("development") {
                "development".to_string()
            } else if detail.contains("staging") {
                "staging".to_string()
            } else if detail.contains("test") {
                "test".to_string()
            } else {
                "default".to_string()
            }
        }
        "Environment" => {
            if detail.contains("Production") {
                "production".to_string()
            } else if detail.contains("Development") {
                "development".to_string()
            } else if detail.contains("Staging") {
                "staging".to_string()
            } else {
                "default".to_string()
            }
        }
        _ => "default".to_string(),
    }
}

fn write_config_bindings(backend: &dyn GraphBackend, bindings: &[ConfigBinding]) -> Result<()> {
    let _ = backend.raw_query("MATCH (c:ConfigBinding) DETACH DELETE c");

    for b in bindings {
        let id = format!("{}::{}::{}", b.symbol_id, b.kind, b.key);
        let id_esc = crate::escape_str(&id);
        let kind_esc = crate::escape_str(b.kind);
        let key_esc = crate::escape_str(&b.key);
        let val_esc = crate::escape_str(&b.value);
        let profile_esc = crate::escape_str(&b.profile);
        let src_esc = crate::escape_str(&b.source_file);
        let sym_esc = crate::escape_str(&b.symbol_id);

        let _ = backend.raw_query(&format!(
            "CREATE (c:ConfigBinding {{id: '{id_esc}', kind: '{kind_esc}', key: '{key_esc}', value: '{val_esc}', `profile`: '{profile_esc}', source_file: '{src_esc}'}})"
        ));
        let _ = backend.raw_query(&format!(
            "MATCH (s:Symbol), (c:ConfigBinding) WHERE s.id = '{sym_esc}' AND c.id = '{id_esc}' CREATE (s)-[:HAS_CONFIG]->(c)"
        ));
    }

    Ok(())
}

pub fn detect_config_files(root: &Path) -> Vec<ConfigFileInfo> {
    let mut configs = Vec::new();
    let patterns = [
        ("application.yml", "Spring"),
        ("application.yaml", "Spring"),
        ("application.properties", "Spring"),
        ("application-*.yml", "Spring"),
        ("application-*.yaml", "Spring"),
        ("application-*.properties", "Spring"),
        ("settings.py", "Django"),
        ("appsettings.json", "DotNet"),
        ("appsettings.*.json", "DotNet"),
        ("config/database.yml", "Rails"),
        ("config/environments/", "Rails"),
        (".env", "Generic"),
        (".env.*", "Generic"),
    ];

    if let Ok(walker) = glob_walk(root) {
        for entry in walker {
            let rel = entry.strip_prefix(root).unwrap_or(&entry);
            let name = rel.file_name().unwrap_or_default().to_string_lossy();
            for (pat, framework) in &patterns {
                if matches_config_pattern(&name, &rel.to_string_lossy(), pat) {
                    let profile = extract_profile_from_filename(&name, framework);
                    configs.push(ConfigFileInfo {
                        path: rel.to_string_lossy().to_string(),
                        framework: framework.to_string(),
                        profile,
                    });
                    break;
                }
            }
        }
    }
    configs
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigFileInfo {
    pub path: String,
    pub framework: String,
    pub profile: String,
}

fn glob_walk(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    walk_config_dir(root, root, &mut files, 0)?;
    Ok(files)
}

#[allow(clippy::only_used_in_recursion)]
fn walk_config_dir(
    root: &Path,
    dir: &Path,
    files: &mut Vec<std::path::PathBuf>,
    depth: usize,
) -> Result<()> {
    if depth > 5 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".env" && !name.starts_with(".env.") {
            continue;
        }
        if path.is_dir() {
            let skip = [
                "node_modules",
                "target",
                "build",
                "dist",
                ".git",
                "__pycache__",
                "venv",
                ".venv",
            ];
            if !skip.contains(&name.as_str()) {
                walk_config_dir(root, &path, files, depth + 1)?;
            }
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn matches_config_pattern(filename: &str, rel_path: &str, pattern: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            return filename.starts_with(parts[0]) && filename.ends_with(parts[1]);
        }
    }
    if pattern.contains('/') {
        return rel_path.contains(pattern);
    }
    filename == pattern
}

fn extract_profile_from_filename(filename: &str, framework: &str) -> String {
    match framework {
        "Spring" => {
            if filename.starts_with("application-") {
                let name = filename.strip_prefix("application-").unwrap_or("");
                let profile = name.split('.').next().unwrap_or("default");
                return profile.to_string();
            }
            "default".to_string()
        }
        "DotNet" => {
            if filename.starts_with("appsettings.") && filename != "appsettings.json" {
                let name = filename.strip_prefix("appsettings.").unwrap_or("");
                let profile = name.strip_suffix(".json").unwrap_or(name);
                return profile.to_string();
            }
            "default".to_string()
        }
        "Generic" => {
            if filename.starts_with(".env.") {
                return filename
                    .strip_prefix(".env.")
                    .unwrap_or("default")
                    .to_string();
            }
            "default".to_string()
        }
        _ => "default".to_string(),
    }
}

pub fn format_config_bindings(
    bindings: &[ConfigBinding],
    config_files: &[ConfigFileInfo],
) -> String {
    if bindings.is_empty() && config_files.is_empty() {
        return "No configuration bindings or config files detected.".to_string();
    }

    let mut out = String::new();

    if !config_files.is_empty() {
        out.push_str(&format!(
            "Config files detected: {}\n\n",
            config_files.len()
        ));
        let mut by_fw: std::collections::BTreeMap<&str, Vec<&ConfigFileInfo>> =
            std::collections::BTreeMap::new();
        for cf in config_files {
            by_fw.entry(&cf.framework).or_default().push(cf);
        }
        for (fw, files) in &by_fw {
            out.push_str(&format!("## {} ({})\n", fw, files.len()));
            for f in files {
                out.push_str(&format!("  {} [profile: {}]\n", f.path, f.profile));
            }
            out.push('\n');
        }
    }

    if !bindings.is_empty() {
        out.push_str(&format!("Config bindings: {} total\n\n", bindings.len()));
        let mut by_kind: std::collections::BTreeMap<&str, Vec<&ConfigBinding>> =
            std::collections::BTreeMap::new();
        for b in bindings {
            by_kind.entry(b.kind).or_default().push(b);
        }
        for (kind, items) in &by_kind {
            out.push_str(&format!("## {} ({} symbols)\n", kind, items.len()));
            for item in items {
                if item.value.is_empty() {
                    out.push_str(&format!(
                        "  {} — {} [profile: {}]\n",
                        item.symbol_id, item.key, item.profile
                    ));
                } else {
                    out.push_str(&format!(
                        "  {} — {}={} [profile: {}]\n",
                        item.symbol_id, item.key, item.value, item.profile
                    ));
                }
            }
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_spring_profile() {
        let docstring = "@Profile(\"production\")\n@Component\npublic class ProdDataSource {}";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"Profile"), "should detect @Profile");
    }

    #[test]
    fn test_detect_spring_qualifier() {
        let docstring = "@Qualifier(\"primaryDB\")\n@Autowired\nprivate DataSource ds;";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"Qualifier"), "should detect @Qualifier");
    }

    #[test]
    fn test_detect_dotnet_environment() {
        let docstring = "if (env.IsDevelopment())\n{\n    app.UseDeveloperExceptionPage();\n}";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"Environment"),
            "should detect IsDevelopment()"
        );
    }

    #[test]
    fn test_detect_django_settings() {
        let docstring = "if settings.DEBUG:\n    print('debug mode')";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"DjangoSetting"),
            "should detect settings.DEBUG"
        );
    }

    #[test]
    fn test_detect_rails_env() {
        let docstring = "if Rails.env.production?\n  config.force_ssl = true\nend";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"RailsEnv"),
            "should detect Rails.env.production?"
        );
    }

    #[test]
    fn test_detect_go_build_tag() {
        let docstring = "//go:build linux && amd64\npackage main";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"BuildTag"), "should detect //go:build");
    }

    #[test]
    fn test_detect_rust_cfg() {
        let docstring = "#[cfg(feature = \"postgres\")]\nmod postgres_backend {}";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"FeatureGate"),
            "should detect #[cfg(feature"
        );
    }

    #[test]
    fn test_detect_node_env() {
        let docstring = "const port = process.env.PORT || 3000;";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"EnvConfig"), "should detect process.env.");
    }

    #[test]
    fn test_extract_profile_spring() {
        let detail = "@Profile(\"production\")";
        let profile = extract_profile(detail, "Profile");
        assert_eq!(profile, "production");
    }

    #[test]
    fn test_extract_profile_rails() {
        let detail = "Rails.env.production?";
        let profile = extract_profile(detail, "RailsEnv");
        assert_eq!(profile, "production");
    }

    #[test]
    fn test_extract_profile_dotnet() {
        let detail = "env.IsProduction()";
        let profile = extract_profile(detail, "Environment");
        assert_eq!(profile, "production");
    }

    #[test]
    fn test_config_file_spring_profile() {
        assert_eq!(
            extract_profile_from_filename("application-prod.yml", "Spring"),
            "prod"
        );
        assert_eq!(
            extract_profile_from_filename("application.yml", "Spring"),
            "default"
        );
    }

    #[test]
    fn test_config_file_dotnet_profile() {
        assert_eq!(
            extract_profile_from_filename("appsettings.Production.json", "DotNet"),
            "Production"
        );
        assert_eq!(
            extract_profile_from_filename("appsettings.json", "DotNet"),
            "default"
        );
    }

    #[test]
    fn test_config_file_env_profile() {
        assert_eq!(
            extract_profile_from_filename(".env.production", "Generic"),
            "production"
        );
        assert_eq!(extract_profile_from_filename(".env", "Generic"), "default");
    }

    #[test]
    fn test_matches_config_pattern() {
        assert!(matches_config_pattern(
            "application.yml",
            "application.yml",
            "application.yml"
        ));
        assert!(matches_config_pattern(
            "application-prod.yml",
            "application-prod.yml",
            "application-*.yml"
        ));
        assert!(!matches_config_pattern(
            "other.yml",
            "other.yml",
            "application-*.yml"
        ));
        assert!(matches_config_pattern(
            ".env.production",
            ".env.production",
            ".env.*"
        ));
    }

    #[test]
    fn test_no_false_positive_on_plain_text() {
        let docstring = "This configures the production database settings.";
        let mut found = Vec::new();
        for cp in CONDITIONAL_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.is_empty(), "plain text should not match: {:?}", found);
    }

    #[test]
    fn test_parse_config_kv_spring_profile() {
        let detail = "@Profile(\"production\")";
        let (key, _value) = parse_config_kv(detail, "@Profile(");
        assert_eq!(key, "production");
    }

    #[test]
    fn test_parse_config_kv_conditional() {
        let detail = "@ConditionalOnProperty(name=\"feature.enabled\", havingValue=\"true\")";
        let (key, _val) = parse_config_kv(detail, "@ConditionalOnProperty(");
        assert!(
            key.contains("feature.enabled") || key.contains("name"),
            "key={}",
            key
        );
    }
}
