use infigraph_core::graph::{Concern, GraphBackend, GraphStore, KuzuBackend, ResolvesToEdge};
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
    let a = FileExtraction {
        file: "a.py".to_string(),
        language: "python".to_string(),
        content_hash: "h1".to_string(),
        symbols: vec![sym("a.py::handler", "handler", "a.py", 1, 5)],
        relations: vec![],
        statements: vec![],
    };
    let b = FileExtraction {
        file: "b.py".to_string(),
        language: "python".to_string(),
        content_hash: "h2".to_string(),
        symbols: vec![sym("b.py::worker", "worker", "b.py", 1, 5)],
        relations: vec![],
        statements: vec![],
    };
    backend.upsert_file(&a).unwrap();
    backend.upsert_file(&b).unwrap();
}

#[test]
fn replace_concerns_creates_all_concerns_and_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    let concerns = vec![
        Concern {
            symbol_id: "a.py::handler".to_string(),
            kind: "auth".to_string(),
            detail: "requires login".to_string(),
        },
        Concern {
            symbol_id: "b.py::worker".to_string(),
            kind: "caching".to_string(),
            detail: "cached result".to_string(),
        },
    ];
    backend.replace_concerns(&concerns).unwrap();

    let rows = backend
        .raw_query("MATCH (s:Symbol)-[:HAS_CONCERN]->(c:Concern) RETURN s.id, c.kind ORDER BY s.id")
        .unwrap();
    assert_eq!(rows.len(), 2, "expected both concerns linked, got {rows:?}");
    assert_eq!(rows[0][0], "a.py::handler");
    assert_eq!(rows[0][1], "auth");
    assert_eq!(rows[1][0], "b.py::worker");
    assert_eq!(rows[1][1], "caching");
}

#[test]
fn replace_concerns_replaces_rather_than_accumulates() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    backend
        .replace_concerns(&[Concern {
            symbol_id: "a.py::handler".to_string(),
            kind: "auth".to_string(),
            detail: "first run".to_string(),
        }])
        .unwrap();

    backend
        .replace_concerns(&[Concern {
            symbol_id: "b.py::worker".to_string(),
            kind: "caching".to_string(),
            detail: "second run".to_string(),
        }])
        .unwrap();

    let rows = backend
        .raw_query("MATCH (c:Concern) RETURN c.kind")
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "second call must replace, not accumulate on top of, the first: {rows:?}"
    );
    assert_eq!(rows[0][0], "caching");
}

/// Regression test for the live atomicity bug this session found: the old
/// `write_concerns` issued `BEGIN TRANSACTION`/`COMMIT` through `raw_query`,
/// which both backends deliberately no-op -- so a failure partway through
/// the recreate loop left the DETACH DELETE committed but only some of the
/// new concerns written, permanently losing the rest. This forces a
/// mid-batch failure (a duplicate Concern id, which collides on Kùzu's
/// primary key) and asserts the pre-existing concern survives -- proving
/// the whole batch rolled back together rather than partially landing.
#[test]
fn replace_concerns_rolls_back_atomically_on_mid_batch_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    backend
        .replace_concerns(&[Concern {
            symbol_id: "a.py::handler".to_string(),
            kind: "auth".to_string(),
            detail: "pre-existing, must survive the failed call below".to_string(),
        }])
        .unwrap();

    // Two concerns with the same symbol_id+kind produce the same Concern.id
    // ("b.py::worker::caching") -- the second CREATE collides with the
    // first on Kùzu's primary key and fails the whole transaction.
    let result = backend.replace_concerns(&[
        Concern {
            symbol_id: "b.py::worker".to_string(),
            kind: "caching".to_string(),
            detail: "first".to_string(),
        },
        Concern {
            symbol_id: "b.py::worker".to_string(),
            kind: "caching".to_string(),
            detail: "duplicate id -- forces a mid-batch failure".to_string(),
        },
    ]);
    assert!(
        result.is_err(),
        "a duplicate Concern id must fail, not silently succeed"
    );

    let rows = backend
        .raw_query("MATCH (c:Concern) RETURN c.kind, c.detail")
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the failed call must roll back completely -- the old concern must \
         still be present and no partial new concerns should have landed: {rows:?}"
    );
    assert_eq!(rows[0][0], "auth");
    assert_eq!(
        rows[0][1], "pre-existing, must survive the failed call below",
        "the old concern's own data must be untouched, not deleted-then-lost"
    );
}

#[test]
fn replace_resolves_to_creates_edges_and_skips_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    let edges = vec![ResolvesToEdge {
        caller_symbol: "a.py::handler".to_string(),
        target: "b.py::worker".to_string(),
        mechanism: "importlib".to_string(),
        config_source: String::new(),
    }];
    backend.replace_resolves_to(&edges).unwrap();

    let rows = backend
        .raw_query("MATCH (:Symbol)-[r:RESOLVES_TO]->(:Symbol) RETURN r.mechanism")
        .unwrap();
    assert_eq!(rows.len(), 1, "expected exactly one edge: {rows:?}");
    assert_eq!(rows[0][0], "importlib");
}

#[test]
fn replace_resolves_to_empty_still_clears_old_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = KuzuBackend::open(&tmp.path().join("graph")).unwrap();
    seed_two_symbols(&backend);

    backend
        .replace_resolves_to(&[ResolvesToEdge {
            caller_symbol: "a.py::handler".to_string(),
            target: "b.py::worker".to_string(),
            mechanism: "importlib".to_string(),
            config_source: String::new(),
        }])
        .unwrap();

    // Matches the original free function's semantics: called whenever the
    // analysis pass ran at all (non-empty `sites`), even if none of them
    // resolved this time -- old edges must still be cleared.
    backend.replace_resolves_to(&[]).unwrap();

    let rows = backend
        .raw_query("MATCH (:Symbol)-[r:RESOLVES_TO]->(:Symbol) RETURN r.mechanism")
        .unwrap();
    assert!(
        rows.is_empty(),
        "an empty edge list must still clear previously-written edges: {rows:?}"
    );
}

#[test]
fn graph_store_transaction_commits_on_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph")).unwrap();

    store
        .transaction(|conn| {
            conn.query("CREATE (:Concern {id: 'x', kind: 'k', detail: 'd'})")
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(())
        })
        .unwrap();

    let conn = store.connection().unwrap();
    let rows: Vec<_> = conn
        .query("MATCH (c:Concern) RETURN c.id")
        .unwrap()
        .collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn graph_store_transaction_rolls_back_on_err() {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph")).unwrap();

    let result: anyhow::Result<()> = store.transaction(|conn| {
        conn.query("CREATE (:Concern {id: 'x', kind: 'k', detail: 'd'})")
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        anyhow::bail!("simulated failure after the write");
    });
    assert!(result.is_err());

    let conn = store.connection().unwrap();
    let rows: Vec<_> = conn
        .query("MATCH (c:Concern) RETURN c.id")
        .unwrap()
        .collect();
    assert!(
        rows.is_empty(),
        "the write before the simulated failure must have been rolled back: {rows:?}"
    );
}
