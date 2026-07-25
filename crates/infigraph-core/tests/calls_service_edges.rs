use infigraph_core::graph::{CallsServiceEdge, GraphBackend, KuzuBackend};
use infigraph_core::model::{FileExtraction, Span, Symbol, SymbolKind};

fn span(file: &str, start: u32, end: u32) -> Span {
    Span {
        file: file.to_string(),
        start_line: start,
        start_col: 0,
        end_line: end,
        end_col: 0,
    }
}

fn sym(id: &str, name: &str, file: &str, start: u32, end: u32) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: name.to_string(),
        kind: SymbolKind::Function,
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

fn seed_two_symbols(backend: &KuzuBackend) {
    let src = FileExtraction {
        file: "caller.py".to_string(),
        language: "python".to_string(),
        content_hash: "h1".to_string(),
        symbols: vec![sym("caller.py::handler", "handler", "caller.py", 1, 5)],
        relations: vec![],
        statements: vec![],
    };
    let tgt = FileExtraction {
        file: "target.py".to_string(),
        language: "python".to_string(),
        content_hash: "h2".to_string(),
        symbols: vec![sym("target.py::endpoint", "endpoint", "target.py", 1, 5)],
        relations: vec![],
        statements: vec![],
    };
    backend.upsert_file(&src).unwrap();
    backend.upsert_file(&tgt).unwrap();
}

/// Regression test for the exact bug found this session: writing more than
/// one CALLS_SERVICE edge used to fail with "No active transaction for
/// COMMIT" because BEGIN/CREATE-loop/COMMIT were three separate raw_query
/// calls, each silently getting its own fresh Kùzu connection. This test
/// writes two edges (the loop must run more than once for the old bug to
/// reliably manifest) and asserts both real edges exist afterward.
#[test]
fn write_calls_service_edges_creates_all_edges_in_one_call() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    let edges = vec![
        CallsServiceEdge {
            symbol_id: "caller.py::handler".to_string(),
            target_id: "target.py::endpoint".to_string(),
            method: "GET".to_string(),
            path: "/api/one".to_string(),
        },
        CallsServiceEdge {
            symbol_id: "caller.py::handler".to_string(),
            target_id: "target.py::endpoint".to_string(),
            method: "POST".to_string(),
            path: "/api/two".to_string(),
        },
    ];

    backend.write_calls_service_edges(&edges).expect(
        "write_calls_service_edges must succeed, not fail with 'No active transaction for COMMIT'",
    );

    let rows = backend
        .raw_query("MATCH (:Symbol)-[r:CALLS_SERVICE]->(:Symbol) RETURN r.method, r.path ORDER BY r.method")
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "expected both edges to be created, got {rows:?}"
    );
    assert_eq!(rows[0][0], "GET");
    assert_eq!(rows[0][1], "/api/one");
    assert_eq!(rows[1][0], "POST");
    assert_eq!(rows[1][1], "/api/two");
}

/// Empty input must not error (matches the old code's behavior — the old
/// function's caller only invoked it when `!urls.is_empty()`, but the new
/// trait method should be safe to call with zero edges regardless).
#[test]
fn write_calls_service_edges_empty_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    backend.write_calls_service_edges(&[]).unwrap();
}
