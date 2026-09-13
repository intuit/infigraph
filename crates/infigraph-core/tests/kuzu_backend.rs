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

fn make_backend() -> (tempfile::TempDir, Box<dyn GraphBackend>) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let backend = KuzuBackend::open(&dir.path().join("graph")).expect("open");
    (dir, Box::new(backend))
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

#[test]
fn test_backend_upsert_bulk_and_stats() {
    let (_dir, backend) = make_backend();
    backend
        .upsert_files_bulk(&fixture(), true)
        .expect("bulk upsert");

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.symbols, 3, "expected 3 symbols");
    assert_eq!(stats.files, 2, "expected 2 files");
    assert!(stats.modules >= 2, "expected at least 2 modules");
}

#[test]
fn test_backend_symbols_in_file() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let syms = backend.symbols_in_file("src/main.py").expect("query");
    assert_eq!(syms.len(), 2);
    assert!(syms.iter().any(|s| s.name == "main"));
    assert!(syms.iter().any(|s| s.name == "helper"));
}

#[test]
fn test_backend_find_symbol_by_id() {
    let (_dir, backend) = make_backend();
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
fn test_backend_get_file_hashes() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let hashes = backend.get_file_hashes().expect("hashes");
    assert_eq!(hashes.get("src/main.py").map(|s| s.as_str()), Some("aaa"));
    assert_eq!(hashes.get("src/lib.py").map(|s| s.as_str()), Some("bbb"));
}

#[test]
fn test_backend_resolve_calls() {
    let (_dir, backend) = make_backend();
    let extractions = fixture();
    backend.upsert_files_bulk(&extractions, true).expect("bulk");

    let stats = backend.resolve_calls(&extractions, None).expect("resolve");
    assert!(stats.resolved > 0, "expected some resolved calls");
}

#[test]
fn test_backend_traversal_after_resolve() {
    let (_dir, backend) = make_backend();
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

    let callees_cross = backend.callees_of("src/main.py::main").expect("callees");
    assert!(
        callees_cross.iter().any(|c| c.contains("process")),
        "main should call process cross-file, got: {:?}",
        callees_cross
    );
}

#[test]
fn test_backend_remove_file() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    backend.remove_file("src/lib.py").expect("remove");

    let stats = backend.stats().expect("stats");
    assert_eq!(stats.files, 1, "one file should remain after removal");
}

#[test]
fn test_backend_incremental_upsert() {
    let (_dir, backend) = make_backend();
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
fn test_backend_raw_query() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let rows = backend
        .raw_query("MATCH (s:Symbol) RETURN s.name ORDER BY s.name")
        .expect("raw");
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_backend_skeleton() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    let skel = backend.skeleton("src/main.py").expect("skeleton");
    assert!(skel.contains("main"), "skeleton should contain main");
    assert!(skel.contains("helper"), "skeleton should contain helper");
}

#[test]
fn test_backend_trait_object() {
    fn use_backend(b: &dyn GraphBackend) -> u64 {
        b.stats().expect("stats").symbols
    }

    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");
    assert_eq!(use_backend(backend.as_ref()), 3);
}

fn interface_fixture() -> Vec<FileExtraction> {
    // Mirrors the real bug: querying transitive_impact/trace_callers on one
    // method of a multi-method interface (ITpsContext::GetValueFor) missed
    // callers that only went through a sibling method (GetFieldTypeFor).
    // Method ids are class-scoped ("file::Class::method") per the C++
    // find_parent_class fix — sibling_methods_of relies on exactly this shape.
    vec![FileExtraction {
        file: "iface.h".to_string(),
        language: "cpp".to_string(),
        content_hash: "ccc".to_string(),
        symbols: vec![
            sym(
                "iface.h::IBase",
                "IBase",
                SymbolKind::Class,
                "iface.h",
                1,
                10,
            ),
            sym(
                "iface.h::IBase::GetValueFor",
                "GetValueFor",
                SymbolKind::Method,
                "iface.h",
                2,
                2,
            ),
            sym(
                "iface.h::IBase::GetFieldTypeFor",
                "GetFieldTypeFor",
                SymbolKind::Method,
                "iface.h",
                3,
                3,
            ),
            sym(
                "iface.h::callerA",
                "callerA",
                SymbolKind::Function,
                "iface.h",
                5,
                7,
            ),
            sym(
                "iface.h::callerB",
                "callerB",
                SymbolKind::Function,
                "iface.h",
                8,
                10,
            ),
        ],
        relations: vec![
            rel(
                "iface.h::callerA",
                "iface.h::IBase::GetValueFor",
                RelationKind::Calls,
            ),
            rel(
                "iface.h::callerB",
                "iface.h::IBase::GetFieldTypeFor",
                RelationKind::Calls,
            ),
        ],
        statements: vec![],
    }]
}

#[test]
fn test_sibling_methods_of_finds_other_methods_on_same_class() {
    let (_dir, backend) = make_backend();
    backend
        .upsert_files_bulk(&interface_fixture(), true)
        .expect("bulk");

    let siblings = backend
        .sibling_methods_of("iface.h::IBase::GetValueFor")
        .expect("query");
    assert_eq!(
        siblings,
        vec!["iface.h::IBase::GetFieldTypeFor".to_string()]
    );
}

#[test]
fn test_sibling_methods_of_empty_for_non_class_scoped_symbol() {
    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture(), true).expect("bulk");

    // "src/main.py::main" has only one "::" — a free function, not a
    // class-scoped method — so there is no interface to expand.
    let siblings = backend
        .sibling_methods_of("src/main.py::main")
        .expect("query");
    assert!(siblings.is_empty());
}

#[test]
fn test_single_method_query_misses_sibling_callers_without_expansion() {
    // This is the actual bug being regression-tested: callers_of_filtered on
    // one method must NOT include callers of a sibling method — confirming
    // the narrow-scope behavior is real (so the opt-in expansion path in
    // tool_trace_callers/tool_transitive_impact has something real to fix).
    let (_dir, backend) = make_backend();
    backend
        .upsert_files_bulk(&interface_fixture(), true)
        .expect("bulk");

    let callers = backend
        .callers_of_filtered("iface.h::IBase::GetValueFor", true)
        .expect("query");
    assert_eq!(callers, vec!["iface.h::callerA".to_string()]);
    assert!(
        !callers.iter().any(|c| c == "iface.h::callerB"),
        "callerB only calls the sibling method GetFieldTypeFor, not \
         GetValueFor — it must not appear unless expand_interface is used"
    );
}

fn interface_impl_split_fixture() -> Vec<FileExtraction> {
    // GetValueFor is called (via the interface); GetFieldTypeFor never is.
    // find_uncalled_symbols flags GetFieldTypeFor alone as "0 callers" even
    // though it's a sibling of a method that's clearly reachable — the
    // classic interface/impl-split false positive filter_dead_code_candidates
    // exists to catch.
    vec![FileExtraction {
        file: "iface.h".to_string(),
        language: "cpp".to_string(),
        content_hash: "ddd".to_string(),
        symbols: vec![
            sym(
                "iface.h::IBase",
                "IBase",
                SymbolKind::Class,
                "iface.h",
                1,
                10,
            ),
            sym(
                "iface.h::IBase::GetValueFor",
                "GetValueFor",
                SymbolKind::Method,
                "iface.h",
                2,
                2,
            ),
            sym(
                "iface.h::IBase::GetFieldTypeFor",
                "GetFieldTypeFor",
                SymbolKind::Method,
                "iface.h",
                3,
                3,
            ),
            sym(
                "iface.h::callerA",
                "callerA",
                SymbolKind::Function,
                "iface.h",
                5,
                7,
            ),
        ],
        relations: vec![rel(
            "iface.h::callerA",
            "iface.h::IBase::GetValueFor",
            RelationKind::Calls,
        )],
        statements: vec![],
    }]
}

fn vendor_path_fixture() -> Vec<FileExtraction> {
    vec![
        FileExtraction {
            file: "wwwroot/js/vendor/jquery.min.js".to_string(),
            language: "javascript".to_string(),
            content_hash: "eee".to_string(),
            symbols: vec![sym(
                "wwwroot/js/vendor/jquery.min.js::noop",
                "noop",
                SymbolKind::Function,
                "wwwroot/js/vendor/jquery.min.js",
                1,
                1,
            )],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "src/app.js".to_string(),
            language: "javascript".to_string(),
            content_hash: "fff".to_string(),
            symbols: vec![sym(
                "src/app.js::orphan",
                "orphan",
                SymbolKind::Function,
                "src/app.js",
                1,
                1,
            )],
            relations: vec![],
            statements: vec![],
        },
    ]
}

#[test]
fn test_filter_dead_code_drops_interface_impl_split_false_positive() {
    use infigraph_core::graph::filter_dead_code_candidates;

    let (_dir, backend) = make_backend();
    backend
        .upsert_files_bulk(&interface_impl_split_fixture(), true)
        .expect("bulk");

    let raw = backend.find_uncalled_symbols().expect("query");
    assert!(
        raw.iter().any(|r| r.name == "GetFieldTypeFor"),
        "sanity: GetFieldTypeFor should show up in the raw uncalled list"
    );

    let filtered = filter_dead_code_candidates(backend.as_ref(), raw);
    assert!(
        !filtered.iter().any(|r| r.name == "GetFieldTypeFor"),
        "GetFieldTypeFor has a called sibling (GetValueFor) on the same \
         class — it's an interface/impl-split false positive, not real \
         dead code, and must be dropped"
    );
}

#[test]
fn test_filter_dead_code_keeps_genuinely_dead_method_with_dead_sibling() {
    use infigraph_core::graph::filter_dead_code_candidates;

    // Same shape as the interface fixture, but neither method is called —
    // both siblings are genuinely dead, so neither should be suppressed.
    let fixture = vec![FileExtraction {
        file: "iface.h".to_string(),
        language: "cpp".to_string(),
        content_hash: "ggg".to_string(),
        symbols: vec![
            sym(
                "iface.h::IBase",
                "IBase",
                SymbolKind::Class,
                "iface.h",
                1,
                10,
            ),
            sym(
                "iface.h::IBase::GetValueFor",
                "GetValueFor",
                SymbolKind::Method,
                "iface.h",
                2,
                2,
            ),
            sym(
                "iface.h::IBase::GetFieldTypeFor",
                "GetFieldTypeFor",
                SymbolKind::Method,
                "iface.h",
                3,
                3,
            ),
        ],
        relations: vec![],
        statements: vec![],
    }];

    let (_dir, backend) = make_backend();
    backend.upsert_files_bulk(&fixture, true).expect("bulk");

    let raw = backend.find_uncalled_symbols().expect("query");
    let filtered = filter_dead_code_candidates(backend.as_ref(), raw);
    assert_eq!(
        filtered.len(),
        2,
        "both methods are genuinely dead (no sibling is called) — neither \
         should be suppressed by the interface-split heuristic"
    );
}

#[test]
fn test_filter_dead_code_drops_vendored_paths() {
    use infigraph_core::graph::filter_dead_code_candidates;

    let (_dir, backend) = make_backend();
    backend
        .upsert_files_bulk(&vendor_path_fixture(), true)
        .expect("bulk");

    let raw = backend.find_uncalled_symbols().expect("query");
    assert_eq!(raw.len(), 2, "sanity: both symbols start out uncalled");

    let filtered = filter_dead_code_candidates(backend.as_ref(), raw);
    assert_eq!(
        filtered.len(),
        1,
        "the vendored jquery.min.js symbol must be dropped, leaving only \
         the real app-code orphan"
    );
    assert_eq!(filtered[0].name, "orphan");
}
