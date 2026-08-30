use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use infigraph_core::lang::CustomExtractor;
use infigraph_core::model::{Relation, RelationKind, Span, Symbol, SymbolKind};

use crate::driver::GrammarDriver;

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarPluginConfig {
    pub language: LanguageMeta,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LanguageMeta {
    pub name: String,
    pub extensions: Vec<String>,
    pub entry_rule: String,
    pub lexer: String,
    pub parser: String,
    pub preprocessor: Option<String>,
    pub extractor: String,
    #[serde(default)]
    pub emit_referenced_form_imports: bool,
    #[serde(default)]
    pub pipe_strings: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectPreprocessorConfig {
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    pub preprocessor: Option<ProjectPreprocessorConfig>,
}

pub struct GrammarPlugin {
    pub config: GrammarPluginConfig,
    pub plugin_dir: PathBuf,
    driver: Arc<GrammarDriver>,
    project_preprocessor: Option<ProjectPreprocessorConfig>,
}

impl GrammarPlugin {
    pub fn new(
        config: GrammarPluginConfig,
        plugin_dir: PathBuf,
        driver: Arc<GrammarDriver>,
        project_preprocessor: Option<ProjectPreprocessorConfig>,
    ) -> Self {
        Self {
            config,
            plugin_dir,
            driver,
            project_preprocessor,
        }
    }

    pub fn load(&self) -> Result<()> {
        let lexer_path = self.plugin_dir.join(&self.config.language.lexer);
        let parser_path = self.plugin_dir.join(&self.config.language.parser);
        self.driver.load_grammar(
            &self.config.language.name,
            lexer_path.to_str().context("Invalid lexer path")?,
            parser_path.to_str().context("Invalid parser path")?,
            &crate::driver::LoadGrammarOptions {
                entry_rule: &self.config.language.entry_rule,
                preprocessor: self.config.language.preprocessor.as_deref(),
                emit_referenced_form_imports: self.config.language.emit_referenced_form_imports,
                pipe_strings: self.config.language.pipe_strings,
            },
        )?;

        let resolved_extractor =
            resolve_extractor(&self.plugin_dir, &self.config.language.extractor)?;
        self.driver
            .set_extractor(&self.config.language.name, &resolved_extractor)?;

        Ok(())
    }

    pub fn extract(&self, path: &str, source: &[u8]) -> Result<(Vec<Symbol>, Vec<Relation>)> {
        let source_str = std::str::from_utf8(source)?;

        let (defines, include_paths) = if self.config.language.preprocessor.is_some() {
            if let Some(ref pp_config) = self.project_preprocessor {
                (
                    Some(pp_config.defines.join(",")),
                    Some(pp_config.include_paths.join(",")),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let resp = self.driver.extract(
            &self.config.language.name,
            path,
            source_str,
            defines.as_deref(),
            include_paths.as_deref(),
        )?;
        let language = &self.config.language.name;

        let symbols = resp
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some(Symbol {
                            scip_id: None,
                            id: s.get("id")?.as_str()?.to_string(),
                            name: s.get("name")?.as_str()?.to_string(),
                            kind: parse_symbol_kind(s.get("kind")?.as_str()?),
                            span: Span {
                                file: s.get("file")?.as_str()?.to_string(),
                                start_line: s.get("start_line")?.as_u64()? as u32,
                                start_col: s.get("start_col")?.as_u64()? as u32,
                                end_line: s.get("end_line")?.as_u64()? as u32,
                                end_col: s.get("end_col")?.as_u64()? as u32,
                            },
                            signature_hash: s
                                .get("signature_hash")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0000000000000000")
                                .to_string(),
                            parent: s
                                .get("parent")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            language: language.clone(),
                            visibility: None,
                            docstring: None,
                            complexity: 0,
                            parameters: None,
                            return_type: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let relations = resp
            .get("relations")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        Some(Relation {
                            source_id: r.get("source_id")?.as_str()?.to_string(),
                            target_id: r.get("target_id")?.as_str()?.to_string(),
                            kind: parse_relation_kind(r.get("kind")?.as_str()?),
                            span: Some(Span {
                                file: r.get("file")?.as_str()?.to_string(),
                                start_line: r.get("start_line")?.as_u64()? as u32,
                                start_col: r.get("start_col")?.as_u64()? as u32,
                                end_line: r.get("end_line")?.as_u64()? as u32,
                                end_col: r.get("end_col")?.as_u64()? as u32,
                            }),
                            receiver: r
                                .get("receiver")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok((symbols, relations))
    }
}

impl CustomExtractor for GrammarPlugin {
    fn extract(
        &self,
        path: &str,
        source: &[u8],
        _language: &str,
    ) -> Result<(Vec<Symbol>, Vec<Relation>)> {
        self.extract(path, source)
    }
}

pub fn discover_plugins(plugins_dir: &Path) -> Result<Vec<(GrammarPluginConfig, PathBuf)>> {
    let mut plugins = Vec::new();

    if !plugins_dir.exists() {
        return Ok(plugins);
    }

    for entry in std::fs::read_dir(plugins_dir)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let config_path = dir.join("plugin.toml");
        if !config_path.exists() {
            continue;
        }
        let config_str = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let config: GrammarPluginConfig = toml::from_str(&config_str)
            .with_context(|| format!("Failed to parse {}", config_path.display()))?;

        let lexer_path = dir.join(&config.language.lexer);
        let parser_path = dir.join(&config.language.parser);
        if !lexer_path.exists() {
            eprintln!(
                "[infigraph] Plugin '{}': lexer grammar not found: {}",
                config.language.name,
                lexer_path.display()
            );
            continue;
        }
        if !parser_path.exists() {
            eprintln!(
                "[infigraph] Plugin '{}': parser grammar not found: {}",
                config.language.name,
                parser_path.display()
            );
            continue;
        }

        plugins.push((config, dir));
    }

    Ok(plugins)
}

/// Resolves the `extractor` field to what the driver expects: a `.java`
/// source file is resolved to an absolute path next to `plugin.toml` (the
/// driver compiles it on load); anything else (e.g. `"GenericExtractor"`)
/// passes through unchanged.
fn resolve_extractor(plugin_dir: &Path, extractor: &str) -> Result<String> {
    if extractor.ends_with(".java") {
        let joined = plugin_dir.join(extractor);
        let s = joined
            .to_str()
            .context("Invalid extractor path")?
            .replace('\\', "/");
        Ok(s)
    } else {
        Ok(extractor.to_string())
    }
}

fn parse_symbol_kind(s: &str) -> SymbolKind {
    match s {
        "Function" => SymbolKind::Function,
        "Method" => SymbolKind::Method,
        "Class" => SymbolKind::Class,
        "Struct" => SymbolKind::Struct,
        "Interface" => SymbolKind::Interface,
        "Trait" => SymbolKind::Trait,
        "Enum" => SymbolKind::Enum,
        "Module" => SymbolKind::Module,
        "Variable" => SymbolKind::Variable,
        "Constant" => SymbolKind::Constant,
        "Test" => SymbolKind::Test,
        "Section" => SymbolKind::Section,
        "Route" => SymbolKind::Route,
        "Field" => SymbolKind::Field,
        _ => SymbolKind::Function,
    }
}

fn parse_relation_kind(s: &str) -> RelationKind {
    match s {
        "Calls" => RelationKind::Calls,
        "Imports" => RelationKind::Imports,
        "Inherits" => RelationKind::Inherits,
        "Implements" => RelationKind::Implements,
        "Contains" => RelationKind::Contains,
        "Reads" => RelationKind::Reads,
        "Writes" => RelationKind::Writes,
        _ => RelationKind::Calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol_kind_all_variants() {
        assert_eq!(parse_symbol_kind("Function"), SymbolKind::Function);
        assert_eq!(parse_symbol_kind("Method"), SymbolKind::Method);
        assert_eq!(parse_symbol_kind("Class"), SymbolKind::Class);
        assert_eq!(parse_symbol_kind("Struct"), SymbolKind::Struct);
        assert_eq!(parse_symbol_kind("Interface"), SymbolKind::Interface);
        assert_eq!(parse_symbol_kind("Trait"), SymbolKind::Trait);
        assert_eq!(parse_symbol_kind("Enum"), SymbolKind::Enum);
        assert_eq!(parse_symbol_kind("Module"), SymbolKind::Module);
        assert_eq!(parse_symbol_kind("Variable"), SymbolKind::Variable);
        assert_eq!(parse_symbol_kind("Constant"), SymbolKind::Constant);
        assert_eq!(parse_symbol_kind("Test"), SymbolKind::Test);
        assert_eq!(parse_symbol_kind("Section"), SymbolKind::Section);
        assert_eq!(parse_symbol_kind("Route"), SymbolKind::Route);
        assert_eq!(parse_symbol_kind("Field"), SymbolKind::Field);
    }

    #[test]
    fn test_parse_symbol_kind_unknown_defaults() {
        assert_eq!(parse_symbol_kind("unknown_kind"), SymbolKind::Function);
        assert_eq!(parse_symbol_kind(""), SymbolKind::Function);
    }

    #[test]
    fn test_parse_relation_kind_all_variants() {
        assert_eq!(parse_relation_kind("Calls"), RelationKind::Calls);
        assert_eq!(parse_relation_kind("Imports"), RelationKind::Imports);
        assert_eq!(parse_relation_kind("Inherits"), RelationKind::Inherits);
        assert_eq!(parse_relation_kind("Implements"), RelationKind::Implements);
        assert_eq!(parse_relation_kind("Contains"), RelationKind::Contains);
        assert_eq!(parse_relation_kind("Reads"), RelationKind::Reads);
        assert_eq!(parse_relation_kind("Writes"), RelationKind::Writes);
    }

    #[test]
    fn test_parse_relation_kind_unknown_defaults() {
        assert_eq!(parse_relation_kind("unknown_rel"), RelationKind::Calls);
        assert_eq!(parse_relation_kind(""), RelationKind::Calls);
    }

    #[test]
    fn test_plugin_config_deserialize() {
        let toml_str = r#"
[language]
name = "cobol"
extensions = [".cob", ".cbl"]
entry_rule = "program"
lexer = "CobolLexer.g4"
parser = "CobolParser.g4"
extractor = "CobolExtractor"
"#;
        let config: GrammarPluginConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.language.name, "cobol");
        assert_eq!(config.language.extensions, vec![".cob", ".cbl"]);
        assert_eq!(config.language.entry_rule, "program");
        assert_eq!(config.language.lexer, "CobolLexer.g4");
        assert_eq!(config.language.parser, "CobolParser.g4");
        assert_eq!(config.language.extractor, "CobolExtractor");
    }

    #[test]
    fn test_plugin_config_optional_fields() {
        let toml_str = r#"
[language]
name = "plsql"
extensions = [".sql"]
entry_rule = "compilation_unit"
lexer = "PlSqlLexer.g4"
parser = "PlSqlParser.g4"
extractor = "PlSqlExtractor"
"#;
        let config: GrammarPluginConfig = toml::from_str(toml_str).unwrap();
        assert!(config.language.preprocessor.is_none());
        assert!(!config.language.emit_referenced_form_imports);
        assert!(!config.language.pipe_strings);
    }

    #[test]
    fn test_plugin_config_pipe_strings_enabled() {
        let toml_str = r#"
[language]
name = "interview"
extensions = [".int"]
entry_rule = "program"
lexer = "InterviewLexer.g4"
parser = "InterviewParser.g4"
extractor = "InterviewExtractor.java"
preprocessor = "c"
pipe_strings = true
"#;
        let config: GrammarPluginConfig = toml::from_str(toml_str).unwrap();
        assert!(config.language.pipe_strings);
    }

    #[test]
    fn test_plugin_config_with_all_fields() {
        let toml_str = r#"
[language]
name = "vb6"
extensions = [".frm", ".bas", ".cls"]
entry_rule = "startRule"
lexer = "VisualBasic6Lexer.g4"
parser = "VisualBasic6Parser.g4"
preprocessor = "Vb6Preprocessor"
extractor = "Vb6Extractor"
emit_referenced_form_imports = true
"#;
        let config: GrammarPluginConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.language.preprocessor.as_deref(),
            Some("Vb6Preprocessor")
        );
        assert_eq!(config.language.extractor, "Vb6Extractor");
        assert!(config.language.emit_referenced_form_imports);
    }

    #[test]
    fn test_resolve_extractor_java_source_resolves_to_absolute_path() {
        let plugin_dir = Path::new("/plugins/interview");
        let resolved = resolve_extractor(plugin_dir, "InterviewExtractor.java").unwrap();
        assert_eq!(resolved, "/plugins/interview/InterviewExtractor.java");
    }

    #[test]
    fn test_resolve_extractor_generic_extractor_passes_through() {
        let plugin_dir = Path::new("/plugins/interview");
        let resolved = resolve_extractor(plugin_dir, "GenericExtractor").unwrap();
        assert_eq!(resolved, "GenericExtractor");
    }

    #[test]
    fn test_discover_plugins_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugins = discover_plugins(dir.path()).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_discover_plugins_finds_plugin_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin_dir = dir.path().join("my-grammar");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
[language]
name = "test-lang"
extensions = [".tst"]
entry_rule = "start"
lexer = "TestLexer.g4"
parser = "TestParser.g4"
extractor = "TestExtractor"
"#,
        )
        .unwrap();
        std::fs::write(plugin_dir.join("TestLexer.g4"), "").unwrap();
        std::fs::write(plugin_dir.join("TestParser.g4"), "").unwrap();
        let plugins = discover_plugins(dir.path()).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].0.language.name, "test-lang");
    }

    #[test]
    fn test_discover_plugins_skips_missing_grammar_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin_dir = dir.path().join("no-grammar");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
[language]
name = "missing"
extensions = [".miss"]
entry_rule = "start"
lexer = "Missing.g4"
parser = "MissingParser.g4"
extractor = "X"
"#,
        )
        .unwrap();
        let plugins = discover_plugins(dir.path()).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_project_config_deserialize() {
        let toml_str = r#"
[preprocessor]
defines = ["WIN32", "DEBUG"]
include_paths = ["/usr/include", "./vendor"]
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let pp = config.preprocessor.unwrap();
        assert_eq!(pp.defines, vec!["WIN32", "DEBUG"]);
        assert_eq!(pp.include_paths, vec!["/usr/include", "./vendor"]);
    }

    #[test]
    fn test_project_config_no_preprocessor() {
        let toml_str = "";
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert!(config.preprocessor.is_none());
    }
}
