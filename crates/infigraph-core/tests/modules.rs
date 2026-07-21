use infigraph_core::graph::{GraphStore, KuzuBackend};
use infigraph_core::model::{FileExtraction, Relation, RelationKind, Span, Symbol, SymbolKind};

fn span(file: &str, start: u32, end: u32) -> Span {
    Span {
        file: file.to_string(),
        start_line: start,
        start_col: 0,
        end_line: end,
        end_col: 0,
    }
}

fn sym(id: &str, name: &str, kind: SymbolKind, file: &str, start: u32, end: u32) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        span: span(file, start, end),
        signature_hash: format!("h_{id}"),
        parent: None,
        language: "python".to_string(),
        visibility: Some("public".to_string()),
        docstring: None,
        complexity: 1,
        parameters: None,
        return_type: None,
    }
}

fn sym_complex(
    id: &str,
    name: &str,
    kind: SymbolKind,
    file: &str,
    start: u32,
    end: u32,
    complexity: u32,
) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        span: span(file, start, end),
        signature_hash: format!("h_{id}"),
        parent: None,
        language: "python".to_string(),
        visibility: Some("public".to_string()),
        docstring: None,
        complexity,
        parameters: None,
        return_type: None,
    }
}

fn rel(src: &str, tgt: &str, kind: RelationKind) -> Relation {
    Relation {
        source_id: src.to_string(),
        target_id: tgt.to_string(),
        kind,
        span: None,
        receiver: None,
    }
}

struct TestGraph {
    _dir: tempfile::TempDir,
    backend: KuzuBackend,
}

fn setup_graph() -> TestGraph {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let extractions = vec![
        FileExtraction {
            file: "src/app.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![
                sym(
                    "src/app.py::main",
                    "main",
                    SymbolKind::Function,
                    "src/app.py",
                    1,
                    30,
                ),
                sym(
                    "src/app.py::helper",
                    "helper",
                    SymbolKind::Function,
                    "src/app.py",
                    32,
                    50,
                ),
                sym_complex(
                    "src/app.py::complex_handler",
                    "complex_handler",
                    SymbolKind::Function,
                    "src/app.py",
                    52,
                    200,
                    25,
                ),
            ],
            relations: vec![
                rel(
                    "src/app.py::main",
                    "src/app.py::helper",
                    RelationKind::Calls,
                ),
                rel(
                    "src/app.py::main",
                    "src/app.py::complex_handler",
                    RelationKind::Calls,
                ),
                rel(
                    "src/app.py::complex_handler",
                    "src/app.py::helper",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "src/models.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![
                sym(
                    "src/models.py::BaseModel",
                    "BaseModel",
                    SymbolKind::Class,
                    "src/models.py",
                    1,
                    20,
                ),
                sym(
                    "src/models.py::UserModel",
                    "UserModel",
                    SymbolKind::Class,
                    "src/models.py",
                    22,
                    50,
                ),
                sym(
                    "src/models.py::UserModel::save",
                    "save",
                    SymbolKind::Method,
                    "src/models.py",
                    30,
                    45,
                ),
            ],
            relations: vec![
                rel(
                    "src/models.py::UserModel",
                    "src/models.py::BaseModel",
                    RelationKind::Inherits,
                ),
                rel(
                    "src/app.py::complex_handler",
                    "src/models.py::UserModel::save",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "src/utils.py".to_string(),
            language: "python".to_string(),
            content_hash: "c".to_string(),
            symbols: vec![
                sym(
                    "src/utils.py::format_date",
                    "format_date",
                    SymbolKind::Function,
                    "src/utils.py",
                    1,
                    10,
                ),
                sym(
                    "src/utils.py::validate",
                    "validate",
                    SymbolKind::Function,
                    "src/utils.py",
                    12,
                    25,
                ),
            ],
            relations: vec![],
            statements: vec![],
        },
    ];
    {
        let conn = store.connection().unwrap();
        store.upsert_all_bulk(&conn, &extractions).unwrap();
    }
    TestGraph {
        _dir: dir,
        backend: KuzuBackend::from_store(store),
    }
}

// ============================================================
// Cluster detection tests
// ============================================================

#[test]
fn test_cluster_empty_graph() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);

    let stats = infigraph_core::cluster::detect_clusters(&backend).unwrap();
    assert_eq!(stats.num_clusters, 0);
    assert!(stats.cluster_sizes.is_empty());
    assert_eq!(stats.modularity, 0.0);
}

#[test]
fn test_cluster_with_connected_graph() {
    let tg = setup_graph();

    let stats = infigraph_core::cluster::detect_clusters(&tg.backend).unwrap();
    assert!(stats.num_clusters >= 1, "should find at least 1 cluster");
    let total_members: usize = stats.cluster_sizes.iter().sum();
    assert!(total_members >= 3, "clusters should contain symbols");
}

#[test]
fn test_cluster_stats_display() {
    let tg = setup_graph();

    let stats = infigraph_core::cluster::detect_clusters(&tg.backend).unwrap();
    let display = format!("{stats}");
    assert!(display.contains("Cluster Statistics:"));
    assert!(display.contains("Modularity:"));
    assert!(display.contains("Top sizes:"));
}

#[test]
fn test_cluster_isolated_symbols() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let extractions = vec![FileExtraction {
        file: "a.py".to_string(),
        language: "python".to_string(),
        content_hash: "a".to_string(),
        symbols: vec![
            sym("a.py::foo", "foo", SymbolKind::Function, "a.py", 1, 5),
            sym("a.py::bar", "bar", SymbolKind::Function, "a.py", 7, 10),
        ],
        relations: vec![],
        statements: vec![],
    }];
    {
        let conn = store.connection().unwrap();
        store.upsert_all_bulk(&conn, &extractions).unwrap();
    }
    let backend = KuzuBackend::from_store(store);
    let stats = infigraph_core::cluster::detect_clusters(&backend).unwrap();
    assert!(
        stats.num_clusters >= 2,
        "isolated symbols should each be own cluster"
    );
}

// ============================================================
// Manifest parsing tests
// ============================================================

#[test]
fn test_manifest_package_json() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies": {"express": "^4.18.0"}, "devDependencies": {"jest": "^29.0.0"}}"#,
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "npm");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "express" && !d.is_dev));
    assert!(results[0].deps.iter().any(|d| d.name == "jest" && d.is_dev));
}

#[test]
fn test_manifest_cargo_toml() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
serde = "1.0"
anyhow = { version = "1.0" }

[dev-dependencies]
tempfile = "3.0"
"#,
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "cargo");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "serde" && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "tempfile" && d.is_dev));
}

#[test]
fn test_manifest_requirements_txt() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("requirements.txt"),
        "flask==2.0.1\nrequests>=2.25.0\npytest\n# comment\n",
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "pip");
    assert_eq!(results[0].deps.len(), 3);
    assert!(results[0].deps.iter().any(|d| d.name == "flask"));
    assert!(results[0].deps.iter().any(|d| d.name == "requests"));
    assert!(results[0].deps.iter().any(|d| d.name == "pytest"));
}

#[test]
fn test_manifest_go_mod() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/myapp\n\ngo 1.21\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgolang.org/x/sync v0.3.0 // indirect\n)\n",
    ).unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "go");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name.contains("gin-gonic") && !d.is_dev));
    assert!(
        results[0]
            .deps
            .iter()
            .any(|d| d.name.contains("golang.org")),
        "should parse indirect dependency"
    );
}

#[test]
fn test_manifest_pom_xml() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("pom.xml"),
        r#"<project>
  <dependencies>
    <dependency>
      <groupId>org.springframework</groupId>
      <artifactId>spring-core</artifactId>
      <version>5.3.0</version>
    </dependency>
    <dependency>
      <groupId>junit</groupId>
      <artifactId>junit</artifactId>
      <version>4.13</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>"#,
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "maven");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name.contains("spring-core") && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name.contains("junit") && d.is_dev));
}

#[test]
fn test_manifest_gemfile() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Gemfile"),
        "source 'https://rubygems.org'\ngem 'rails', '~> 7.0'\ngem 'puma'\n\ngroup :test do\n  gem 'rspec'\nend\n",
    ).unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "gem");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "rails" && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "rspec" && d.is_dev));
}

#[test]
fn test_manifest_composer_json() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{"require": {"laravel/framework": "^10.0"}, "require-dev": {"phpunit/phpunit": "^10.0"}}"#,
    ).unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "composer");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name.contains("laravel") && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name.contains("phpunit") && d.is_dev));
}

#[test]
fn test_manifest_empty_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_manifest_query_deps() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies": {"express": "^4.18.0"}}"#,
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    let deps = infigraph_core::manifest::query_deps(&backend).unwrap();
    assert!(deps.iter().any(|d| d.name == "express"));
}

#[test]
fn test_manifest_gradle() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("build.gradle"),
        "dependencies {\n    implementation 'com.google.guava:guava:31.1'\n    testImplementation 'junit:junit:4.13'\n}\n",
    ).unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "gradle");
    assert!(results[0].deps.iter().any(|d| d.name.contains("guava")));
}

#[test]
fn test_manifest_pubspec_yaml() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("pubspec.yaml"),
        "name: myapp\ndependencies:\n  http: ^0.13.0\n  provider: ^6.0.0\ndev_dependencies:\n  flutter_test: any\n",
    ).unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ecosystem, "pub");
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "http" && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "flutter_test" && d.is_dev));
}

#[test]
fn test_manifest_packages_config() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("packages.config"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<packages>
  <package id="Newtonsoft.Json" version="13.0.1" />
  <package id="xunit" version="2.4.1" developmentDependency="true" />
</packages>"#,
    )
    .unwrap();

    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);
    let results = infigraph_core::manifest::index_manifests(dir.path(), &backend).unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "Newtonsoft.Json" && !d.is_dev));
    assert!(results[0]
        .deps
        .iter()
        .any(|d| d.name == "xunit" && d.is_dev));
}

// ============================================================
// Vuln report formatting and filtering tests
// ============================================================

#[test]
fn test_vuln_format_table_empty() {
    let report = infigraph_core::vuln::VulnReport {
        total_deps: 10,
        vulnerable_deps: 0,
        findings: vec![],
    };
    let table = infigraph_core::vuln::format_table(&report);
    assert!(table.contains("No vulnerabilities found"));
    assert!(table.contains("10"));
}

#[test]
fn test_vuln_format_table_with_findings() {
    let report = infigraph_core::vuln::VulnReport {
        total_deps: 5,
        vulnerable_deps: 1,
        findings: vec![infigraph_core::vuln::VulnEntry {
            dep_name: "lodash".to_string(),
            dep_version: "4.17.15".to_string(),
            ecosystem: "npm".to_string(),
            vuln_id: "GHSA-xxxx".to_string(),
            summary: "Prototype pollution".to_string(),
            severity: "HIGH".to_string(),
            fixed_version: Some("4.17.21".to_string()),
            url: "https://example.com".to_string(),
        }],
    };
    let table = infigraph_core::vuln::format_table(&report);
    assert!(table.contains("lodash"));
    assert!(table.contains("HIGH"));
    assert!(table.contains("1 vulnerable"));
}

#[test]
fn test_vuln_format_json() {
    let report = infigraph_core::vuln::VulnReport {
        total_deps: 2,
        vulnerable_deps: 1,
        findings: vec![infigraph_core::vuln::VulnEntry {
            dep_name: "pkg".to_string(),
            dep_version: "1.0".to_string(),
            ecosystem: "npm".to_string(),
            vuln_id: "CVE-2024-0001".to_string(),
            summary: "test vuln".to_string(),
            severity: "CRITICAL".to_string(),
            fixed_version: None,
            url: "https://example.com".to_string(),
        }],
    };
    let json = infigraph_core::vuln::format_json(&report);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["total_deps"], 2);
    assert_eq!(parsed["findings"][0]["vuln_id"], "CVE-2024-0001");
}

#[test]
fn test_vuln_filter_by_severity() {
    let mut report = infigraph_core::vuln::VulnReport {
        total_deps: 5,
        vulnerable_deps: 3,
        findings: vec![
            infigraph_core::vuln::VulnEntry {
                dep_name: "a".to_string(),
                dep_version: "1.0".to_string(),
                ecosystem: "npm".to_string(),
                vuln_id: "V1".to_string(),
                summary: "".to_string(),
                severity: "CRITICAL".to_string(),
                fixed_version: None,
                url: "".to_string(),
            },
            infigraph_core::vuln::VulnEntry {
                dep_name: "b".to_string(),
                dep_version: "1.0".to_string(),
                ecosystem: "npm".to_string(),
                vuln_id: "V2".to_string(),
                summary: "".to_string(),
                severity: "HIGH".to_string(),
                fixed_version: None,
                url: "".to_string(),
            },
            infigraph_core::vuln::VulnEntry {
                dep_name: "c".to_string(),
                dep_version: "1.0".to_string(),
                ecosystem: "npm".to_string(),
                vuln_id: "V3".to_string(),
                summary: "".to_string(),
                severity: "LOW".to_string(),
                fixed_version: None,
                url: "".to_string(),
            },
        ],
    };

    infigraph_core::vuln::filter_by_severity(&mut report, "HIGH");
    assert_eq!(report.findings.len(), 2);
    assert!(report
        .findings
        .iter()
        .all(|f| f.severity == "CRITICAL" || f.severity == "HIGH"));
}

#[test]
fn test_vuln_filter_by_ecosystem() {
    let mut report = infigraph_core::vuln::VulnReport {
        total_deps: 4,
        vulnerable_deps: 2,
        findings: vec![
            infigraph_core::vuln::VulnEntry {
                dep_name: "a".to_string(),
                dep_version: "1.0".to_string(),
                ecosystem: "npm".to_string(),
                vuln_id: "V1".to_string(),
                summary: "".to_string(),
                severity: "HIGH".to_string(),
                fixed_version: None,
                url: "".to_string(),
            },
            infigraph_core::vuln::VulnEntry {
                dep_name: "b".to_string(),
                dep_version: "1.0".to_string(),
                ecosystem: "pip".to_string(),
                vuln_id: "V2".to_string(),
                summary: "".to_string(),
                severity: "HIGH".to_string(),
                fixed_version: None,
                url: "".to_string(),
            },
        ],
    };

    infigraph_core::vuln::filter_by_ecosystem(&mut report, "npm");
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].dep_name, "a");
}

// ============================================================
// Check config and runner tests
// ============================================================

#[test]
fn test_check_config_defaults() {
    let cfg = infigraph_core::check::CheckConfig::default();
    assert!(cfg.security.enabled);
    assert!(cfg.complexity.enabled);
    assert!(cfg.dead_code.enabled);
    assert!(!cfg.vulnerabilities.enabled);
    assert_eq!(cfg.complexity.threshold, 15);
}

#[test]
fn test_check_config_load_missing_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = infigraph_core::check::load_config(&dir.path().join("nonexistent.toml")).unwrap();
    assert!(cfg.security.enabled);
}

#[test]
fn test_check_config_load_from_toml() {
    let dir = tempfile::TempDir::new().unwrap();
    let config_path = dir.path().join("check.toml");
    std::fs::write(
        &config_path,
        "[security]\nenabled = false\nmax_critical = 5\nmax_high = 10\n\n[complexity]\nthreshold = 20\n",
    ).unwrap();

    let cfg = infigraph_core::check::load_config(&config_path).unwrap();
    assert!(!cfg.security.enabled);
    assert_eq!(cfg.security.max_critical, 5);
    assert_eq!(cfg.complexity.threshold, 20);
}

#[test]
fn test_check_selection_all() {
    let sel = infigraph_core::check::CheckSelection::all();
    assert!(sel.security);
    assert!(sel.complexity);
    assert!(sel.dead_code);
    assert!(sel.vulnerabilities);
}

#[test]
fn test_check_selection_from_csv() {
    let sel = infigraph_core::check::CheckSelection::from_csv("security,complexity");
    assert!(sel.security);
    assert!(sel.complexity);
    assert!(!sel.dead_code);
    assert!(!sel.vulnerabilities);
}

#[test]
fn test_check_selection_aliases() {
    let sel = infigraph_core::check::CheckSelection::from_csv("sec,cx,deadcode,vulns");
    assert!(sel.security);
    assert!(sel.complexity);
    assert!(sel.dead_code);
    assert!(sel.vulnerabilities);
}

#[test]
fn test_check_run_complexity() {
    let tg = setup_graph();
    let cfg = infigraph_core::check::CheckConfig {
        complexity: infigraph_core::check::ComplexityConfig {
            enabled: true,
            threshold: 10,
            max_violations: 0,
        },
        security: infigraph_core::check::SecurityConfig {
            enabled: false,
            ..Default::default()
        },
        dead_code: infigraph_core::check::DeadCodeConfig {
            enabled: false,
            ..Default::default()
        },
        vulnerabilities: infigraph_core::check::VulnCheckConfig {
            enabled: false,
            ..Default::default()
        },
    };
    let sel = infigraph_core::check::CheckSelection::from_csv("complexity");
    let results = infigraph_core::check::run_checks(tg._dir.path(), &cfg, &tg.backend, &sel);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "complexity");
    assert_eq!(
        results[0].status,
        infigraph_core::check::CheckStatus::Fail,
        "complex_handler has complexity 25 > threshold 10"
    );
}

#[test]
fn test_check_run_dead_code() {
    let tg = setup_graph();
    let cfg = infigraph_core::check::CheckConfig {
        dead_code: infigraph_core::check::DeadCodeConfig {
            enabled: true,
            max_dead: 0,
            ignore_patterns: vec!["main".into(), "test_*".into()],
        },
        security: infigraph_core::check::SecurityConfig {
            enabled: false,
            ..Default::default()
        },
        complexity: infigraph_core::check::ComplexityConfig {
            enabled: false,
            ..Default::default()
        },
        vulnerabilities: infigraph_core::check::VulnCheckConfig {
            enabled: false,
            ..Default::default()
        },
    };
    let sel = infigraph_core::check::CheckSelection::from_csv("dead-code");
    let results = infigraph_core::check::run_checks(tg._dir.path(), &cfg, &tg.backend, &sel);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "dead-code");
}

#[test]
fn test_check_format_table() {
    use infigraph_core::check::{CheckResult, CheckStatus};
    let results = vec![
        CheckResult {
            name: "security".into(),
            status: CheckStatus::Pass,
            summary: "0 issues".into(),
            details: vec![],
        },
        CheckResult {
            name: "complexity".into(),
            status: CheckStatus::Fail,
            summary: "3 violations".into(),
            details: vec!["  [25] handler (app.py)".into()],
        },
    ];
    let table = infigraph_core::check::format_table(&results);
    assert!(table.contains("PASS"));
    assert!(table.contains("FAIL"));
    assert!(table.contains("1/2 checks passed"));
    assert!(table.contains("handler"));
}

#[test]
fn test_check_format_json() {
    use infigraph_core::check::{CheckResult, CheckStatus};
    let results = vec![CheckResult {
        name: "security".into(),
        status: CheckStatus::Pass,
        summary: "ok".into(),
        details: vec![],
    }];
    let json = infigraph_core::check::format_json(&results);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed[0]["name"], "security");
    assert_eq!(parsed[0]["status"], "Pass");
}

#[test]
fn test_check_status_display() {
    assert_eq!(
        format!("{}", infigraph_core::check::CheckStatus::Pass),
        "PASS"
    );
    assert_eq!(
        format!("{}", infigraph_core::check::CheckStatus::Fail),
        "FAIL"
    );
}

// ============================================================
// Refactor analysis tests
// ============================================================

#[test]
fn test_refactor_analyze_empty_graph() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);

    let recs = infigraph_core::refactor::analyze(
        &backend,
        None,
        None,
        infigraph_core::refactor::Focus::All,
        10,
    )
    .unwrap();
    assert!(recs.is_empty());
}

#[test]
fn test_refactor_analyze_detects_complexity() {
    let tg = setup_graph();

    let recs = infigraph_core::refactor::analyze(
        &tg.backend,
        None,
        None,
        infigraph_core::refactor::Focus::Complexity,
        10,
    )
    .unwrap();
    assert!(
        recs.iter().any(|r| r.target.contains("complex_handler")),
        "should flag complex_handler (complexity=25)"
    );
}

#[test]
fn test_refactor_analyze_with_target_filter() {
    let tg = setup_graph();

    let recs = infigraph_core::refactor::analyze(
        &tg.backend,
        None,
        Some("src/utils.py"),
        infigraph_core::refactor::Focus::All,
        10,
    )
    .unwrap();
    for rec in &recs {
        assert!(
            rec.target.contains("utils.py") || !rec.target.contains("app.py"),
            "filtered results should focus on target"
        );
    }
}

#[test]
fn test_refactor_analyze_limit() {
    let tg = setup_graph();

    let recs = infigraph_core::refactor::analyze(
        &tg.backend,
        None,
        None,
        infigraph_core::refactor::Focus::All,
        1,
    )
    .unwrap();
    assert!(recs.len() <= 1);
}

#[test]
fn test_refactor_format_recommendations_empty() {
    let output = infigraph_core::refactor::format_recommendations(&[], None);
    assert!(output.contains("No refactoring recommendations"));
}

#[test]
fn test_refactor_format_recommendations_with_target() {
    let output = infigraph_core::refactor::format_recommendations(&[], Some("src/app.py"));
    assert!(output.contains("src/app.py"));
}

#[test]
fn test_refactor_format_recommendations_populated() {
    let recs = vec![infigraph_core::refactor::Recommendation {
        category: infigraph_core::refactor::Category::SimplifyLogic,
        target: "handler (app.py:52)".to_string(),
        impact: infigraph_core::refactor::Impact::High,
        effort: infigraph_core::refactor::Effort::Medium,
        rationale: "Cyclomatic complexity 25.".to_string(),
    }];
    let output = infigraph_core::refactor::format_recommendations(&recs, None);
    assert!(output.contains("handler"));
    assert!(output.contains("simplify_logic"));
    assert!(output.contains("HIGH IMPACT"));
}

#[test]
fn test_refactor_focus_from_str() {
    assert_eq!(
        infigraph_core::refactor::Focus::parse("complexity"),
        infigraph_core::refactor::Focus::Complexity
    );
    assert_eq!(
        infigraph_core::refactor::Focus::parse("duplication"),
        infigraph_core::refactor::Focus::Duplication
    );
    assert_eq!(
        infigraph_core::refactor::Focus::parse("coupling"),
        infigraph_core::refactor::Focus::Coupling
    );
    assert_eq!(
        infigraph_core::refactor::Focus::parse("size"),
        infigraph_core::refactor::Focus::Size
    );
    assert_eq!(
        infigraph_core::refactor::Focus::parse("unknown"),
        infigraph_core::refactor::Focus::All
    );
}

#[test]
fn test_refactor_category_display() {
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::SplitFile),
        "split_file"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::ExtractFunction),
        "extract_function"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::MergeDuplicates),
        "merge_duplicates"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::RemoveDeadCode),
        "remove_dead_code"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::ReduceCoupling),
        "reduce_coupling"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Category::SimplifyLogic),
        "simplify_logic"
    );
}

#[test]
fn test_refactor_impact_effort_display() {
    assert_eq!(
        format!("{}", infigraph_core::refactor::Impact::High),
        "high"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Impact::Medium),
        "medium"
    );
    assert_eq!(format!("{}", infigraph_core::refactor::Impact::Low), "low");
    assert_eq!(
        format!("{}", infigraph_core::refactor::Effort::High),
        "high"
    );
    assert_eq!(
        format!("{}", infigraph_core::refactor::Effort::Medium),
        "medium"
    );
    assert_eq!(format!("{}", infigraph_core::refactor::Effort::Low), "low");
}
