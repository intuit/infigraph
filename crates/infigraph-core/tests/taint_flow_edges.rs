use infigraph_core::graph::{GraphBackend, KuzuBackend, TaintFlowEdge};
use infigraph_core::model::{FileExtraction, Span, Symbol, SymbolKind};

fn sym(id: &str, name: &str, file: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: name.to_string(),
        kind: SymbolKind::Function,
        span: Span {
            file: file.to_string(),
            start_line: 1,
            start_col: 0,
            end_line: 5,
            end_col: 0,
        },
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

fn seed(backend: &KuzuBackend) {
    backend
        .upsert_file(&FileExtraction {
            file: "app.py".to_string(),
            language: "python".to_string(),
            content_hash: "h1".to_string(),
            symbols: vec![
                sym("app.py::handler", "handler", "app.py"),
                sym("app.py::other", "other", "app.py"),
            ],
            relations: vec![],
            statements: vec![],
        })
        .unwrap();
}

fn edge(symbol: &str, sink: &str) -> TaintFlowEdge {
    TaintFlowEdge {
        symbol_id: symbol.to_string(),
        source_kind: "HttpParam".to_string(),
        sink_kind: sink.to_string(),
        path: format!("L1: x <- HttpParam -> L2: {sink}(x)"),
    }
}

/// Every flow must land in one call. `write_taint_flows` used to issue a
/// separate `raw_query` per flow, and Kùzu's `raw_query` takes a fresh
/// connection each time -- so 500-odd flows meant 500-odd connections and
/// transactions, measured at ~4.5 ms each, which made this the single
/// largest cost of an incremental reindex.
#[test]
fn replace_taint_flows_writes_every_flow_in_one_call() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed(&backend);

    let flows = vec![
        edge("app.py::handler", "SqlQuery"),
        edge("app.py::handler", "CommandExec"),
        edge("app.py::other", "FileWrite"),
    ];
    backend.replace_taint_flows(&flows).unwrap();

    let rows = backend
        .raw_query("MATCH ()-[r:TAINT_FLOW]->() RETURN r.sink_kind ORDER BY r.sink_kind")
        .unwrap();
    assert_eq!(rows.len(), 3, "expected all three flows, got {rows:?}");
    assert_eq!(rows[0][0], "CommandExec");
    assert_eq!(rows[1][0], "FileWrite");
    assert_eq!(rows[2][0], "SqlQuery");
}

/// The call replaces rather than accumulates: a second run with different
/// flows must leave only the second run's edges, or repeated indexing would
/// pile up duplicate TAINT_FLOW edges forever.
#[test]
fn replace_taint_flows_replaces_previous_flows() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed(&backend);

    backend
        .replace_taint_flows(&[edge("app.py::handler", "SqlQuery")])
        .unwrap();
    backend
        .replace_taint_flows(&[edge("app.py::other", "FileWrite")])
        .unwrap();

    let rows = backend
        .raw_query("MATCH ()-[r:TAINT_FLOW]->() RETURN r.sink_kind")
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "expected only the second run's flow: {rows:?}"
    );
    assert_eq!(rows[0][0], "FileWrite");
}

/// An empty flow list still clears what was there -- a reindex that finds no
/// taint must not leave the previous run's flows behind, which is why the
/// clear is unconditional rather than an early return on empty input.
#[test]
fn replace_taint_flows_with_no_flows_clears_previous_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed(&backend);

    backend
        .replace_taint_flows(&[edge("app.py::handler", "SqlQuery")])
        .unwrap();
    backend.replace_taint_flows(&[]).unwrap();

    let rows = backend
        .raw_query("MATCH ()-[r:TAINT_FLOW]->() RETURN r.sink_kind")
        .unwrap();
    assert!(rows.is_empty(), "expected no flows left, got {rows:?}");
}
