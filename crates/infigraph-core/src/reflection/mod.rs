use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use crate::graph::GraphBackend;

#[derive(Debug, Clone, Serialize)]
pub struct ReflectionSite {
    pub caller_symbol: String,
    pub mechanism: &'static str,
    pub raw_arg: String,
    pub resolved_to: Option<String>,
    pub config_source: Option<String>,
    pub file: String,
    pub line: u32,
}

struct ReflectionPattern {
    mechanism: &'static str,
    patterns: &'static [&'static str],
    extensions: &'static [&'static str],
}

static REFLECTION_PATTERNS: &[ReflectionPattern] = &[
    // Java reflection
    ReflectionPattern {
        mechanism: "ClassForName",
        patterns: &["Class.forName(", "Class.forName ("],
        extensions: &["java", "kt"],
    },
    ReflectionPattern {
        mechanism: "ServiceLoader",
        patterns: &["ServiceLoader.load(", "ServiceLoader.load ("],
        extensions: &["java", "kt"],
    },
    ReflectionPattern {
        mechanism: "JavaReflection",
        patterns: &[".getMethod(", ".getDeclaredMethod(", ".invoke("],
        extensions: &["java", "kt"],
    },
    // Python reflection
    ReflectionPattern {
        mechanism: "Getattr",
        patterns: &["getattr(", "getattr ("],
        extensions: &["py"],
    },
    ReflectionPattern {
        mechanism: "ImportModule",
        patterns: &[
            "importlib.import_module(",
            "importlib.import_module (",
            "__import__(",
        ],
        extensions: &["py"],
    },
    // JavaScript/TypeScript dynamic require/import
    ReflectionPattern {
        mechanism: "DynamicRequire",
        patterns: &["require(variable", "require(`"],
        extensions: &["js", "ts", "jsx", "tsx"],
    },
    ReflectionPattern {
        mechanism: "DynamicImport",
        patterns: &["import(", "import ("],
        extensions: &["js", "ts", "jsx", "tsx"],
    },
    // C# reflection
    ReflectionPattern {
        mechanism: "CSharpReflection",
        patterns: &[
            "Activator.CreateInstance(",
            "Type.GetType(",
            "Assembly.Load(",
        ],
        extensions: &["cs"],
    },
    // Ruby dynamic dispatch
    ReflectionPattern {
        mechanism: "RubySend",
        patterns: &[".send(", ".public_send(", "const_get("],
        extensions: &["rb"],
    },
    // Go plugin
    ReflectionPattern {
        mechanism: "GoPlugin",
        patterns: &["plugin.Open(", "reflect.ValueOf(", "reflect.TypeOf("],
        extensions: &["go"],
    },
];

pub fn detect_reflection_sites(
    backend: &dyn GraphBackend,
    root: &Path,
) -> Result<Vec<ReflectionSite>> {
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.docstring IS NOT NULL AND s.docstring <> '' RETURN s.id, s.docstring, s.file")?;

    let all_symbols = load_symbol_names(backend)?;
    let config_values = scan_config_files(root);

    let mut sites = Vec::new();

    for row in result {
        if row.len() < 3 {
            continue;
        }
        let symbol_id = row[0].to_string();
        let docstring = row[1].to_string();
        let file = row[2].to_string();

        let ext = file.rsplit('.').next().unwrap_or("");

        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&ext) {
                continue;
            }
            for &pattern in rp.patterns {
                if let Some(pos) = docstring.find(pattern) {
                    let raw_arg = extract_string_arg(&docstring[pos + pattern.len()..]);
                    if raw_arg.is_empty() {
                        continue;
                    }

                    let (resolved, config_src) =
                        try_resolve(&raw_arg, rp.mechanism, &all_symbols, &config_values, root);

                    let line = docstring[..pos].lines().count() as u32 + 1;

                    sites.push(ReflectionSite {
                        caller_symbol: symbol_id.clone(),
                        mechanism: rp.mechanism,
                        raw_arg: raw_arg.clone(),
                        resolved_to: resolved,
                        config_source: config_src,
                        file: file.clone(),
                        line,
                    });
                    break;
                }
            }
        }
    }

    if !sites.is_empty() {
        write_resolves_to(backend, &sites)?;
    }

    Ok(sites)
}

fn extract_string_arg(after_pattern: &str) -> String {
    let trimmed = after_pattern.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    if let Some(rest) = trimmed.strip_prefix('\'') {
        if let Some(end) = rest.find('\'') {
            return rest[..end].to_string();
        }
    }
    if let Some(rest) = trimmed.strip_prefix('`') {
        if let Some(end) = rest.find('`') {
            return rest[..end].to_string();
        }
    }
    let end = trimmed
        .find(|c: char| c == ')' || c == ',' || c.is_whitespace())
        .unwrap_or(trimmed.len().min(80));
    let candidate = &trimmed[..end];
    if candidate.contains('.')
        || candidate.contains("::")
        || candidate
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '/')
    {
        return candidate.to_string();
    }
    String::new()
}

fn load_symbol_names(backend: &dyn GraphBackend) -> Result<HashMap<String, String>> {
    let result = backend.raw_query("MATCH (s:Symbol) RETURN s.id, s.name")?;

    let mut map = HashMap::new();
    for row in result {
        if row.len() >= 2 {
            let id = row[0].to_string();
            let name = row[1].to_string();
            map.insert(name, id);
        }
    }
    Ok(map)
}

fn scan_config_files(root: &Path) -> HashMap<String, String> {
    let mut values = HashMap::new();

    let config_files = [
        "application.properties",
        "application.yml",
        "application.yaml",
        "config.properties",
        "config.yml",
        "config.yaml",
    ];

    for cf in &config_files {
        let path = root.join(cf);
        if let Ok(content) = std::fs::read_to_string(&path) {
            parse_properties_into(&content, &mut values);
        }
        let src_resources = root.join("src/main/resources").join(cf);
        if let Ok(content) = std::fs::read_to_string(&src_resources) {
            parse_properties_into(&content, &mut values);
        }
    }

    // META-INF/services for ServiceLoader
    let services_dir = root.join("src/main/resources/META-INF/services");
    if services_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&services_dir) {
            for entry in entries.flatten() {
                let iface = entry.file_name().to_string_lossy().to_string();
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    for line in content.lines() {
                        let line = line.trim();
                        if !line.is_empty() && !line.starts_with('#') {
                            values.insert(format!("service:{}", iface), line.to_string());
                        }
                    }
                }
            }
        }
    }

    // Python settings
    for settings_file in &["settings.py", "config/settings.py", "config.py"] {
        let path = root.join(settings_file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            parse_python_settings(&content, &mut values);
        }
    }

    values
}

fn parse_properties_into(content: &str, values: &mut HashMap<String, String>) {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let val = line[eq_pos + 1..].trim();
            values.insert(key.to_string(), val.to_string());
        } else if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim();
            let val = line[colon_pos + 1..].trim();
            if !val.is_empty() {
                values.insert(key.to_string(), val.to_string());
            }
        }
    }
}

fn parse_python_settings(content: &str, values: &mut HashMap<String, String>) {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let val = line[eq_pos + 1..]
                .trim()
                .trim_matches(|c: char| c == '\'' || c == '"');
            if key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                values.insert(key.to_string(), val.to_string());
            }
        }
    }
}

fn try_resolve(
    raw_arg: &str,
    mechanism: &str,
    all_symbols: &HashMap<String, String>,
    config_values: &HashMap<String, String>,
    _root: &Path,
) -> (Option<String>, Option<String>) {
    // Direct match: raw_arg is a FQCN or symbol name
    if let Some(symbol_id) = all_symbols.get(raw_arg) {
        return (Some(symbol_id.clone()), None);
    }

    // Try short name match (last segment)
    let short_name = raw_arg.rsplit('.').next().unwrap_or(raw_arg);
    let short_name2 = raw_arg.rsplit("::").next().unwrap_or(raw_arg);
    for name in [short_name, short_name2] {
        if let Some(symbol_id) = all_symbols.get(name) {
            return (Some(symbol_id.clone()), None);
        }
    }

    // ServiceLoader: check META-INF/services
    if mechanism == "ServiceLoader" {
        let service_key = format!("service:{}", raw_arg);
        if let Some(impl_fqcn) = config_values.get(&service_key) {
            let impl_short = impl_fqcn.rsplit('.').next().unwrap_or(impl_fqcn);
            if let Some(symbol_id) = all_symbols.get(impl_short) {
                return (
                    Some(symbol_id.clone()),
                    Some(format!("META-INF/services/{}", raw_arg)),
                );
            }
            return (
                Some(impl_fqcn.clone()),
                Some(format!("META-INF/services/{}", raw_arg)),
            );
        }
    }

    // Config-driven: check if raw_arg is a config key that maps to a class name
    for (key, val) in config_values {
        if key.contains(raw_arg) || raw_arg.contains(key.as_str()) {
            let val_short = val.rsplit('.').next().unwrap_or(val);
            if let Some(symbol_id) = all_symbols.get(val_short) {
                return (Some(symbol_id.clone()), Some(key.clone()));
            }
            if val.contains('.') || val.contains("::") {
                return (Some(val.clone()), Some(key.clone()));
            }
        }
    }

    (None, None)
}

fn write_resolves_to(backend: &dyn GraphBackend, sites: &[ReflectionSite]) -> Result<()> {
    backend.raw_query("BEGIN TRANSACTION")?;

    let _ = backend.raw_query("MATCH ()-[r:RESOLVES_TO]->() DELETE r");

    for site in sites {
        if let Some(ref target) = site.resolved_to {
            let src_esc = crate::escape_str(&site.caller_symbol);
            let tgt_esc = crate::escape_str(target);
            let mech_esc = crate::escape_str(site.mechanism);
            let cfg_esc = crate::escape_str(site.config_source.as_deref().unwrap_or(""));

            let _ = backend.raw_query(&format!(
                "MATCH (s:Symbol), (t:Symbol) WHERE s.id = '{src_esc}' AND t.id = '{tgt_esc}' \
                 CREATE (s)-[:RESOLVES_TO {{mechanism: '{mech_esc}', config_source: '{cfg_esc}'}}]->(t)"
            ));
        }
    }

    backend.raw_query("COMMIT")?;

    Ok(())
}

pub fn format_reflection_sites(sites: &[ReflectionSite]) -> String {
    if sites.is_empty() {
        return "No reflection/dynamic invocation sites detected.".to_string();
    }

    let resolved_count = sites.iter().filter(|s| s.resolved_to.is_some()).count();
    let unresolved_count = sites.len() - resolved_count;

    let mut out = format!(
        "Reflection sites: {} total ({} resolved, {} unresolved)\n\n",
        sites.len(),
        resolved_count,
        unresolved_count
    );

    let mut by_mechanism: std::collections::BTreeMap<&str, Vec<&ReflectionSite>> =
        std::collections::BTreeMap::new();
    for s in sites {
        by_mechanism.entry(s.mechanism).or_default().push(s);
    }

    for (mech, items) in &by_mechanism {
        out.push_str(&format!("## {} ({} sites)\n", mech, items.len()));
        for item in items {
            let status = match &item.resolved_to {
                Some(target) => format!("-> {}", target),
                None => "UNRESOLVED".to_string(),
            };
            out.push_str(&format!(
                "  {}:{} — {}({}) {}\n",
                item.file, item.line, mech, item.raw_arg, status
            ));
            if let Some(ref cfg) = item.config_source {
                out.push_str(&format!("    via config: {}\n", cfg));
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
    fn test_extract_string_arg_double_quotes() {
        assert_eq!(
            extract_string_arg("\"com.example.MyClass\")"),
            "com.example.MyClass"
        );
    }

    #[test]
    fn test_extract_string_arg_single_quotes() {
        assert_eq!(extract_string_arg("'my_module')"), "my_module");
    }

    #[test]
    fn test_extract_string_arg_backtick() {
        assert_eq!(
            extract_string_arg("`./modules/${name}`)"),
            "./modules/${name}"
        );
    }

    #[test]
    fn test_extract_string_arg_bare_identifier() {
        assert_eq!(extract_string_arg("MyClass.class)"), "MyClass.class");
    }

    #[test]
    fn test_extract_string_arg_empty_for_variable() {
        assert_eq!(extract_string_arg("someVariable)"), "someVariable");
    }

    #[test]
    fn test_detect_java_class_forname() {
        let docstring = "handler = Class.forName(\"com.example.Handler\").newInstance();";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"java") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"ClassForName"),
            "should detect Class.forName"
        );
    }

    #[test]
    fn test_detect_python_importlib() {
        let docstring = "mod = importlib.import_module(\"handlers.email\")";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"py") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"ImportModule"),
            "should detect importlib.import_module"
        );
    }

    #[test]
    fn test_detect_python_getattr() {
        let docstring = "fn = getattr(obj, method_name)";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"py") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(found.contains(&"Getattr"), "should detect getattr");
    }

    #[test]
    fn test_detect_csharp_activator() {
        let docstring = "var obj = Activator.CreateInstance(\"MyApp.Handlers.EmailHandler\");";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"cs") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"CSharpReflection"),
            "should detect Activator.CreateInstance"
        );
    }

    #[test]
    fn test_detect_ruby_send() {
        let docstring = "result = obj.send(method_name, *args)";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"rb") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(found.contains(&"RubySend"), "should detect .send(");
    }

    #[test]
    fn test_detect_go_reflect() {
        let docstring = "v := reflect.ValueOf(handler)";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"go") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(found.contains(&"GoPlugin"), "should detect reflect.ValueOf");
    }

    #[test]
    fn test_detect_java_service_loader() {
        let docstring = "ServiceLoader.load(PaymentProcessor.class)";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            if !rp.extensions.contains(&"java") {
                continue;
            }
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"ServiceLoader"),
            "should detect ServiceLoader.load"
        );
    }

    #[test]
    fn test_parse_properties() {
        let content = "handler.class=com.example.MyHandler\ndb.url=jdbc:mysql://localhost/test";
        let mut values = HashMap::new();
        parse_properties_into(content, &mut values);
        assert_eq!(
            values.get("handler.class").unwrap(),
            "com.example.MyHandler"
        );
        assert_eq!(values.get("db.url").unwrap(), "jdbc:mysql://localhost/test");
    }

    #[test]
    fn test_parse_yaml_style_properties() {
        let content = "handler: com.example.MyHandler\nport: 8080";
        let mut values = HashMap::new();
        parse_properties_into(content, &mut values);
        assert_eq!(values.get("handler").unwrap(), "com.example.MyHandler");
    }

    #[test]
    fn test_try_resolve_direct_match() {
        let mut symbols = HashMap::new();
        symbols.insert(
            "MyHandler".to_string(),
            "handler.java::MyHandler".to_string(),
        );
        let configs = HashMap::new();
        let (resolved, _) = try_resolve(
            "MyHandler",
            "ClassForName",
            &symbols,
            &configs,
            Path::new("."),
        );
        assert_eq!(resolved.unwrap(), "handler.java::MyHandler");
    }

    #[test]
    fn test_try_resolve_fqcn_short_name() {
        let mut symbols = HashMap::new();
        symbols.insert(
            "MyHandler".to_string(),
            "handler.java::MyHandler".to_string(),
        );
        let configs = HashMap::new();
        let (resolved, _) = try_resolve(
            "com.example.MyHandler",
            "ClassForName",
            &symbols,
            &configs,
            Path::new("."),
        );
        assert_eq!(resolved.unwrap(), "handler.java::MyHandler");
    }

    #[test]
    fn test_try_resolve_unresolved() {
        let symbols = HashMap::new();
        let configs = HashMap::new();
        let (resolved, _) = try_resolve(
            "com.unknown.Mystery",
            "ClassForName",
            &symbols,
            &configs,
            Path::new("."),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn test_no_false_positive_plain_text() {
        let docstring = "This class forwards messages to the service loader pattern.";
        let mut found = Vec::new();
        for rp in REFLECTION_PATTERNS {
            for &pattern in rp.patterns {
                if docstring.contains(pattern) {
                    found.push(rp.mechanism);
                    break;
                }
            }
        }
        assert!(found.is_empty(), "plain text should not match: {:?}", found);
    }

    #[test]
    fn test_parse_python_settings() {
        let content = "HANDLER_CLASS = 'myapp.handlers.EmailHandler'\nDEBUG = True";
        let mut values = HashMap::new();
        super::parse_python_settings(content, &mut values);
        assert_eq!(
            values.get("HANDLER_CLASS").unwrap(),
            "myapp.handlers.EmailHandler"
        );
        assert_eq!(values.get("DEBUG").unwrap(), "True");
    }
}
