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
            file: "src/api/handler.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![
                sym(
                    "src/api/handler.py::handle_request",
                    "handle_request",
                    SymbolKind::Function,
                    "src/api/handler.py",
                    1,
                    20,
                ),
                sym(
                    "src/api/handler.py::validate_input",
                    "validate_input",
                    SymbolKind::Function,
                    "src/api/handler.py",
                    22,
                    35,
                ),
            ],
            relations: vec![
                rel(
                    "src/api/handler.py::handle_request",
                    "src/api/handler.py::validate_input",
                    RelationKind::Calls,
                ),
                rel(
                    "src/api/handler.py::handle_request",
                    "src/service/user.py::get_user",
                    RelationKind::Calls,
                ),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "src/service/user.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![
                sym(
                    "src/service/user.py::get_user",
                    "get_user",
                    SymbolKind::Function,
                    "src/service/user.py",
                    1,
                    15,
                ),
                sym(
                    "src/service/user.py::save_user",
                    "save_user",
                    SymbolKind::Function,
                    "src/service/user.py",
                    17,
                    30,
                ),
            ],
            relations: vec![
                rel(
                    "src/service/user.py::get_user",
                    "src/service/user.py::save_user",
                    RelationKind::Calls,
                ),
                rel(
                    "src/service/user.py",
                    "src/api/handler.py",
                    RelationKind::Imports,
                ),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "src/models/base.py".to_string(),
            language: "python".to_string(),
            content_hash: "c".to_string(),
            symbols: vec![
                sym(
                    "src/models/base.py::BaseModel",
                    "BaseModel",
                    SymbolKind::Class,
                    "src/models/base.py",
                    1,
                    20,
                ),
                sym(
                    "src/models/base.py::UserModel",
                    "UserModel",
                    SymbolKind::Class,
                    "src/models/base.py",
                    22,
                    40,
                ),
            ],
            relations: vec![rel(
                "src/models/base.py::UserModel",
                "src/models/base.py::BaseModel",
                RelationKind::Inherits,
            )],
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
// Export tests — Cypher, GraphML, JSON
// ============================================================

#[test]
fn test_export_cypher() {
    let tg = setup_graph();
    let mut buf = Vec::new();
    infigraph_core::export::export_cypher(&tg.backend, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();

    assert!(output.contains("CREATE"), "should have CREATE statements");
    assert!(
        output.contains("handle_request"),
        "should contain symbol names"
    );
    assert!(output.contains("CALLS"), "should contain CALLS edges");
}

#[test]
fn test_export_graphml() {
    let tg = setup_graph();
    let mut buf = Vec::new();
    infigraph_core::export::export_graphml(&tg.backend, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();

    assert!(output.contains("<graphml"), "should be valid GraphML");
    assert!(output.contains("handle_request"));
    assert!(output.contains("<edge"));
}

#[test]
fn test_export_json() {
    let tg = setup_graph();
    let mut buf = Vec::new();
    infigraph_core::export::export_json(&tg.backend, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(parsed.get("nodes").is_some(), "JSON should have nodes key");
    assert!(parsed.get("edges").is_some(), "JSON should have edges key");
}

#[test]
fn test_export_empty_graph() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    let backend = KuzuBackend::from_store(store);

    let mut buf = Vec::new();
    infigraph_core::export::export_json(&backend, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(parsed["nodes"].as_array().unwrap().is_empty());
}

// ============================================================
// Sequence diagram tests
// ============================================================

#[test]
fn test_sequence_diagram_basic() {
    let tg = setup_graph();

    let mermaid = infigraph_core::sequence::generate_sequence_mermaid(
        &tg.backend,
        "src/api/handler.py::handle_request",
        3,
    )
    .unwrap();

    assert!(
        mermaid.contains("sequenceDiagram"),
        "should start with sequenceDiagram"
    );
    assert!(
        mermaid.contains("handle_request") || mermaid.contains("handler"),
        "should reference entry symbol"
    );
}

#[test]
fn test_sequence_diagram_no_calls() {
    let tg = setup_graph();

    let mermaid = infigraph_core::sequence::generate_sequence_mermaid(
        &tg.backend,
        "src/api/handler.py::validate_input",
        3,
    )
    .unwrap();

    assert!(mermaid.contains("sequenceDiagram"));
    assert!(
        mermaid.contains("no outgoing calls"),
        "leaf symbol should show no-calls note"
    );
}

#[test]
fn test_sequence_diagram_depth_limit() {
    let tg = setup_graph();

    let shallow = infigraph_core::sequence::generate_sequence_mermaid(
        &tg.backend,
        "src/api/handler.py::handle_request",
        1,
    )
    .unwrap();

    let deep = infigraph_core::sequence::generate_sequence_mermaid(
        &tg.backend,
        "src/api/handler.py::handle_request",
        5,
    )
    .unwrap();

    // Deeper traversal should produce same or more content
    assert!(deep.len() >= shallow.len());
}

// ============================================================
// Bridges (cross-language detection) tests
// ============================================================

#[test]
fn test_detect_bridges_rust_ffi() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("ffi.rs"),
        r#"
extern "C" {
    fn sqlite3_open(filename: *const i8, db: *mut *mut u8) -> i32;
}

#[no_mangle]
pub extern "C" fn my_exported_fn() {}
"#,
    )
    .unwrap();

    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    assert!(
        result.ffi_count() >= 1,
        "should detect FFI bridge: {:?}",
        result.bridges
    );
}

#[test]
fn test_detect_bridges_grpc() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("service.proto"),
        "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (GetUserRequest) returns (User);\n}\n",
    ).unwrap();

    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    assert!(
        result.grpc_count() >= 1,
        "should detect gRPC service: {:?}",
        result.bridges
    );
}

#[test]
fn test_detect_bridges_jni() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Native.java"),
        "public class Native {\n    public native void process();\n    System.loadLibrary(\"mylib\");\n}\n",
    ).unwrap();

    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    assert!(
        result.jni_count() >= 1,
        "should detect JNI bridge: {:?}",
        result.bridges
    );
}

#[test]
fn test_detect_bridges_pinvoke() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Interop.cs"),
        "[DllImport(\"kernel32.dll\")]\nstatic extern bool CloseHandle(IntPtr handle);\n",
    )
    .unwrap();

    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    assert!(
        result.pinvoke_count() >= 1,
        "should detect P/Invoke: {:?}",
        result.bridges
    );
}

#[test]
fn test_detect_bridges_empty_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    assert!(result.bridges.is_empty());
}

#[test]
fn test_bridge_scan_result_by_kind() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "extern \"C\" { fn ext_func(); }\n").unwrap();
    std::fs::write(
        dir.path().join("service.proto"),
        "syntax = \"proto3\";\nservice Svc { rpc Do (Req) returns (Res); }\n",
    )
    .unwrap();

    let result = infigraph_core::bridges::detect_bridges(dir.path()).unwrap();
    let ffi = result.by_kind(&infigraph_core::model::BridgeKind::Ffi);
    let grpc = result.by_kind(&infigraph_core::model::BridgeKind::Grpc);
    assert!(!ffi.is_empty());
    assert!(!grpc.is_empty());
}

// ============================================================
// Diff — format helpers (full semantic_diff needs git archive)
// ============================================================

#[test]
fn test_diff_format() {
    use infigraph_core::diff::{format_diff, ChangeKind, SymbolChange, SymbolDiff};

    let diff = SymbolDiff {
        old_ref: "main".to_string(),
        new_ref: "feature".to_string(),
        changes: vec![
            SymbolChange {
                name: "new_func".to_string(),
                kind: "Function".to_string(),
                file: "api.py".to_string(),
                change: ChangeKind::Added,
                caller_count: 0,
            },
            SymbolChange {
                name: "old_func".to_string(),
                kind: "Function".to_string(),
                file: "api.py".to_string(),
                change: ChangeKind::Removed,
                caller_count: 2,
            },
            SymbolChange {
                name: "changed_func".to_string(),
                kind: "Function".to_string(),
                file: "api.py".to_string(),
                change: ChangeKind::BodyChanged,
                caller_count: 1,
            },
        ],
    };

    assert_eq!(diff.added().count(), 1);
    assert_eq!(diff.removed().count(), 1);
    assert_eq!(diff.modified().count(), 1);

    let formatted = format_diff(&diff);
    assert!(
        formatted.contains("new_func"),
        "should contain added symbol"
    );
    assert!(
        formatted.contains("old_func"),
        "should contain removed symbol"
    );
    assert!(
        formatted.contains("changed_func"),
        "should contain modified symbol"
    );
    assert!(formatted.contains("main"), "should reference old ref");
    assert!(formatted.contains("feature"), "should reference new ref");
}

// ============================================================
// Viz — generate_html (needs graph)
// ============================================================

#[test]
fn test_viz_generate_html() {
    let tg = setup_graph();

    let output_path = tg._dir.path().join("graph.html");
    let result_path = infigraph_core::viz::generate_html(&tg.backend, &output_path).unwrap();
    assert!(!result_path.is_empty());
    let html = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        html.contains("<html") || html.contains("<!DOCTYPE"),
        "should produce HTML"
    );
    assert!(
        html.contains("handle_request") || html.contains("node"),
        "should contain graph data"
    );
}

#[test]
fn test_viz_generate_symbol_html() {
    let tg = setup_graph();

    let output_path = tg._dir.path().join("symbol.html");
    let result_path = infigraph_core::viz::generate_symbol_html(
        &tg.backend,
        "src/api/handler.py::handle_request",
        2,
        &output_path,
    )
    .unwrap();
    assert!(!result_path.is_empty());
    let html = std::fs::read_to_string(&output_path).unwrap();
    assert!(html.contains("<html") || html.contains("<!DOCTYPE"));
    assert!(html.contains("handle_request"));
}
