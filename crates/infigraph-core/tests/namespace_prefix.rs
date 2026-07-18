//! Unit tests for namespace prefixing in multi-repo indexing.
//! No Docker required — tests against KuzuBackend.

use infigraph_core::graph::{GraphBackend, KuzuBackend};
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

fn fixture_namespaced(ns: &str) -> Vec<FileExtraction> {
    vec![FileExtraction {
        file: format!("{ns}/src/main.py"),
        language: "python".to_string(),
        content_hash: format!("hash_{ns}"),
        symbols: vec![
            sym(
                &format!("{ns}/src/main.py::main"),
                "main",
                SymbolKind::Function,
                &format!("{ns}/src/main.py"),
                1,
                10,
            ),
            sym(
                &format!("{ns}/src/main.py::helper"),
                "helper",
                SymbolKind::Function,
                &format!("{ns}/src/main.py"),
                12,
                20,
            ),
        ],
        relations: vec![Relation {
            source_id: format!("{ns}/src/main.py::main"),
            target_id: format!("{ns}/src/main.py::helper"),
            kind: RelationKind::Calls,
            span: None,
            receiver: None,
        }],
        statements: vec![],
    }]
}

#[test]
fn test_namespace_prevents_collision() {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");

    let repo_a = fixture_namespaced("repo-a");
    let repo_b = fixture_namespaced("repo-b");

    backend.upsert_files_bulk(&repo_a, true).expect("upsert a");
    backend.upsert_files_bulk(&repo_b, false).expect("upsert b");

    let stats = backend.stats().expect("stats");
    // Without namespace: both would write "src/main.py" → 1 file, 2 symbols
    // With namespace: "repo-a/src/main.py" and "repo-b/src/main.py" → 2 files, 4 symbols
    assert_eq!(stats.files, 2, "namespaced files should not collide");
    assert_eq!(stats.symbols, 4, "namespaced symbols should not collide");
}

#[test]
fn test_namespace_symbols_queryable() {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");

    backend
        .upsert_files_bulk(&fixture_namespaced("svc-auth"), true)
        .expect("upsert");

    let syms = backend
        .symbols_in_file("svc-auth/src/main.py")
        .expect("query");
    assert_eq!(syms.len(), 2);
    assert!(syms.iter().any(|s| s.name == "main"));

    let detail = backend
        .find_symbol_by_id("svc-auth/src/main.py::main")
        .expect("find");
    assert!(detail.is_some());
    assert_eq!(detail.unwrap().file, "svc-auth/src/main.py");
}

#[test]
fn test_namespace_file_hashes_isolated() {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");

    backend
        .upsert_files_bulk(&fixture_namespaced("repo-x"), true)
        .expect("x");
    backend
        .upsert_files_bulk(&fixture_namespaced("repo-y"), false)
        .expect("y");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert!(hashes.contains_key("repo-x/src/main.py"));
    assert!(hashes.contains_key("repo-y/src/main.py"));
    assert_eq!(
        hashes.get("repo-x/src/main.py").map(|s| s.as_str()),
        Some("hash_repo-x")
    );
    assert_eq!(
        hashes.get("repo-y/src/main.py").map(|s| s.as_str()),
        Some("hash_repo-y")
    );
}

#[test]
fn test_namespace_remove_one_repo() {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");

    backend
        .upsert_files_bulk(&fixture_namespaced("keep"), true)
        .expect("keep");
    backend
        .upsert_files_bulk(&fixture_namespaced("remove"), false)
        .expect("remove");

    assert_eq!(backend.stats().expect("before").files, 2);

    backend
        .remove_file("remove/src/main.py")
        .expect("remove file");

    let stats = backend.stats().expect("after");
    assert_eq!(stats.files, 1);
    assert_eq!(stats.symbols, 2);

    // Verify correct repo survived
    let syms = backend.symbols_in_file("keep/src/main.py").expect("query");
    assert_eq!(syms.len(), 2);

    let syms = backend
        .symbols_in_file("remove/src/main.py")
        .expect("query removed");
    assert_eq!(syms.len(), 0);
}

#[test]
fn test_namespace_resolve_within_repo() {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");

    let data = fixture_namespaced("my-svc");
    backend.upsert_files_bulk(&data, true).expect("upsert");
    backend.resolve_calls(&data, None).expect("resolve");

    let callees = backend
        .callees_of("my-svc/src/main.py::main")
        .expect("callees");
    assert!(
        callees.iter().any(|c| c.contains("helper")),
        "namespaced call resolution should work, got: {:?}",
        callees
    );
}
