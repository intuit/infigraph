use infigraph_core::extract::extract_file;
use infigraph_core::graph::GraphStore;
use infigraph_core::model::{FileExtraction, Relation, RelationKind, Span, Symbol, SymbolKind};
use infigraph_core::resolve;
use infigraph_languages::bundled_registry;

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

fn call(src: &str, tgt: &str) -> Relation {
    Relation {
        source_id: src.to_string(),
        target_id: tgt.to_string(),
        kind: RelationKind::Calls,
        span: None,
        receiver: None,
    }
}

fn call_with_receiver(src: &str, tgt: &str, recv: &str) -> Relation {
    Relation {
        source_id: src.to_string(),
        target_id: tgt.to_string(),
        kind: RelationKind::Calls,
        span: None,
        receiver: Some(recv.to_string()),
    }
}

fn import(src_file: &str, target_module: &str) -> Relation {
    Relation {
        source_id: src_file.to_string(),
        target_id: target_module.to_string(),
        kind: RelationKind::Imports,
        span: None,
        receiver: None,
    }
}

fn inherits(child: &str, parent: &str) -> Relation {
    Relation {
        source_id: child.to_string(),
        target_id: parent.to_string(),
        kind: RelationKind::Inherits,
        span: None,
        receiver: None,
    }
}

struct TestEnv {
    _dir: tempfile::TempDir,
    store: GraphStore,
}

impl TestEnv {
    fn new(extractions: &[FileExtraction]) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let store = GraphStore::open(&dir.path().join("graph")).unwrap();
        {
            let conn = store.connection().unwrap();
            store.upsert_all_bulk(&conn, extractions).unwrap();
        }
        Self { _dir: dir, store }
    }
}

// ---------- resolve_calls (local symbol table only) ----------

#[test]
fn test_resolve_cross_file_call() {
    let extractions = vec![
        FileExtraction {
            file: "main.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "main.py::run",
                "run",
                SymbolKind::Function,
                "main.py",
                1,
                5,
            )],
            relations: vec![
                // run() calls authenticate() — but target is wrongly scoped to main.py
                call("main.py::run", "main.py::authenticate"),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "auth.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![sym(
                "auth.py::authenticate",
                "authenticate",
                SymbolKind::Function,
                "auth.py",
                1,
                10,
            )],
            relations: vec![],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(stats.total_calls, 1, "one dangling call");
    assert_eq!(stats.resolved, 1, "should resolve to auth.py::authenticate");
    assert_eq!(stats.unresolved, 0);
}

#[test]
fn test_resolve_no_dangling_calls() {
    let extractions = vec![FileExtraction {
        file: "f.py".to_string(),
        language: "python".to_string(),
        content_hash: "a".to_string(),
        symbols: vec![
            sym("f.py::a", "a", SymbolKind::Function, "f.py", 1, 5),
            sym("f.py::b", "b", SymbolKind::Function, "f.py", 7, 10),
        ],
        relations: vec![
            call("f.py::a", "f.py::b"), // local call — already resolved
        ],
        statements: vec![],
    }];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(stats.total_calls, 0, "no dangling calls");
    assert_eq!(stats.resolved, 0);
}

#[test]
fn test_resolve_receiver_aware() {
    let extractions = vec![
        FileExtraction {
            file: "main.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "main.py::handler",
                "handler",
                SymbolKind::Function,
                "main.py",
                1,
                10,
            )],
            relations: vec![
                call_with_receiver("main.py::handler", "main.py::save", "User"),
                import("main.py", "models"),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "models.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![
                sym(
                    "models.py::User",
                    "User",
                    SymbolKind::Class,
                    "models.py",
                    1,
                    20,
                ),
                sym(
                    "models.py::User::save",
                    "save",
                    SymbolKind::Method,
                    "models.py",
                    5,
                    15,
                ),
                sym(
                    "models.py::Admin::save",
                    "save",
                    SymbolKind::Method,
                    "models.py",
                    22,
                    30,
                ),
            ],
            relations: vec![],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(stats.resolved, 1, "should resolve User.save()");

    // Verify it resolved to User::save, not Admin::save
    let conn = env.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let callees = q.callees_of("main.py::handler").unwrap();
    assert!(
        callees.iter().any(|c| c.contains("User::save")),
        "should resolve to User::save, got: {:?}",
        callees
    );
}

#[test]
fn test_resolve_receiver_with_no_local_symbol_becomes_external_call() {
    // Mirrors the real gap found in tto-engine-master: GetMappingHandler.cpp
    // calls tpsContext.GetInstanceWithUUIDs(...) where `tpsContext` resolves
    // to a real class name (ITpsContext), but ITpsContext.h's source isn't
    // indexed anywhere in this repo (statically-linked lib, no source
    // available) — before this fix, the call vanished with zero trace at
    // write time, not even counted anywhere queryable. Now it should
    // produce an EXTERNAL_CALL edge to an ExternalRef node instead.
    let extractions = vec![FileExtraction {
        file: "handler.cpp".to_string(),
        language: "cpp".to_string(),
        content_hash: "a".to_string(),
        symbols: vec![sym(
            "handler.cpp::MapTPSPathToMEF",
            "MapTPSPathToMEF",
            SymbolKind::Function,
            "handler.cpp",
            1,
            10,
        )],
        relations: vec![call_with_receiver(
            "handler.cpp::MapTPSPathToMEF",
            "handler.cpp::GetInstanceWithUUIDs",
            "ITpsContext",
        )],
        statements: vec![],
    }];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(
        stats.resolved, 0,
        "no real Symbol exists for ITpsContext — this must not fabricate a CALLS edge"
    );

    let conn = env.store.connection().unwrap();
    let rows = conn
        .query(
            "MATCH (a:Symbol)-[:EXTERNAL_CALL]->(e:ExternalRef) \
             WHERE a.id = 'handler.cpp::MapTPSPathToMEF' \
             RETURN e.qualifier, e.method",
        )
        .unwrap();
    let mut found = false;
    for row in rows {
        let vals: Vec<String> = row.into_iter().map(|v| v.to_string()).collect();
        if vals[0].contains("ITpsContext") && vals[1].contains("GetInstanceWithUUIDs") {
            found = true;
        }
    }
    assert!(
        found,
        "expected an EXTERNAL_CALL edge to an ExternalRef(qualifier=ITpsContext, \
         method=GetInstanceWithUUIDs) node, found none"
    );
}

#[test]
fn test_resolve_import_scope_preference() {
    let extractions = vec![
        FileExtraction {
            file: "main.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "main.py::run",
                "run",
                SymbolKind::Function,
                "main.py",
                1,
                5,
            )],
            relations: vec![
                call("main.py::run", "main.py::process"),
                import("main.py", "utils"),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "utils.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![sym(
                "utils.py::process",
                "process",
                SymbolKind::Function,
                "utils.py",
                1,
                10,
            )],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "other.py".to_string(),
            language: "python".to_string(),
            content_hash: "c".to_string(),
            symbols: vec![sym(
                "other.py::process",
                "process",
                SymbolKind::Function,
                "other.py",
                1,
                10,
            )],
            relations: vec![],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(stats.resolved, 1);

    let conn = env.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let callees = q.callees_of("main.py::run").unwrap();
    assert!(
        callees.iter().any(|c| c.contains("utils.py")),
        "should prefer imported module, got: {:?}",
        callees
    );
}

// ---------- resolve_calls_incremental (full graph symbol table) ----------

#[test]
fn test_resolve_incremental_uses_full_graph() {
    let initial = vec![FileExtraction {
        file: "lib.py".to_string(),
        language: "python".to_string(),
        content_hash: "x".to_string(),
        symbols: vec![sym(
            "lib.py::helper",
            "helper",
            SymbolKind::Function,
            "lib.py",
            1,
            5,
        )],
        relations: vec![],
        statements: vec![],
    }];

    let env = TestEnv::new(&initial);

    // Now "incrementally" add a new file that calls helper
    let new_files = vec![FileExtraction {
        file: "app.py".to_string(),
        language: "python".to_string(),
        content_hash: "y".to_string(),
        symbols: vec![sym(
            "app.py::main",
            "main",
            SymbolKind::Function,
            "app.py",
            1,
            5,
        )],
        relations: vec![call("app.py::main", "app.py::helper")],
        statements: vec![],
    }];
    {
        let conn = env.store.connection().unwrap();
        env.store.upsert_all_bulk(&conn, &new_files).unwrap();
    }

    // resolve_calls_incremental uses get_all_symbols() from the full graph
    let stats = resolve::resolve_calls_incremental(&env.store, &new_files, None).unwrap();
    assert_eq!(stats.resolved, 1, "should resolve helper from full graph");
}

// ---------- resolve_inherits ----------

#[test]
fn test_resolve_cross_file_inheritance() {
    let extractions = vec![
        FileExtraction {
            file: "base.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "base.py::Animal",
                "Animal",
                SymbolKind::Class,
                "base.py",
                1,
                10,
            )],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "pets.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![sym(
                "pets.py::Dog",
                "Dog",
                SymbolKind::Class,
                "pets.py",
                1,
                10,
            )],
            relations: vec![
                inherits("pets.py::Dog", "pets.py::Animal"),
                import("pets.py", "base"),
            ],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert!(
        stats.inherits_resolved >= 1,
        "should resolve Dog->Animal inheritance"
    );

    let conn = env.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let hier = q.get_type_hierarchy("base.py::Animal", 3).unwrap();
    assert!(
        hier.descendants.iter().any(|d| d.name == "Dog"),
        "Dog should be descendant of Animal"
    );
}

// ---------- re_resolve_for_files ----------

#[test]
fn test_re_resolve_for_specific_files() {
    let extractions = vec![
        FileExtraction {
            file: "a.py".to_string(),
            language: "python".to_string(),
            content_hash: "1".to_string(),
            symbols: vec![sym("a.py::foo", "foo", SymbolKind::Function, "a.py", 1, 5)],
            relations: vec![call("a.py::foo", "a.py::bar")],
            statements: vec![],
        },
        FileExtraction {
            file: "b.py".to_string(),
            language: "python".to_string(),
            content_hash: "2".to_string(),
            symbols: vec![sym("b.py::bar", "bar", SymbolKind::Function, "b.py", 1, 5)],
            relations: vec![],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);

    let stats =
        resolve::re_resolve_for_files(&env.store, &["a.py".to_string()], &extractions, None)
            .unwrap();

    assert_eq!(stats.resolved, 1, "should re-resolve foo->bar");
}

// ---------- Edge cases ----------

#[test]
fn test_resolve_empty_extractions() {
    let env = TestEnv::new(&[]);
    let stats = resolve::resolve_calls_incremental(&env.store, &[], None).unwrap();
    assert_eq!(stats.total_calls, 0);
    assert_eq!(stats.resolved, 0);
}

#[test]
fn test_resolve_class_method_calling_imported_function_no_duplicate_edge() {
    // Regression for AIF3X-331 #14: a class method's call site is emitted with
    // an unqualified source_id (file::method, no class segment), which forces
    // the file_name_to_ids fallback expansion in resolve_with_map. That map is
    // populated once from extractions and once from symbol_map, so the same
    // target id could be pushed twice for one (file, name) key — fanning a
    // single call site out into two identical CALLS edges.
    let extractions = vec![
        FileExtraction {
            file: "risk_service.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "risk_service.py::do_input_risk_screening",
                "do_input_risk_screening",
                SymbolKind::Function,
                "risk_service.py",
                1,
                3,
            )],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "chat_service.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![
                sym(
                    "chat_service.py::ChatService",
                    "ChatService",
                    SymbolKind::Class,
                    "chat_service.py",
                    1,
                    10,
                ),
                sym(
                    "chat_service.py::ChatService::process_chat_request",
                    "process_chat_request",
                    SymbolKind::Method,
                    "chat_service.py",
                    4,
                    9,
                ),
            ],
            relations: vec![
                // Unqualified source_id, as find_enclosing_function emits it.
                call(
                    "chat_service.py::process_chat_request",
                    "risk_service.py::do_input_risk_screening",
                ),
                import("chat_service.py", "risk_service"),
            ],
            statements: vec![],
        },
    ];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();
    assert_eq!(stats.resolved, 1);

    let conn = env.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let callees = q
        .callees_of("chat_service.py::ChatService::process_chat_request")
        .unwrap();
    assert_eq!(
        callees.len(),
        1,
        "expected exactly one CALLS edge, got: {:?}",
        callees
    );
}

// ---------- AIF3X-331 #15: real extractor, relative-import + name-collision repro ----------
//
// The eval report claimed do_input_risk_screening was missing 2 of 4 real
// production callers. Every hand-crafted-relation fixture tried previously
// resolved 4/4, because a unique target name short-circuits resolution
// (resolve_with_map only consults import-scope matching when more than one
// same-named candidate exists). This test removes that short-circuit by
// creating a genuine name collision, and runs real .py source through the
// production tree-sitter registry (not hand-built Relations), since the
// python/relations.scm import query only matches `module_name: (dotted_name)`
// and silently produces zero Imports relations for a relative import.
fn extract_real(file: &str, src: &[u8]) -> FileExtraction {
    let registry = bundled_registry().unwrap();
    let pack = registry.for_extension(".py").unwrap();
    extract_file(file, src, pack).unwrap()
}

#[test]
fn test_resolve_relative_import_with_name_collision() {
    let risk_service = extract_real(
        "app/service/v3/services/risk_service.py",
        b"async def do_input_risk_screening(request):\n    return True\n",
    );
    // The collision: a second, unrelated definition sharing the same bare name.
    let decoy = extract_real(
        "tests/fakes/fake_risk.py",
        b"async def do_input_risk_screening(request):\n    return False\n",
    );
    let chat_service = extract_real(
        "app/service/v3/services/chat_service.py",
        b"from .risk_service import do_input_risk_screening\n\nclass ChatService:\n    async def process_chat_request(self, request):\n        await do_input_risk_screening(request)\n",
    );
    let messages_service = extract_real(
        "app/service/v3/services/messages_service.py",
        b"from app.service.v3.services.risk_service import do_input_risk_screening\n\nclass MessagesService:\n    async def process_messages_request(self, request):\n        await do_input_risk_screening(request)\n",
    );

    let extractions = vec![risk_service, decoy, chat_service, messages_service];
    let env = TestEnv::new(&extractions);
    resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    let conn = env.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let real_callers = q
        .callers_of("app/service/v3/services/risk_service.py::do_input_risk_screening")
        .unwrap();

    assert!(
        real_callers
            .iter()
            .any(|c| c.contains("process_chat_request")),
        "relative-import caller (chat_service, matches report's missing case) should resolve \
         to the real risk_service definition, got callers: {:?}",
        real_callers
    );
    assert!(
        real_callers
            .iter()
            .any(|c| c.contains("process_messages_request")),
        "absolute-import caller (messages_service, matches report's found case) should resolve \
         to the real risk_service definition, got callers: {:?}",
        real_callers
    );
}

#[test]
fn test_resolve_unresolvable_builtin() {
    let extractions = vec![FileExtraction {
        file: "main.py".to_string(),
        language: "python".to_string(),
        content_hash: "a".to_string(),
        symbols: vec![sym(
            "main.py::work",
            "work",
            SymbolKind::Function,
            "main.py",
            1,
            5,
        )],
        relations: vec![
            call("main.py::work", "main.py::print"), // builtin, not in symbol table
        ],
        statements: vec![],
    }];

    let env = TestEnv::new(&extractions);
    let stats = resolve::resolve_calls(&env.store, &extractions, None).unwrap();

    assert_eq!(stats.total_calls, 1);
    assert_eq!(stats.unresolved, 1, "builtin call should be unresolved");
}

#[test]
fn test_resolve_stats_display() {
    let stats = resolve::ResolveStats {
        total_calls: 10,
        resolved: 7,
        unresolved: 3,
        learned_resolved: 2,
        inherits_resolved: 1,
    };
    let display = format!("{stats}");
    assert!(display.contains("10"));
    assert!(display.contains("7 resolved"));
    assert!(display.contains("2 from learned"));
    assert!(display.contains("1 inheritance"));
}

#[test]
fn test_resolve_stats_display_no_learned() {
    let stats = resolve::ResolveStats {
        total_calls: 5,
        resolved: 3,
        unresolved: 2,
        learned_resolved: 0,
        inherits_resolved: 0,
    };
    let display = format!("{stats}");
    assert!(
        !display.contains("learned"),
        "should not mention learned when 0"
    );
}

// ---------- Learned resolution: interface dispatch scenario ----------
// Simulates the assessment's Example 1: processor.charge() with multiple
// implementations. Without learned store, resolver can't pick the right impl.
// With learned store (e.g. populated from SCIP), it resolves correctly.

#[test]
fn test_learned_resolves_interface_dispatch() {
    use infigraph_core::learned::LearnedStore;

    // Scene: service.py calls processor.charge(request)
    // Three implementations exist: CardPaymentProcessor, BankPaymentProcessor, WalletPaymentProcessor
    // AST sees the call but can't determine which impl — all three have charge().
    let extractions = vec![
        FileExtraction {
            file: "service.py".to_string(),
            language: "python".to_string(),
            content_hash: "a".to_string(),
            symbols: vec![sym(
                "service.py::process_payment",
                "process_payment",
                SymbolKind::Function,
                "service.py",
                1,
                10,
            )],
            relations: vec![
                // The call target is scoped to source file (unresolved cross-file call)
                call("service.py::process_payment", "service.py::charge"),
            ],
            statements: vec![],
        },
        FileExtraction {
            file: "card_processor.py".to_string(),
            language: "python".to_string(),
            content_hash: "b".to_string(),
            symbols: vec![
                sym(
                    "card_processor.py::CardPaymentProcessor",
                    "CardPaymentProcessor",
                    SymbolKind::Class,
                    "card_processor.py",
                    1,
                    30,
                ),
                sym(
                    "card_processor.py::CardPaymentProcessor::charge",
                    "charge",
                    SymbolKind::Method,
                    "card_processor.py",
                    5,
                    15,
                ),
            ],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "bank_processor.py".to_string(),
            language: "python".to_string(),
            content_hash: "c".to_string(),
            symbols: vec![
                sym(
                    "bank_processor.py::BankPaymentProcessor",
                    "BankPaymentProcessor",
                    SymbolKind::Class,
                    "bank_processor.py",
                    1,
                    30,
                ),
                sym(
                    "bank_processor.py::BankPaymentProcessor::charge",
                    "charge",
                    SymbolKind::Method,
                    "bank_processor.py",
                    5,
                    15,
                ),
            ],
            relations: vec![],
            statements: vec![],
        },
        FileExtraction {
            file: "wallet_processor.py".to_string(),
            language: "python".to_string(),
            content_hash: "d".to_string(),
            symbols: vec![
                sym(
                    "wallet_processor.py::WalletPaymentProcessor",
                    "WalletPaymentProcessor",
                    SymbolKind::Class,
                    "wallet_processor.py",
                    1,
                    30,
                ),
                sym(
                    "wallet_processor.py::WalletPaymentProcessor::charge",
                    "charge",
                    SymbolKind::Method,
                    "wallet_processor.py",
                    5,
                    15,
                ),
            ],
            relations: vec![],
            statements: vec![],
        },
    ];

    // --- Without learned store: three ambiguous impls, no receiver, no import hint.
    // Resolver CANNOT disambiguate → call stays unresolved. This is the gap.
    let env = TestEnv::new(&extractions);
    let stats_no_learn = resolve::resolve_calls(&env.store, &extractions, None).unwrap();
    assert_eq!(stats_no_learn.total_calls, 1);
    assert_eq!(
        stats_no_learn.resolved, 0,
        "ambiguous dispatch stays unresolved without learned store"
    );
    assert_eq!(stats_no_learn.unresolved, 1);
    assert_eq!(stats_no_learn.learned_resolved, 0);

    // --- With learned store: SCIP told us the real target is CardPaymentProcessor::charge
    let mut learned = LearnedStore::default();
    learned.record_correction(
        "service.py",
        "charge",
        "card_processor.py",
        "card_processor.py::CardPaymentProcessor::charge",
    );

    // Re-resolve with learned patterns
    let env2 = TestEnv::new(&extractions);
    let stats_learned = resolve::resolve_calls(&env2.store, &extractions, Some(&learned)).unwrap();
    assert_eq!(stats_learned.total_calls, 1);
    assert_eq!(stats_learned.resolved, 1);
    assert_eq!(
        stats_learned.learned_resolved, 1,
        "should resolve via learned pattern"
    );

    // Verify it resolved to the CORRECT impl, not just any impl
    let conn = env2.store.connection().unwrap();
    let q = infigraph_core::graph::GraphQuery::new(&conn);
    let callees = q.callees_of("service.py::process_payment").unwrap();
    assert!(
        callees
            .iter()
            .any(|c| c.contains("CardPaymentProcessor::charge")),
        "learned store should resolve to CardPaymentProcessor::charge, got: {:?}",
        callees
    );
    assert!(
        !callees.iter().any(|c| c.contains("BankPaymentProcessor")),
        "should NOT resolve to BankPaymentProcessor"
    );
    assert!(
        !callees.iter().any(|c| c.contains("WalletPaymentProcessor")),
        "should NOT resolve to WalletPaymentProcessor"
    );
}
