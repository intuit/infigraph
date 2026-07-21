//! Integration tests for Neo4jBackend against a live Neo4j instance.
//!
//! Requires: `docker run -d -p 7687:7687 -e NEO4J_AUTH=neo4j/testpass neo4j:5-community`
//! Run: `NEO4J_URI=127.0.0.1:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=testpass cargo test -p infigraph-core --features neo4j --test neo4j_backend -- --ignored --test-threads=1`
//!
//! Tests share a single Neo4j instance and use `DETACH DELETE` for isolation,
//! so they MUST run with `--test-threads=1`.

#![cfg(feature = "neo4j")]

use infigraph_core::graph::{GraphBackend, Neo4jBackend};
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

fn rel(src: &str, tgt: &str, kind: RelationKind) -> Relation {
    Relation {
        source_id: src.to_string(),
        target_id: tgt.to_string(),
        kind,
        span: None,
        receiver: None,
    }
}

fn connect() -> Neo4jBackend {
    Neo4jBackend::connect_from_env().expect("Neo4j connection — is Docker running?")
}

fn clear_graph(backend: &Neo4jBackend) {
    backend
        .raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear graph");
}

fn fixture() -> Vec<FileExtraction> {
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
            statements: vec![],
        },
        FileExtraction {
            file: "src/lib.py".to_string(),
            language: "python".to_string(),
            content_hash: "bbb".to_string(),
            symbols: vec![sym(
                "src/lib.py::process",
                "process",
                SymbolKind::Function,
                "src/lib.py",
                1,
                15,
            )],
            relations: vec![],
            statements: vec![],
        },
    ]
}

fn fixture_namespaced(ns: &str) -> Vec<FileExtraction> {
    vec![FileExtraction {
        file: format!("{ns}/src/main.py"),
        language: "python".to_string(),
        content_hash: "aaa".to_string(),
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
        relations: vec![rel(
            &format!("{ns}/src/main.py::main"),
            &format!("{ns}/src/main.py::helper"),
            RelationKind::Calls,
        )],
        statements: vec![],
    }]
}

// ── Basic GraphBackend trait tests (mirrors kuzu_backend.rs) ─────────

#[test]
#[ignore]
fn test_neo4j_upsert_bulk_and_stats() {
    let backend = connect();
    clear_graph(&backend);

    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("bulk upsert");

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.symbols, 3, "expected 3 symbols");
    assert_eq!(stats.files, 2, "expected 2 files");
}

#[test]
#[ignore]
fn test_neo4j_symbols_in_file() {
    let backend = connect();
    clear_graph(&backend);
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let syms = backend.symbols_in_file("src/main.py").expect("query");
    assert_eq!(syms.len(), 2);
    assert!(syms.iter().any(|s| s.name == "main"));
    assert!(syms.iter().any(|s| s.name == "helper"));
}

#[test]
#[ignore]
fn test_neo4j_find_symbol_by_id() {
    let backend = connect();
    clear_graph(&backend);
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let sym = backend
        .find_symbol_by_id("src/lib.py::process")
        .expect("query");
    assert!(sym.is_some());
    let sym = sym.unwrap();
    assert_eq!(sym.name, "process");
    assert_eq!(sym.file, "src/lib.py");
}

#[test]
#[ignore]
fn test_neo4j_file_hashes() {
    let backend = connect();
    clear_graph(&backend);
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert_eq!(hashes.get("src/main.py").map(|s| s.as_str()), Some("aaa"));
    assert_eq!(hashes.get("src/lib.py").map(|s| s.as_str()), Some("bbb"));
}

#[test]
#[ignore]
fn test_neo4j_resolve_calls() {
    let backend = connect();
    clear_graph(&backend);
    let extractions = fixture();
    backend.upsert_files_bulk(&extractions, true).expect("bulk");

    let stats = backend.resolve_calls(&extractions, None).expect("resolve");
    assert!(stats.resolved > 0, "expected some resolved calls");
}

#[test]
#[ignore]
fn test_neo4j_traversal_after_resolve() {
    let backend = connect();
    clear_graph(&backend);
    let extractions = fixture();
    backend.upsert_files_bulk(&extractions, true).expect("bulk");
    backend.resolve_calls(&extractions, None).expect("resolve");

    let callees = backend.callees_of("src/main.py::main").expect("callees");
    assert!(
        callees.iter().any(|c| c.contains("helper")),
        "main should call helper, got: {:?}",
        callees
    );

    let callers = backend.callers_of("src/main.py::helper").expect("callers");
    assert!(
        callers.iter().any(|c| c.contains("main")),
        "helper should be called by main, got: {:?}",
        callers
    );
}

#[test]
#[ignore]
fn test_neo4j_remove_file() {
    let backend = connect();
    clear_graph(&backend);
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    backend.remove_file("src/lib.py").expect("remove");

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.files, 1, "one file should remain after removal");
}

#[test]
#[ignore]
fn test_neo4j_incremental_upsert() {
    let backend = connect();
    clear_graph(&backend);
    let extractions = fixture();

    backend
        .upsert_files_bulk(&extractions, true)
        .expect("fresh");
    let stats1 = backend.stats().expect("stats1");

    backend
        .upsert_files_bulk(&extractions, false)
        .expect("incremental");
    let stats2 = backend.stats().expect("stats2");

    assert_eq!(
        stats1.symbols, stats2.symbols,
        "symbol count same after re-upsert"
    );
    assert_eq!(
        stats1.files, stats2.files,
        "file count same after re-upsert"
    );
}

#[test]
#[ignore]
fn test_neo4j_raw_query() {
    let backend = connect();
    clear_graph(&backend);
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let rows = backend
        .raw_query("MATCH (s:Symbol) RETURN s.name ORDER BY s.name")
        .expect("raw");
    assert_eq!(rows.len(), 3);
}

// ── Namespace isolation tests ────────────────────────────────────────

#[test]
#[ignore]
fn test_neo4j_namespace_isolation() {
    let backend = connect();
    clear_graph(&backend);

    let repo_a = fixture_namespaced("repo-a");
    let repo_b = fixture_namespaced("repo-b");

    backend
        .upsert_files_bulk(&repo_a, true)
        .expect("upsert repo-a");
    backend
        .upsert_files_bulk(&repo_b, false)
        .expect("upsert repo-b");

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.files, 2, "2 files — one per repo namespace");
    assert_eq!(stats.symbols, 4, "4 symbols — 2 per repo namespace");

    // Each namespace has its own symbols
    let syms_a = backend
        .symbols_in_file("repo-a/src/main.py")
        .expect("syms a");
    assert_eq!(syms_a.len(), 2, "repo-a should have 2 symbols");

    let syms_b = backend
        .symbols_in_file("repo-b/src/main.py")
        .expect("syms b");
    assert_eq!(syms_b.len(), 2, "repo-b should have 2 symbols");

    // Remove one namespace — other stays
    backend.remove_file("repo-a/src/main.py").expect("remove");
    let stats = backend.stats().expect("stats after remove");
    assert_eq!(stats.files, 1, "only repo-b file should remain");
    assert_eq!(stats.symbols, 2, "only repo-b symbols should remain");
}

// ── Concurrent write tests ───────────────────────────────────────────

#[test]
#[ignore]
fn test_neo4j_concurrent_upsert() {
    use std::sync::Arc;
    use std::thread;

    let backend = Arc::new(connect());
    clear_graph(&backend);

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let b = Arc::clone(&backend);
            thread::spawn(move || {
                let ns = format!("concurrent-repo-{i}");
                let data = fixture_namespaced(&ns);
                b.upsert_files_bulk(&data, i == 0)
                    .expect("concurrent upsert");
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.files, 4, "4 files from 4 concurrent repos");
    assert_eq!(
        stats.symbols, 8,
        "8 symbols from 4 concurrent repos (2 each)"
    );
}

// ── File lifecycle tests (add / delete / modify / move) ─────────────

#[test]
#[ignore]
fn test_neo4j_add_file() {
    let backend = connect();
    clear_graph(&backend);

    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("initial bulk");
    let stats1 = backend.stats().expect("stats before add");
    assert_eq!(stats1.files, 2);
    assert_eq!(stats1.symbols, 3);

    let new_file = vec![FileExtraction {
        file: "src/utils.py".to_string(),
        language: "python".to_string(),
        content_hash: "ccc".to_string(),
        symbols: vec![sym(
            "src/utils.py::format_str",
            "format_str",
            SymbolKind::Function,
            "src/utils.py",
            1,
            8,
        )],
        relations: vec![],
        statements: vec![],
    }];
    backend
        .upsert_files_bulk(&new_file, false)
        .expect("add file");

    let stats2 = backend.stats().expect("stats after add");
    assert_eq!(stats2.files, 3, "new file should appear");
    assert_eq!(stats2.symbols, 4, "new symbol should appear");

    let syms = backend
        .symbols_in_file("src/utils.py")
        .expect("query new file");
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "format_str");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert_eq!(
        hashes.get("src/utils.py").map(|s| s.as_str()),
        Some("ccc"),
        "new file hash should be stored"
    );
}

#[test]
#[ignore]
fn test_neo4j_delete_file_full() {
    let backend = connect();
    clear_graph(&backend);

    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("initial bulk");
    let stats1 = backend.stats().expect("stats before delete");
    assert_eq!(stats1.files, 2);
    assert_eq!(stats1.symbols, 3);

    backend.remove_file("src/lib.py").expect("remove file");

    let stats2 = backend.stats().expect("stats after delete");
    assert_eq!(stats2.files, 1, "deleted file should be gone");
    assert_eq!(
        stats2.symbols, 2,
        "symbols from deleted file should be gone"
    );

    let syms = backend
        .symbols_in_file("src/lib.py")
        .expect("query deleted file");
    assert!(syms.is_empty(), "no symbols should remain for deleted file");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert!(
        hashes.get("src/lib.py").is_none(),
        "hash for deleted file should be gone"
    );

    let remaining = backend
        .symbols_in_file("src/main.py")
        .expect("query surviving file");
    assert_eq!(remaining.len(), 2, "surviving file symbols untouched");
}

#[test]
#[ignore]
fn test_neo4j_modify_file() {
    let backend = connect();
    clear_graph(&backend);

    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("initial bulk");

    let modified = vec![FileExtraction {
        file: "src/lib.py".to_string(),
        language: "python".to_string(),
        content_hash: "bbb_v2".to_string(),
        symbols: vec![
            sym(
                "src/lib.py::process",
                "process",
                SymbolKind::Function,
                "src/lib.py",
                1,
                20,
            ),
            sym(
                "src/lib.py::validate",
                "validate",
                SymbolKind::Function,
                "src/lib.py",
                22,
                35,
            ),
        ],
        relations: vec![rel(
            "src/lib.py::process",
            "src/lib.py::validate",
            RelationKind::Calls,
        )],
        statements: vec![],
    }];
    backend
        .upsert_files_bulk(&modified, false)
        .expect("modify file");

    let stats = backend.stats().expect("stats after modify");
    assert_eq!(stats.files, 2, "file count unchanged after modify");
    assert_eq!(
        stats.symbols, 4,
        "modified file now has 2 symbols (was 1) + 2 from main"
    );

    let syms = backend
        .symbols_in_file("src/lib.py")
        .expect("query modified file");
    assert_eq!(syms.len(), 2, "modified file should have 2 symbols");
    assert!(syms.iter().any(|s| s.name == "process"));
    assert!(syms.iter().any(|s| s.name == "validate"));

    let hashes = backend.get_file_hashes().expect("hashes");
    assert_eq!(
        hashes.get("src/lib.py").map(|s| s.as_str()),
        Some("bbb_v2"),
        "hash should be updated"
    );

    let main_syms = backend
        .symbols_in_file("src/main.py")
        .expect("main untouched");
    assert_eq!(main_syms.len(), 2, "unmodified file should be untouched");
}

#[test]
#[ignore]
fn test_neo4j_move_rename_file() {
    let backend = connect();
    clear_graph(&backend);

    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("initial bulk");

    backend.remove_file("src/lib.py").expect("remove old path");

    let moved = vec![FileExtraction {
        file: "src/core/lib.py".to_string(),
        language: "python".to_string(),
        content_hash: "bbb".to_string(),
        symbols: vec![sym(
            "src/core/lib.py::process",
            "process",
            SymbolKind::Function,
            "src/core/lib.py",
            1,
            15,
        )],
        relations: vec![],
        statements: vec![],
    }];
    backend
        .upsert_files_bulk(&moved, false)
        .expect("add new path");

    let stats = backend.stats().expect("stats after move");
    assert_eq!(stats.files, 2, "still 2 files (old removed, new added)");
    assert_eq!(stats.symbols, 3, "still 3 symbols total");

    let old_syms = backend
        .symbols_in_file("src/lib.py")
        .expect("query old path");
    assert!(old_syms.is_empty(), "old path should have no symbols");

    let new_syms = backend
        .symbols_in_file("src/core/lib.py")
        .expect("query new path");
    assert_eq!(new_syms.len(), 1);
    assert_eq!(new_syms[0].name, "process");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert!(hashes.get("src/lib.py").is_none(), "old path hash gone");
    assert_eq!(
        hashes.get("src/core/lib.py").map(|s| s.as_str()),
        Some("bbb"),
        "new path hash present"
    );

    let main_syms = backend
        .symbols_in_file("src/main.py")
        .expect("main untouched");
    assert_eq!(main_syms.len(), 2, "unrelated file untouched after move");
}
