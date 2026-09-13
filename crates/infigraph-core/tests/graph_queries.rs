use infigraph_core::graph::{GraphQuery, GraphStore};
use infigraph_core::model::{
    FileExtraction, Relation, RelationKind, Span, Statement, StatementKind, Symbol, SymbolKind,
};

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
        scip_id: None,
        id: id.to_string(),
        name: name.to_string(),
        kind,
        span: span(file, start, end),
        signature_hash: format!("hash_{id}"),
        parent: None,
        language: "python".to_string(),
        visibility: Some("public".to_string()),
        docstring: None,
        complexity: 1,
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

fn stmt(parent: &str, idx: u32, kind: StatementKind, line: u32, depth: u32) -> Statement {
    Statement {
        id: format!("{parent}::stmt_{idx}"),
        kind,
        condition: format!("cond_{idx}"),
        start_line: line,
        end_line: line + 2,
        depth,
        parent_symbol: parent.to_string(),
    }
}

struct TestGraph {
    _dir: tempfile::TempDir,
    store: GraphStore,
}

impl TestGraph {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let store = GraphStore::open(&dir.path().join("graph")).expect("open store");
        Self { _dir: dir, store }
    }
}

fn fixture_extractions() -> Vec<FileExtraction> {
    vec![
        FileExtraction {
            file: "src/main.py".to_string(),
            language: "python".to_string(),
            content_hash: "aaa".to_string(),
            symbols: vec![
                sym(
                    "src/main.py::main",
                    "main",
                    SymbolKind::Function,
                    "src/main.py",
                    1,
                    10,
                ),
                sym(
                    "src/main.py::helper",
                    "helper",
                    SymbolKind::Function,
                    "src/main.py",
                    12,
                    20,
                ),
            ],
            relations: vec![
                rel(
                    "src/main.py::main",
                    "src/main.py::helper",
                    RelationKind::Calls,
                ),
                rel(
                    "src/main.py::main",
                    "src/lib.py::process",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![
                stmt("src/main.py::main", 0, StatementKind::If, 3, 0),
                stmt("src/main.py::main", 1, StatementKind::Else, 5, 0),
                stmt("src/main.py::main", 2, StatementKind::For, 7, 1),
            ],
        },
        FileExtraction {
            file: "src/lib.py".to_string(),
            language: "python".to_string(),
            content_hash: "bbb".to_string(),
            symbols: vec![
                sym(
                    "src/lib.py::process",
                    "process",
                    SymbolKind::Function,
                    "src/lib.py",
                    1,
                    15,
                ),
                sym(
                    "src/lib.py::validate",
                    "validate",
                    SymbolKind::Function,
                    "src/lib.py",
                    17,
                    25,
                ),
                {
                    let mut s = sym(
                        "src/lib.py::BaseClass",
                        "BaseClass",
                        SymbolKind::Class,
                        "src/lib.py",
                        27,
                        40,
                    );
                    s.complexity = 3;
                    s
                },
            ],
            relations: vec![
                rel(
                    "src/lib.py::process",
                    "src/lib.py::validate",
                    RelationKind::Calls,
                ),
                rel("src/lib.py", "src/lib.py::main", RelationKind::Imports),
            ],
            statements: vec![
                stmt("src/lib.py::process", 0, StatementKind::Try, 3, 0),
                stmt("src/lib.py::process", 1, StatementKind::Catch, 8, 0),
            ],
        },
        FileExtraction {
            file: "src/models.py".to_string(),
            language: "python".to_string(),
            content_hash: "ccc".to_string(),
            symbols: vec![
                {
                    let mut s = sym(
                        "src/models.py::ChildClass",
                        "ChildClass",
                        SymbolKind::Class,
                        "src/models.py",
                        1,
                        20,
                    );
                    s.complexity = 2;
                    s
                },
                sym(
                    "src/models.py::do_work",
                    "do_work",
                    SymbolKind::Method,
                    "src/models.py",
                    5,
                    15,
                ),
            ],
            relations: vec![
                rel(
                    "src/models.py::ChildClass",
                    "src/lib.py::BaseClass",
                    RelationKind::Inherits,
                ),
                rel(
                    "src/models.py::do_work",
                    "src/lib.py::validate",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "tests/test_main.py".to_string(),
            language: "python".to_string(),
            content_hash: "ddd".to_string(),
            symbols: vec![
                {
                    let mut s = sym(
                        "tests/test_main.py::test_main",
                        "test_main",
                        SymbolKind::Test,
                        "tests/test_main.py",
                        1,
                        10,
                    );
                    s.docstring = Some("@pytest".to_string());
                    s
                },
                sym(
                    "tests/test_main.py::test_helper",
                    "test_helper",
                    SymbolKind::Test,
                    "tests/test_main.py",
                    12,
                    20,
                ),
            ],
            relations: vec![
                rel(
                    "tests/test_main.py::test_main",
                    "src/main.py::main",
                    RelationKind::Calls,
                ),
                rel(
                    "tests/test_main.py::test_helper",
                    "src/main.py::helper",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![],
        },
    ]
}

fn setup() -> TestGraph {
    let tg = TestGraph::new();
    {
        let conn = tg.store.connection().expect("connection");
        tg.store
            .upsert_all_bulk(&conn, &fixture_extractions())
            .expect("bulk insert");
    }
    tg
}

// ---------- GraphQuery tests ----------

#[test]
fn test_symbols_in_file() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let rows = q.symbols_in_file("src/main.py").unwrap();
    assert_eq!(rows.len(), 2);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"helper"));
    assert_eq!(rows[0].start_line, 1);
}

#[test]
fn test_symbols_in_file_empty() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let rows = q.symbols_in_file("nonexistent.py").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_callers_of() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let callers = q.callers_of("src/main.py::helper").unwrap();
    assert_eq!(callers.len(), 2, "main + test_helper both call helper");
    assert!(callers.iter().any(|c| c.contains("main")));
    assert!(callers.iter().any(|c| c.contains("test_helper")));
}

#[test]
fn test_callees_of() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let callees = q.callees_of("src/main.py::main").unwrap();
    assert_eq!(callees.len(), 2);
    let ids: Vec<&str> = callees.iter().map(|s| s.as_str()).collect();
    assert!(ids.iter().any(|id| id.contains("helper")));
    assert!(ids.iter().any(|id| id.contains("process")));
}

#[test]
fn test_callers_callees_empty() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let callers = q.callers_of("nonexistent::sym").unwrap();
    assert!(callers.is_empty());
    let callees = q.callees_of("nonexistent::sym").unwrap();
    assert!(callees.is_empty());
}

#[test]
fn test_branches_of() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let branches = q.branches_of("src/main.py::main").unwrap();
    assert_eq!(branches.len(), 3);
    let kinds: Vec<&str> = branches.iter().map(|b| b.kind.as_str()).collect();
    assert!(kinds.contains(&"If"));
    assert!(kinds.contains(&"Else"));
    assert!(kinds.contains(&"For"));
    assert_eq!(branches[0].line, 3);
}

#[test]
fn test_branches_of_empty() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let branches = q.branches_of("src/models.py::do_work").unwrap();
    assert!(branches.is_empty());
}

#[test]
fn test_transitive_impact() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    // validate is called by process (depth 1) and process is called by main (depth 2)
    let impact = q.transitive_impact("src/lib.py::validate", 3).unwrap();
    let ids: Vec<&str> = impact.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.contains("process")),
        "process should be impacted"
    );
    assert!(
        ids.iter().any(|id| id.contains("main")),
        "main should be transitively impacted"
    );
}

#[test]
fn test_transitive_impact_depth_1() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let impact = q.transitive_impact("src/main.py::helper", 1).unwrap();
    let ids: Vec<&str> = impact.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.contains("main")),
        "main calls helper directly"
    );
}

#[test]
fn test_find_symbol_by_id() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let detail = q.find_symbol_by_id("src/lib.py::process").unwrap();
    assert!(detail.is_some());
    let d = detail.unwrap();
    assert_eq!(d.name, "process");
    assert_eq!(d.file, "src/lib.py");
    assert_eq!(d.start_line, 1);
    assert_eq!(d.end_line, 15);
}

#[test]
fn test_find_symbol_by_id_missing() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let detail = q.find_symbol_by_id("nonexistent::sym").unwrap();
    assert!(detail.is_none());
}

#[test]
fn test_symbols_in_range() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let rows = q.symbols_in_range("src/main.py", 1, 10).unwrap();
    assert!(rows.iter().any(|r| r.name == "main"));

    let rows2 = q.symbols_in_range("src/main.py", 12, 20).unwrap();
    assert!(rows2.iter().any(|r| r.name == "helper"));

    let rows3 = q.symbols_in_range("src/main.py", 100, 200).unwrap();
    assert!(rows3.is_empty());
}

#[test]
fn test_find_all_references() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let refs = q.find_all_references("src/main.py::helper").unwrap();
    assert_eq!(refs.len(), 2, "main + test_helper both reference helper");
    assert!(refs.iter().any(|r| r.caller_id.contains("main")));
    assert!(refs.iter().any(|r| r.caller_id.contains("test_helper")));

    // validate is called by process and do_work
    let refs2 = q.find_all_references("src/lib.py::validate").unwrap();
    assert_eq!(refs2.len(), 2);
}

#[test]
fn test_get_api_surface() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let api = q.get_api_surface().unwrap();
    assert!(!api.is_empty());
    assert!(api.iter().all(|s| s.visibility == "public"));
}

/// For languages where `visibility` is a naming convention rather than a
/// real access modifier (e.g. Python: any non-underscore name is
/// "public"), test helpers/fixtures end up marked public too and swamp
/// the real API surface. get_api_surface_filtered(false) must exclude
/// SymbolKind::Test symbols; get_api_surface_filtered(true) must match
/// plain get_api_surface.
#[test]
fn test_get_api_surface_filtered_excludes_test_symbols() {
    use infigraph_core::graph::{GraphBackend, KuzuBackend};

    let extraction = FileExtraction {
        file: "src/handler.py".to_string(),
        language: "python".to_string(),
        content_hash: "hash".to_string(),
        symbols: vec![
            sym(
                "src/handler.py::process_request",
                "process_request",
                SymbolKind::Function,
                "src/handler.py",
                1,
                10,
            ),
            sym(
                "src/handler.py::test_process_request_returns_200",
                "test_process_request_returns_200",
                SymbolKind::Test,
                "src/handler.py",
                12,
                20,
            ),
        ],
        relations: Vec::new(),
        statements: Vec::new(),
    };

    let dir = tempfile::TempDir::new().unwrap();
    let backend = KuzuBackend::open(&dir.path().join("graph")).unwrap();
    backend
        .upsert_files_bulk(std::slice::from_ref(&extraction), true)
        .unwrap();

    let with_tests = backend.get_api_surface_filtered(true).unwrap();
    assert!(
        with_tests.iter().any(|s| s.name == "process_request"),
        "expected process_request in unfiltered surface, got: {with_tests:?}"
    );
    assert!(
        with_tests
            .iter()
            .any(|s| s.name == "test_process_request_returns_200"),
        "expected the test symbol when include_tests=true, got: {with_tests:?}"
    );

    let without_tests = backend.get_api_surface_filtered(false).unwrap();
    assert!(
        without_tests.iter().any(|s| s.name == "process_request"),
        "expected process_request to survive filtering, got: {without_tests:?}"
    );
    assert!(
        !without_tests
            .iter()
            .any(|s| s.name == "test_process_request_returns_200"),
        "expected the test symbol excluded when include_tests=false, got: {without_tests:?}"
    );
}

#[test]
fn test_get_file_deps() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let deps = q.get_file_deps("src/lib.py").unwrap();
    assert!(deps.imports.contains(&"src/main.py".to_string()));

    let deps2 = q.get_file_deps("src/main.py").unwrap();
    assert!(deps2.imported_by.contains(&"src/lib.py".to_string()));
}

#[test]
fn test_get_type_hierarchy() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let hier = q.get_type_hierarchy("src/lib.py::BaseClass", 3).unwrap();
    assert_eq!(hier.root_name, "BaseClass");
    assert!(hier.descendants.iter().any(|d| d.name == "ChildClass"));

    let hier2 = q
        .get_type_hierarchy("src/models.py::ChildClass", 3)
        .unwrap();
    assert!(hier2.ancestors.iter().any(|a| a.name == "BaseClass"));
}

#[test]
fn test_derive_tested_by_and_coverage() {
    let tg = setup();

    // Derive edges (store-level method)
    let count = tg.store.derive_tested_by_edges().unwrap();
    assert!(
        count >= 2,
        "expected at least 2 TESTED_BY edges, got {count}"
    );

    // Query coverage
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let cov = q.get_test_coverage().unwrap();
    assert!(cov.covered_count >= 2, "main and helper should be covered");
    assert!(cov.uncovered_count > 0, "some symbols should be uncovered");
    assert!(cov.coverage_pct > 0 && cov.coverage_pct < 100);
}

#[test]
fn test_derive_tested_by_idempotent() {
    let tg = setup();

    let count1 = tg.store.derive_tested_by_edges().unwrap();
    let count2 = tg.store.derive_tested_by_edges().unwrap();
    assert_eq!(count1, count2, "re-deriving should produce same count");
}

#[test]
fn test_generate_test_context() {
    let tg = setup();
    tg.store.derive_tested_by_edges().unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let ctx = q.generate_test_context(None, 10, None).unwrap();
    assert!(!ctx.targets.is_empty(), "should have untested targets");
    for t in &ctx.targets {
        assert_ne!(t.kind, "Test", "test symbols should not be targets");
    }
}

#[test]
fn test_generate_test_context_file_filter() {
    let tg = setup();
    tg.store.derive_tested_by_edges().unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let ctx = q.generate_test_context(Some("models"), 10, None).unwrap();
    for t in &ctx.targets {
        assert!(
            t.file.contains("models"),
            "filter should restrict to models files"
        );
    }
}

#[test]
fn test_detect_test_framework() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    // Our test fixtures have @pytest in docstring
    let ctx = q.generate_test_context(None, 1, None).unwrap();
    assert!(
        ctx.framework.contains("pytest") || ctx.framework.contains("python"),
        "expected pytest framework, got: {}",
        ctx.framework
    );
}

#[test]
fn test_generate_test_context_templates_all_types() {
    let tg = setup();
    tg.store.derive_tested_by_edges().unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let ctx = q.generate_test_context(None, 10, None).unwrap();
    assert!(
        !ctx.templates.is_empty(),
        "should have templates for detected framework ({})",
        ctx.framework
    );
    for tpl in &ctx.templates {
        assert!(!tpl.test_type.is_empty(), "test_type should not be empty");
        assert!(
            !tpl.conventions.is_empty(),
            "conventions should not be empty"
        );
        assert!(!tpl.scaffold.is_empty(), "scaffold should not be empty");
    }
}

#[test]
fn test_generate_test_context_templates_filtered_type() {
    let tg = setup();
    tg.store.derive_tested_by_edges().unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let ctx = q.generate_test_context(None, 10, Some("unit")).unwrap();
    assert_eq!(
        ctx.templates.len(),
        1,
        "should have exactly one template for 'unit'"
    );
    assert_eq!(ctx.templates[0].test_type, "unit");
}

#[test]
fn test_generate_test_context_templates_unknown_framework() {
    use infigraph_core::graph::test_templates::test_templates_for;
    let templates = test_templates_for("unknown_framework", None);
    assert!(
        templates.is_empty(),
        "unknown framework should return no templates"
    );
}

#[test]
fn test_templates_all_frameworks_have_output() {
    use infigraph_core::graph::test_templates::test_templates_for;
    let frameworks = [
        "rust (cargo test)",
        "python (pytest)",
        "python (unittest)",
        "java (junit)",
        "javascript (jest)",
        "javascript (vitest)",
        "javascript (mocha)",
        "karate",
        "playwright",
        "cypress",
        "go (go test)",
        "csharp (xunit)",
        "csharp (nunit)",
        "csharp (mstest)",
        "kotlin (kotest)",
        "scala (scalatest)",
        "ruby (rspec)",
        "ruby (minitest)",
        "swift (XCTest)",
        "elixir (ExUnit)",
        "java (testng)",
        "bdd (cucumber)",
    ];
    for fw in &frameworks {
        let templates = test_templates_for(fw, None);
        assert!(
            !templates.is_empty(),
            "framework '{}' should return at least one template",
            fw
        );
        for tpl in &templates {
            assert!(
                !tpl.conventions.is_empty(),
                "{} {} conventions empty",
                fw,
                tpl.test_type
            );
            assert!(
                !tpl.scaffold.is_empty(),
                "{} {} scaffold empty",
                fw,
                tpl.test_type
            );
        }
    }
}

#[test]
fn test_templates_test_type_filter() {
    use infigraph_core::graph::test_templates::test_templates_for;
    for tt in &["unit", "integration", "functional", "e2e"] {
        let templates = test_templates_for("python (pytest)", Some(tt));
        assert!(
            templates.len() <= 1,
            "filtered by '{}' should return 0 or 1 template, got {}",
            tt,
            templates.len()
        );
        if !templates.is_empty() {
            assert_eq!(templates[0].test_type, *tt);
        }
    }
}

#[test]
fn test_templates_rust_has_unit_integration_e2e() {
    use infigraph_core::graph::test_templates::test_templates_for;
    let templates = test_templates_for("rust (cargo test)", None);
    let types: Vec<&str> = templates.iter().map(|t| t.test_type.as_str()).collect();
    assert!(types.contains(&"unit"), "rust should have unit template");
    assert!(
        types.contains(&"integration"),
        "rust should have integration template"
    );
    assert!(types.contains(&"e2e"), "rust should have e2e template");
}

#[test]
fn test_templates_pytest_has_all_four_types() {
    use infigraph_core::graph::test_templates::test_templates_for;
    let templates = test_templates_for("python (pytest)", None);
    let types: Vec<&str> = templates.iter().map(|t| t.test_type.as_str()).collect();
    assert!(types.contains(&"unit"), "pytest should have unit");
    assert!(
        types.contains(&"integration"),
        "pytest should have integration"
    );
    assert!(
        types.contains(&"functional"),
        "pytest should have functional"
    );
    assert!(types.contains(&"e2e"), "pytest should have e2e");
}

// ---------- GraphStore tests ----------

#[test]
fn test_stats() {
    let tg = setup();
    let stats = tg.store.stats().unwrap();
    assert!(
        stats.symbols >= 7,
        "expected >= 7 symbols, got {}",
        stats.symbols
    );
    assert!(
        stats.modules >= 4,
        "expected >= 4 modules, got {}",
        stats.modules
    );
    assert!(stats.files >= 4, "expected >= 4 files, got {}", stats.files);
    assert!(
        stats.calls >= 4,
        "expected >= 4 call edges, got {}",
        stats.calls
    );
    assert!(
        stats.inherits >= 1,
        "expected >= 1 inherit edge, got {}",
        stats.inherits
    );
}

#[test]
fn test_get_file_hashes() {
    let tg = setup();
    let hashes = tg.store.get_file_hashes().unwrap();
    assert_eq!(hashes.len(), 4);
    assert_eq!(hashes.get("src/main.py").map(|s| s.as_str()), Some("aaa"));
    assert_eq!(hashes.get("src/lib.py").map(|s| s.as_str()), Some("bbb"));
}

#[test]
fn test_get_all_symbols() {
    let tg = setup();
    let syms = tg.store.get_all_symbols().unwrap();
    assert!(syms.len() >= 7);
    let names: Vec<&str> = syms.iter().map(|(n, _, _, _)| n.as_str()).collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"process"));
    assert!(names.contains(&"BaseClass"));
}

#[test]
fn test_remove_file() {
    let tg = setup();

    // Verify file exists
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    assert!(!q.symbols_in_file("src/main.py").unwrap().is_empty());

    // Remove and verify gone
    tg.store.remove_file("src/main.py").unwrap();
    let conn2 = tg.store.connection().unwrap();
    let q2 = GraphQuery::new(&conn2);
    assert!(q2.symbols_in_file("src/main.py").unwrap().is_empty());
}

#[test]
fn test_upsert_file_single() {
    let tg = TestGraph::new();

    let extraction = FileExtraction {
        file: "single.py".to_string(),
        language: "python".to_string(),
        content_hash: "zzz".to_string(),
        symbols: vec![sym(
            "single.py::foo",
            "foo",
            SymbolKind::Function,
            "single.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![stmt("single.py::foo", 0, StatementKind::If, 2, 0)],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("single.py").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "foo");

    let branches = q.branches_of("single.py::foo").unwrap();
    assert_eq!(branches.len(), 1);
}

#[test]
fn test_upsert_overwrites() {
    let tg = TestGraph::new();

    let v1 = FileExtraction {
        file: "f.py".to_string(),
        language: "python".to_string(),
        content_hash: "v1".to_string(),
        symbols: vec![sym("f.py::a", "a", SymbolKind::Function, "f.py", 1, 5)],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&v1).unwrap();

    let v2 = FileExtraction {
        file: "f.py".to_string(),
        language: "python".to_string(),
        content_hash: "v2".to_string(),
        symbols: vec![
            sym("f.py::a", "a", SymbolKind::Function, "f.py", 1, 5),
            sym("f.py::b", "b", SymbolKind::Function, "f.py", 7, 10),
        ],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&v2).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("f.py").unwrap();
    assert_eq!(rows.len(), 2, "v2 should have 2 symbols");
    let hashes = tg.store.get_file_hashes().unwrap();
    assert_eq!(hashes.get("f.py").map(|s| s.as_str()), Some("v2"));
}

#[test]
fn test_raw_query() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let rows = q
        .raw_query("MATCH (s:Symbol) RETURN s.name ORDER BY s.name")
        .unwrap();
    assert!(!rows.is_empty());
    let first_col: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert!(first_col.contains(&"main"));
}

#[test]
fn test_empty_graph_queries() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    assert!(q.symbols_in_file("any.py").unwrap().is_empty());
    assert!(q.callers_of("any").unwrap().is_empty());
    assert!(q.callees_of("any").unwrap().is_empty());
    assert!(q.branches_of("any").unwrap().is_empty());
    assert!(q.transitive_impact("any", 3).unwrap().is_empty());
    assert!(q.find_symbol_by_id("any").unwrap().is_none());
    assert!(q.find_all_references("any").unwrap().is_empty());
    assert!(q.get_api_surface().unwrap().is_empty());
    assert!(q.get_type_hierarchy("any", 3).unwrap().ancestors.is_empty());

    let cov = q.get_test_coverage().unwrap();
    assert_eq!(cov.coverage_pct, 0);

    let stats = tg.store.stats().unwrap();
    assert_eq!(stats.symbols, 0);
}

#[test]
fn test_special_chars_in_ids() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "src/o'malley.py".to_string(),
        language: "python".to_string(),
        content_hash: "special".to_string(),
        symbols: vec![sym(
            "src/o'malley.py::fn_with'quote",
            "fn_with'quote",
            SymbolKind::Function,
            "src/o'malley.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("src/o'malley.py").unwrap();
    assert_eq!(rows.len(), 1);
}

// ---------- GraphStats Display ----------

#[test]
fn test_stats_display() {
    let tg = setup();
    let stats = tg.store.stats().unwrap();
    let display = format!("{stats}");
    assert!(display.contains("Symbols:"));
    assert!(display.contains("Modules:"));
    assert!(display.contains("Calls edges:"));
}

// ---------- Parquet write path ----------

#[test]
fn test_upsert_all_parquet() {
    let tg = TestGraph::new();
    let extractions = fixture_extractions();
    tg.store.upsert_all_parquet(&extractions).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("src/main.py").unwrap();
    assert_eq!(rows.len(), 2);
    let callees = q.callees_of("src/main.py::main").unwrap();
    assert_eq!(callees.len(), 2);
    let branches = q.branches_of("src/main.py::main").unwrap();
    assert_eq!(branches.len(), 3);
}

// ---------- Folder hierarchy (parquet path) ----------

#[test]
fn test_upsert_folders_bulk() {
    let tg = TestGraph::new();
    {
        let conn = tg.store.connection().unwrap();
        tg.store
            .upsert_all_bulk(&conn, &fixture_extractions())
            .unwrap();
    }
    let file_paths: Vec<&str> = vec![
        "src/main.py",
        "src/lib.py",
        "src/models.py",
        "tests/test_main.py",
    ];
    tg.store.upsert_folders_bulk(&file_paths).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q
        .raw_query("MATCH (d:Folder) RETURN d.id ORDER BY d.id")
        .unwrap();
    assert!(!rows.is_empty(), "should have created folder nodes");
    let ids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert!(ids.iter().any(|id| id.contains("src")));
    assert!(ids.iter().any(|id| id.contains("tests")));
}

// ---------- Custom edge support ----------

#[test]
fn test_custom_edge_relations() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "decorators.py".to_string(),
        language: "python".to_string(),
        content_hash: "custom".to_string(),
        symbols: vec![
            sym(
                "decorators.py::my_decorator",
                "my_decorator",
                SymbolKind::Function,
                "decorators.py",
                1,
                5,
            ),
            sym(
                "decorators.py::my_func",
                "my_func",
                SymbolKind::Function,
                "decorators.py",
                7,
                15,
            ),
        ],
        relations: vec![rel(
            "decorators.py::my_func",
            "decorators.py::my_decorator",
            RelationKind::Custom("DECORATED_BY".to_string()),
        )],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q
        .raw_query("MATCH (a:Symbol)-[:DECORATED_BY]->(b:Symbol) RETURN a.name, b.name")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], "my_func");
    assert_eq!(rows[0][1], "my_decorator");
}

// ---------- Bulk write path equivalence ----------

#[test]
fn test_bulk_vs_single_write_equivalence() {
    let extractions = fixture_extractions();

    let tg_bulk = TestGraph::new();
    {
        let conn = tg_bulk.store.connection().unwrap();
        tg_bulk.store.upsert_all_bulk(&conn, &extractions).unwrap();
    }

    let tg_single = TestGraph::new();
    for e in &extractions {
        tg_single.store.upsert_file(e).unwrap();
    }

    let stats_bulk = tg_bulk.store.stats().unwrap();
    let stats_single = tg_single.store.stats().unwrap();
    assert_eq!(
        stats_bulk.symbols, stats_single.symbols,
        "symbol count mismatch"
    );
    assert_eq!(
        stats_bulk.modules, stats_single.modules,
        "module count mismatch"
    );
    assert_eq!(stats_bulk.files, stats_single.files, "file count mismatch");
    // CALLS may differ: bulk inserts all at once; single upsert deletes+recreates per file,
    // which can drop cross-file edges when target file is rewritten later.
    // Both counts should be >= 4 (the within-file edges).
    assert!(
        stats_bulk.calls >= 4,
        "bulk calls too low: {}",
        stats_bulk.calls
    );
    assert!(
        stats_single.calls >= 4,
        "single calls too low: {}",
        stats_single.calls
    );
    assert_eq!(
        stats_bulk.inherits, stats_single.inherits,
        "inherits count mismatch"
    );
}

// ---------- Direct _conn method tests ----------

#[test]
fn test_upsert_file_conn_direct() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "conn_test.py".to_string(),
        language: "python".to_string(),
        content_hash: "conn1".to_string(),
        symbols: vec![
            sym(
                "conn_test.py::alpha",
                "alpha",
                SymbolKind::Function,
                "conn_test.py",
                1,
                5,
            ),
            sym(
                "conn_test.py::beta",
                "beta",
                SymbolKind::Function,
                "conn_test.py",
                7,
                12,
            ),
        ],
        relations: vec![rel(
            "conn_test.py::alpha",
            "conn_test.py::beta",
            RelationKind::Calls,
        )],
        statements: vec![stmt("conn_test.py::alpha", 0, StatementKind::If, 2, 0)],
    };

    let conn = tg.store.connection().unwrap();
    tg.store.upsert_file_conn(&conn, &extraction).unwrap();

    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("conn_test.py").unwrap();
    assert_eq!(rows.len(), 2);
    let callees = q.callees_of("conn_test.py::alpha").unwrap();
    assert_eq!(callees.len(), 1);
    assert!(callees[0].contains("beta"));
    let branches = q.branches_of("conn_test.py::alpha").unwrap();
    assert_eq!(branches.len(), 1);
}

#[test]
fn test_upsert_file_conn_overwrites_old_data() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();

    let v1 = FileExtraction {
        file: "evolve.py".to_string(),
        language: "python".to_string(),
        content_hash: "v1".to_string(),
        symbols: vec![sym(
            "evolve.py::old_fn",
            "old_fn",
            SymbolKind::Function,
            "evolve.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file_conn(&conn, &v1).unwrap();

    let q = GraphQuery::new(&conn);
    assert_eq!(q.symbols_in_file("evolve.py").unwrap().len(), 1);

    // Upsert again with different symbols — old should be deleted
    let v2 = FileExtraction {
        file: "evolve.py".to_string(),
        language: "python".to_string(),
        content_hash: "v2".to_string(),
        symbols: vec![
            sym(
                "evolve.py::new_fn",
                "new_fn",
                SymbolKind::Function,
                "evolve.py",
                1,
                5,
            ),
            sym(
                "evolve.py::other",
                "other",
                SymbolKind::Function,
                "evolve.py",
                7,
                10,
            ),
        ],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file_conn(&conn, &v2).unwrap();

    let rows = q.symbols_in_file("evolve.py").unwrap();
    assert_eq!(rows.len(), 2, "should have 2 new symbols");
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert!(!names.contains(&"old_fn"), "old_fn should be gone");
    assert!(names.contains(&"new_fn"));
    assert!(names.contains(&"other"));
}

#[test]
fn test_upsert_file_conn_no_delete_accumulates() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();

    let e1 = FileExtraction {
        file: "accum.py".to_string(),
        language: "python".to_string(),
        content_hash: "h1".to_string(),
        symbols: vec![sym(
            "accum.py::first",
            "first",
            SymbolKind::Function,
            "accum.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file_conn_no_delete(&conn, &e1).unwrap();

    let q = GraphQuery::new(&conn);
    assert_eq!(q.symbols_in_file("accum.py").unwrap().len(), 1);

    // Insert again without delete — should fail on duplicate Module PK
    // or accumulate depending on implementation
    let e2 = FileExtraction {
        file: "accum2.py".to_string(),
        language: "python".to_string(),
        content_hash: "h2".to_string(),
        symbols: vec![sym(
            "accum2.py::second",
            "second",
            SymbolKind::Function,
            "accum2.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file_conn_no_delete(&conn, &e2).unwrap();

    // Both files should have their symbols
    assert_eq!(q.symbols_in_file("accum.py").unwrap().len(), 1);
    assert_eq!(q.symbols_in_file("accum2.py").unwrap().len(), 1);
    let stats = tg.store.stats().unwrap();
    assert_eq!(stats.symbols, 2);
}

#[test]
fn test_upsert_folders_bulk_conn_direct() {
    let tg = TestGraph::new();
    {
        let conn = tg.store.connection().unwrap();
        tg.store
            .upsert_all_bulk(&conn, &fixture_extractions())
            .unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let paths: Vec<&str> = vec![
        "src/main.py",
        "src/lib.py",
        "src/models.py",
        "tests/test_main.py",
        "src/deep/nested/file.py",
    ];
    tg.store.upsert_folders_bulk_conn(&conn, &paths).unwrap();

    let q = GraphQuery::new(&conn);
    let folders = q
        .raw_query("MATCH (d:Folder) RETURN d.id ORDER BY d.id")
        .unwrap();
    let ids: Vec<&str> = folders.iter().map(|r| r[0].as_str()).collect();
    assert!(ids.contains(&"src"), "should have src folder");
    assert!(ids.contains(&"tests"), "should have tests folder");
    assert!(ids.contains(&"src/deep"), "should have src/deep folder");
    assert!(
        ids.contains(&"src/deep/nested"),
        "should have src/deep/nested folder"
    );
}

// ---------- Empty extraction edge cases ----------

#[test]
fn test_upsert_empty_extraction() {
    let tg = TestGraph::new();
    let empty = FileExtraction {
        file: "empty.py".to_string(),
        language: "python".to_string(),
        content_hash: "empty".to_string(),
        symbols: vec![],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&empty).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("empty.py").unwrap();
    assert!(
        rows.is_empty(),
        "empty extraction should produce no symbols"
    );

    // Module and File nodes should still exist
    let modules = q
        .raw_query("MATCH (m:Module) WHERE m.id = 'empty.py' RETURN m.id")
        .unwrap();
    assert_eq!(
        modules.len(),
        1,
        "module node should exist even with no symbols"
    );
}

#[test]
fn test_bulk_empty_extractions() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();
    tg.store.upsert_all_bulk(&conn, &[]).unwrap();
    let stats = tg.store.stats().unwrap();
    assert_eq!(stats.symbols, 0);
    assert_eq!(stats.modules, 0);
}

// ---------- Schema idempotency ----------

#[test]
fn test_schema_creation_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("schema_test");

    // Open twice — schema CREATE IF NOT EXISTS should be idempotent
    let store1 = GraphStore::open(&path).unwrap();
    let conn1 = store1.connection().unwrap();
    conn1.query("CREATE (s:Symbol {id: 'test::sym', name: 'sym', kind: 'Function', file: 'test.py', start_line: 1, end_line: 5, signature_hash: 'h', language: 'python', visibility: '', parent: '', docstring: '', complexity: 1, parameters: '', return_type: ''})").unwrap();
    drop(conn1);
    drop(store1);

    let store2 = GraphStore::open(&path).unwrap();
    let conn2 = store2.connection().unwrap();
    let q = GraphQuery::new(&conn2);
    let detail = q.find_symbol_by_id("test::sym").unwrap();
    assert!(detail.is_some(), "symbol should survive schema re-init");
}

#[test]
fn test_ensure_custom_edge_table_idempotent() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();

    // First upsert with custom edge — creates the table
    let extraction1 = FileExtraction {
        file: "custom1.py".to_string(),
        language: "python".to_string(),
        content_hash: "c1".to_string(),
        symbols: vec![
            sym(
                "custom1.py::a",
                "a",
                SymbolKind::Function,
                "custom1.py",
                1,
                5,
            ),
            sym(
                "custom1.py::b",
                "b",
                SymbolKind::Function,
                "custom1.py",
                7,
                10,
            ),
        ],
        relations: vec![rel(
            "custom1.py::a",
            "custom1.py::b",
            RelationKind::Custom("MY_CUSTOM_EDGE".to_string()),
        )],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction1).unwrap();

    // Second upsert with same custom edge type — should not error (idempotent table creation)
    let extraction2 = FileExtraction {
        file: "custom2.py".to_string(),
        language: "python".to_string(),
        content_hash: "c2".to_string(),
        symbols: vec![
            sym(
                "custom2.py::x",
                "x",
                SymbolKind::Function,
                "custom2.py",
                1,
                5,
            ),
            sym(
                "custom2.py::y",
                "y",
                SymbolKind::Function,
                "custom2.py",
                7,
                10,
            ),
        ],
        relations: vec![rel(
            "custom2.py::x",
            "custom2.py::y",
            RelationKind::Custom("MY_CUSTOM_EDGE".to_string()),
        )],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction2).unwrap();

    let q = GraphQuery::new(&conn);
    let rows = q
        .raw_query("MATCH (a:Symbol)-[:MY_CUSTOM_EDGE]->(b:Symbol) RETURN a.name, b.name")
        .unwrap();
    assert_eq!(rows.len(), 2, "both custom edges should exist");
}

/// AIF3X-331 #16 end-to-end: a real FastAPI `add_middleware(..., dispatch=fn)`
/// and `Depends(fn)` registration, extracted via the actual Python tree-sitter
/// registry (not hand-built Relations, per the #14/#15 lesson that hand-crafted
/// relations can hide real extraction-layer bugs), should surface via
/// GraphQuery::callers_of on the registered symbol — this is the exact battery
/// items 6/7 from the eval report (trace_callers on validate_request_headers /
/// v3_logging_context_middleware should show the registration site, not only
/// unit-test callers).
#[test]
fn test_callers_of_surfaces_middleware_and_dependency_registrations() {
    use infigraph_languages::bundled_registry;

    let registry = bundled_registry().unwrap();
    let pack = registry.for_extension(".py").unwrap();

    let middleware_src = b"async def v3_logging_context_middleware(request, call_next):\n    return await call_next(request)\n";
    let deps_src = b"def validate_request_headers(request):\n    return True\n";
    let main_src = b"from app.middleware import v3_logging_context_middleware\nfrom app.deps import validate_request_headers\n\ndef create_app():\n    app.add_middleware(BaseHTTPMiddleware, dispatch=v3_logging_context_middleware)\n\nasync def handler(headers=Depends(validate_request_headers)):\n    pass\n";

    let middleware_ext =
        infigraph_core::extract::extract_file("app/middleware.py", middleware_src, pack).unwrap();
    let deps_ext = infigraph_core::extract::extract_file("app/deps.py", deps_src, pack).unwrap();
    let main_ext = infigraph_core::extract::extract_file("app/main.py", main_src, pack).unwrap();

    let extractions = vec![middleware_ext, deps_ext, main_ext];

    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();
    tg.store.upsert_all_bulk(&conn, &extractions).unwrap();
    infigraph_core::resolve::resolve_calls(&tg.store, &extractions, None).unwrap();

    let q = GraphQuery::new(&conn);

    let middleware_callers = q
        .callers_of("app/middleware.py::v3_logging_context_middleware")
        .unwrap();
    assert!(
        !middleware_callers.is_empty(),
        "expected the add_middleware(dispatch=...) registration site to appear \
         as a caller, got: {middleware_callers:?}"
    );

    let deps_callers = q
        .callers_of("app/deps.py::validate_request_headers")
        .unwrap();
    assert!(
        !deps_callers.is_empty(),
        "expected the Depends(...) registration site to appear as a caller, \
         got: {deps_callers:?}"
    );
}

/// AIF3X-331: detect_cross_cutting only matched docstring patterns, so it was
/// blind to FastAPI's add_middleware(...)/Depends(...) wiring -- that's a
/// structural registration call site, not a decorator on the target
/// function's own docstring. It's already captured as REGISTERS_MIDDLEWARE /
/// INJECTS_DEPENDENCY graph edges during extraction (#16, same fixture as
/// test_callers_of_surfaces_middleware_and_dependency_registrations above);
/// this checks detect_cross_cutting surfaces those edges too.
#[test]
fn test_detect_cross_cutting_surfaces_middleware_and_dependency_edges() {
    use infigraph_core::concerns::detect_cross_cutting;
    use infigraph_core::graph::{GraphBackend, KuzuBackend};
    use infigraph_languages::bundled_registry;

    let registry = bundled_registry().unwrap();
    let pack = registry.for_extension(".py").unwrap();

    let middleware_src = b"async def v3_logging_context_middleware(request, call_next):\n    return await call_next(request)\n";
    let deps_src = b"def validate_request_headers(request):\n    return True\n";
    let main_src = b"from app.middleware import v3_logging_context_middleware\nfrom app.deps import validate_request_headers\n\ndef create_app():\n    app.add_middleware(BaseHTTPMiddleware, dispatch=v3_logging_context_middleware)\n\nasync def handler(headers=Depends(validate_request_headers)):\n    pass\n";

    let middleware_ext =
        infigraph_core::extract::extract_file("app/middleware.py", middleware_src, pack).unwrap();
    let deps_ext = infigraph_core::extract::extract_file("app/deps.py", deps_src, pack).unwrap();
    let main_ext = infigraph_core::extract::extract_file("app/main.py", main_src, pack).unwrap();
    let extractions = vec![middleware_ext, deps_ext, main_ext];

    let dir = tempfile::TempDir::new().unwrap();
    let backend = KuzuBackend::open(&dir.path().join("graph")).unwrap();
    backend.upsert_files_bulk(&extractions, true).unwrap();
    backend.resolve_calls(&extractions, None).unwrap();

    let matches = detect_cross_cutting(&backend).unwrap();

    assert!(
        matches.iter().any(|m| m.kind == "Middleware"
            && m.symbol_id.contains("v3_logging_context_middleware")),
        "expected a Middleware concern for the add_middleware(dispatch=...) target, got: {matches:?}"
    );
    assert!(
        matches
            .iter()
            .any(|m| m.kind == "DependencyInjection"
                && m.symbol_id.contains("validate_request_headers")),
        "expected a DependencyInjection concern for the Depends(...) target, got: {matches:?}"
    );
}

/// AIF3X-331 #16 safety check: callers_of's INJECTS_DEPENDENCY/
/// REGISTERS_MIDDLEWARE union must not error when those rel tables have never
/// been populated (e.g. a pure-Rust/Go repo with no Python) — confirmed via a
/// live no-Python fixture that this previously raised a Kuzu binder exception
/// ("Table ... does not exist") before both edge kinds were added to
/// CREATE_SCHEMA (which init_schema runs unconditionally on every open).
#[test]
fn test_callers_of_no_error_when_custom_edge_tables_unpopulated() {
    let tg = TestGraph::new();
    let conn = tg.store.connection().unwrap();
    tg.store
        .upsert_all_bulk(
            &conn,
            &[FileExtraction {
                file: "main.rs".to_string(),
                language: "rust".to_string(),
                content_hash: "a".to_string(),
                symbols: vec![sym(
                    "main.rs::main",
                    "main",
                    SymbolKind::Function,
                    "main.rs",
                    1,
                    1,
                )],
                relations: vec![],
                statements: vec![],
            }],
        )
        .unwrap();

    let q = GraphQuery::new(&conn);
    let callers = q.callers_of("main.rs::main").unwrap();
    assert!(callers.is_empty());
}

// ---------- Special characters / edge cases ----------

#[test]
fn test_special_chars_newlines_and_quotes() {
    let tg = TestGraph::new();
    let mut s = sym(
        "special.py::func",
        "func",
        SymbolKind::Function,
        "special.py",
        1,
        5,
    );
    s.docstring = Some("Line one\nLine two\n\"quoted\"".to_string());
    s.parameters = Some("a: str, b: int = 'default'".to_string());
    s.return_type = Some("Optional[Dict[str, Any]]".to_string());

    let extraction = FileExtraction {
        file: "special.py".to_string(),
        language: "python".to_string(),
        content_hash: "sp".to_string(),
        symbols: vec![s],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let detail = q.find_symbol_by_id("special.py::func").unwrap().unwrap();
    assert_eq!(detail.name, "func");
}

#[test]
fn test_unicode_in_symbol_names() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "unicode.py".to_string(),
        language: "python".to_string(),
        content_hash: "uni".to_string(),
        symbols: vec![
            sym(
                "unicode.py::café",
                "café",
                SymbolKind::Function,
                "unicode.py",
                1,
                5,
            ),
            sym(
                "unicode.py::日本語",
                "日本語",
                SymbolKind::Function,
                "unicode.py",
                7,
                10,
            ),
        ],
        relations: vec![rel(
            "unicode.py::café",
            "unicode.py::日本語",
            RelationKind::Calls,
        )],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let rows = q.symbols_in_file("unicode.py").unwrap();
    assert_eq!(rows.len(), 2);
    let callees = q.callees_of("unicode.py::café").unwrap();
    assert_eq!(callees.len(), 1);
}

// ---------- Transitive impact depth enforcement ----------

#[test]
fn test_transitive_impact_depth_limit() {
    // Build a chain: a -> b -> c -> d -> e
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "chain.py".to_string(),
        language: "python".to_string(),
        content_hash: "chain".to_string(),
        symbols: vec![
            sym("chain.py::a", "a", SymbolKind::Function, "chain.py", 1, 5),
            sym("chain.py::b", "b", SymbolKind::Function, "chain.py", 7, 10),
            sym("chain.py::c", "c", SymbolKind::Function, "chain.py", 12, 15),
            sym("chain.py::d", "d", SymbolKind::Function, "chain.py", 17, 20),
            sym("chain.py::e", "e", SymbolKind::Function, "chain.py", 22, 25),
        ],
        relations: vec![
            rel("chain.py::a", "chain.py::b", RelationKind::Calls),
            rel("chain.py::b", "chain.py::c", RelationKind::Calls),
            rel("chain.py::c", "chain.py::d", RelationKind::Calls),
            rel("chain.py::d", "chain.py::e", RelationKind::Calls),
        ],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    // Depth 1: changing e should only show d
    let impact1 = q.transitive_impact("chain.py::e", 1).unwrap();
    let ids1: Vec<&str> = impact1.iter().map(|r| r.id.as_str()).collect();
    assert!(ids1.contains(&"chain.py::d"), "d calls e directly");
    assert!(
        !ids1.contains(&"chain.py::a"),
        "a should not appear at depth 1"
    );

    // Depth 2: should show d and c
    let impact2 = q.transitive_impact("chain.py::e", 2).unwrap();
    let ids2: Vec<&str> = impact2.iter().map(|r| r.id.as_str()).collect();
    assert!(ids2.contains(&"chain.py::d"));
    assert!(ids2.contains(&"chain.py::c"));

    // Depth 4: should show all callers
    let impact4 = q.transitive_impact("chain.py::e", 4).unwrap();
    assert!(
        impact4.len() >= 4,
        "depth 4 should reach all 4 callers, got {}",
        impact4.len()
    );
}

// ---------- NULL field handling ----------

#[test]
fn test_null_optional_fields() {
    let tg = TestGraph::new();
    let mut s = sym(
        "nulls.py::func",
        "func",
        SymbolKind::Function,
        "nulls.py",
        1,
        5,
    );
    s.visibility = None;
    s.docstring = None;
    s.parent = None;
    s.parameters = None;
    s.return_type = None;

    let extraction = FileExtraction {
        file: "nulls.py".to_string(),
        language: "python".to_string(),
        content_hash: "nulls".to_string(),
        symbols: vec![s],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let detail = q.find_symbol_by_id("nulls.py::func").unwrap().unwrap();
    assert_eq!(detail.name, "func");

    // API surface — NULL visibility should not appear as "public"
    let api = q.get_api_surface().unwrap();
    let has_nulls_func = api.iter().any(|a| a.name == "func");
    assert!(
        !has_nulls_func,
        "NULL visibility should not count as public"
    );
}

// ---------- Folders with no parent (root-level files) ----------

#[test]
fn test_folders_root_level_files() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "main.py".to_string(),
        language: "python".to_string(),
        content_hash: "root".to_string(),
        symbols: vec![sym(
            "main.py::main",
            "main",
            SymbolKind::Function,
            "main.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    };
    tg.store.upsert_file(&extraction).unwrap();

    // Root-level file has no directory — should not create folders
    let paths: Vec<&str> = vec!["main.py"];
    tg.store.upsert_folders_bulk(&paths).unwrap();

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let folders = q.raw_query("MATCH (f:Folder) RETURN f.id").unwrap();
    assert!(
        folders.is_empty(),
        "root-level file should create no folders"
    );
}

// ---------- Skeleton tests ----------

#[test]
fn test_skeleton_output_format() {
    let tg = TestGraph::new();
    let mut s = sym(
        "skel.py::compute",
        "compute",
        SymbolKind::Function,
        "skel.py",
        10,
        25,
    );
    s.complexity = 5;
    s.parameters = Some("(x: int, y: int)".to_string());
    s.return_type = Some("int".to_string());

    let extraction = FileExtraction {
        file: "skel.py".to_string(),
        language: "python".to_string(),
        content_hash: "sk".to_string(),
        symbols: vec![s],
        relations: vec![],
        statements: vec![
            stmt("skel.py::compute", 0, StatementKind::If, 12, 0),
            stmt("skel.py::compute", 1, StatementKind::For, 15, 1),
        ],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("skel.py").unwrap();

    assert!(out.contains("# skel.py"), "should have file header");
    assert!(
        out.contains("compute(x: int, y: int) -> int"),
        "should have signature with params and return type"
    );
    assert!(
        out.contains("# complexity: 5"),
        "should show complexity annotation"
    );
    assert!(out.contains("nesting:"), "should show nesting annotation");
    assert!(out.contains("stmts:"), "should show stmts annotation");
    assert!(out.contains("fan-in:"), "should show fan-in annotation");
}

#[test]
fn test_skeleton_nesting_from_statements() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "nest.py".to_string(),
        language: "python".to_string(),
        content_hash: "ns".to_string(),
        symbols: vec![sym(
            "nest.py::deep",
            "deep",
            SymbolKind::Function,
            "nest.py",
            1,
            20,
        )],
        relations: vec![],
        statements: vec![
            stmt("nest.py::deep", 0, StatementKind::If, 2, 0),
            stmt("nest.py::deep", 1, StatementKind::For, 4, 1),
            stmt("nest.py::deep", 2, StatementKind::If, 6, 2),
            stmt("nest.py::deep", 3, StatementKind::While, 8, 3),
        ],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("nest.py").unwrap();
    assert!(
        out.contains("nesting: 3"),
        "max depth should be 3, got:\n{out}"
    );
    assert!(
        out.contains("stmts: 4"),
        "should have 4 statements, got:\n{out}"
    );
}

#[test]
fn test_skeleton_fan_in_count() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "fanin.py".to_string(),
        language: "python".to_string(),
        content_hash: "fi".to_string(),
        symbols: vec![
            sym(
                "fanin.py::target",
                "target",
                SymbolKind::Function,
                "fanin.py",
                1,
                5,
            ),
            sym(
                "fanin.py::caller1",
                "caller1",
                SymbolKind::Function,
                "fanin.py",
                7,
                10,
            ),
            sym(
                "fanin.py::caller2",
                "caller2",
                SymbolKind::Function,
                "fanin.py",
                12,
                15,
            ),
        ],
        relations: vec![
            rel("fanin.py::caller1", "fanin.py::target", RelationKind::Calls),
            rel("fanin.py::caller2", "fanin.py::target", RelationKind::Calls),
        ],
        statements: vec![],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("fanin.py").unwrap();

    let target_section: &str = out.split("target").nth(1).unwrap_or("");
    let fan_in_line = target_section
        .lines()
        .find(|l| l.contains("fan-in:"))
        .unwrap_or("");
    assert!(
        fan_in_line.contains("fan-in: 2"),
        "target should have fan-in: 2, got: {fan_in_line}"
    );
}

#[test]
fn test_skeleton_class_members_indented() {
    let tg = TestGraph::new();
    let mut method = sym(
        "cls.py::MyClass::do_thing",
        "do_thing",
        SymbolKind::Method,
        "cls.py",
        5,
        15,
    );
    method.parent = Some("cls.py::MyClass".to_string());

    let extraction = FileExtraction {
        file: "cls.py".to_string(),
        language: "python".to_string(),
        content_hash: "cl".to_string(),
        symbols: vec![
            sym(
                "cls.py::MyClass",
                "MyClass",
                SymbolKind::Class,
                "cls.py",
                1,
                20,
            ),
            method,
        ],
        relations: vec![],
        statements: vec![],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("cls.py").unwrap();

    let lines: Vec<&str> = out.lines().collect();
    let class_line = lines.iter().find(|l| l.contains("MyClass")).unwrap();
    let method_line = lines.iter().find(|l| l.contains("do_thing")).unwrap();

    fn content_after_colon(line: &str) -> &str {
        line.find(": ").map(|i| &line[i + 2..]).unwrap_or(line)
    }
    let class_content = content_after_colon(class_line);
    let method_content = content_after_colon(method_line);
    let class_indent = class_content.len() - class_content.trim_start().len();
    let method_indent = method_content.len() - method_content.trim_start().len();
    assert!(
        method_indent > class_indent,
        "method should be indented more than class: class={class_indent}, method={method_indent}"
    );
}

#[test]
fn test_skeleton_no_annotations_on_class() {
    let tg = TestGraph::new();
    let extraction = FileExtraction {
        file: "noann.py".to_string(),
        language: "python".to_string(),
        content_hash: "na".to_string(),
        symbols: vec![sym(
            "noann.py::Base",
            "Base",
            SymbolKind::Class,
            "noann.py",
            1,
            20,
        )],
        relations: vec![],
        statements: vec![],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("noann.py").unwrap();

    let lines: Vec<&str> = out.lines().collect();
    let class_idx = lines.iter().position(|l| l.contains("Base")).unwrap();
    let next_line = lines.get(class_idx + 1).unwrap_or(&"");
    assert!(
        !next_line.contains("# complexity:"),
        "class should not have annotation line, got: {next_line}"
    );
}

#[test]
fn test_skeleton_empty_file() {
    let tg = setup();
    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("nonexistent_file.py").unwrap();
    assert!(
        out.contains("No symbols found"),
        "empty/missing file should say no symbols: {out}"
    );
}

#[test]
fn test_skeleton_visibility_prefix() {
    let tg = TestGraph::new();
    let mut s = sym(
        "vis.py::_internal",
        "_internal",
        SymbolKind::Function,
        "vis.py",
        1,
        10,
    );
    s.visibility = Some("private".to_string());

    let extraction = FileExtraction {
        file: "vis.py".to_string(),
        language: "python".to_string(),
        content_hash: "vp".to_string(),
        symbols: vec![s],
        relations: vec![],
        statements: vec![],
    };
    {
        let conn = tg.store.connection().unwrap();
        tg.store.upsert_all_bulk(&conn, &[extraction]).unwrap();
    }

    let conn = tg.store.connection().unwrap();
    let q = GraphQuery::new(&conn);
    let out = q.skeleton("vis.py").unwrap();
    assert!(
        out.contains("private _internal"),
        "should show visibility prefix for non-public: {out}"
    );
}

// ---------- Parquet vs Bulk write equivalence ----------

#[test]
fn test_parquet_vs_bulk_write_equivalence() {
    let extractions = fixture_extractions();

    let tg_parquet = TestGraph::new();
    tg_parquet.store.upsert_all_parquet(&extractions).unwrap();

    let tg_bulk = TestGraph::new();
    {
        let conn = tg_bulk.store.connection().unwrap();
        tg_bulk.store.upsert_all_bulk(&conn, &extractions).unwrap();
    }

    let stats_pq = tg_parquet.store.stats().unwrap();
    let stats_bulk = tg_bulk.store.stats().unwrap();
    assert_eq!(
        stats_pq.symbols, stats_bulk.symbols,
        "symbol count mismatch"
    );
    assert_eq!(
        stats_pq.modules, stats_bulk.modules,
        "module count mismatch"
    );
    assert_eq!(stats_pq.files, stats_bulk.files, "file count mismatch");
    assert_eq!(stats_pq.calls, stats_bulk.calls, "calls count mismatch");
    assert_eq!(
        stats_pq.inherits, stats_bulk.inherits,
        "inherits count mismatch"
    );
}
