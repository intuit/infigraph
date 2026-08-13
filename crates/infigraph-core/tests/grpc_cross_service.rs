//! Comprehensive gRPC cross-repo dependency coverage (AIF3X-331 #21).
//!
//! Layers under test (all passing, local mode):
//!   1. Contract extraction (PRODUCER) — `.proto` service+rpc → GrpcService
//!      contracts. Exercises find_parent_class's `service` handling +
//!      extract_grpc_contracts.
//!   2. Client detection (CONSUMER) — SOURCE-SCAN based (#21c): a stub
//!      import/usage leaves no queryable Symbol node (external imports are
//!      dropped at index time), so consumers are found by scanning source text
//!      (scan_source_for_grpc_stubs) exactly like the HTTP consumer scan, then
//!      resolving the hit line to its enclosing symbol. One case per stub
//!      naming pattern (Python pb2_grpc/Stub, Go Stub, TS Client).
//!   3. End-to-end group build — producer + consumer → link_cross_service_calls
//!      emits a cross-service edge.
//!
//! NOTE (scope, tracked separately): the combined-graph Phase-3 promotion of
//! gRPC CALLS_SERVICE → real CALLS edges (task #21b); per-language stub-pattern
//! precision beyond the baseline substring match (tasks #30-34); and a live
//! Neo4j remote fixture in remote_cross_service.rs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use infigraph_core::extract::extract_file;
use infigraph_core::graph::{GraphBackend, KuzuBackend};
use infigraph_core::multi::combined::{build_combined_graph, combined_query};
use infigraph_core::multi::grpc::extract_grpc_contracts;
use infigraph_core::multi::{self, ContractKind, Group, Registry, RepoEntry};
use infigraph_languages::bundled_registry;

// The combined graph is a $HOME-keyed singleton per group name; serialize the
// tests that build it and isolate HOME so they don't collide (mirrors
// combined_graph.rs's COMBINED_LOCK convention).
static COMBINED_LOCK: Mutex<()> = Mutex::new(());

// ── helpers ────────────────────────────────────────────────────────────────

/// Index one or more source files (real extraction) into a fresh Kuzu backend.
fn backend_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, Box<dyn GraphBackend>) {
    let registry = bundled_registry().unwrap();
    let mut extractions = Vec::new();
    for (path, src) in files {
        let ext_dot = format!(".{}", path.rsplit('.').next().unwrap_or(""));
        let pack = registry
            .for_extension(&ext_dot)
            .unwrap_or_else(|| panic!("no language pack for {ext_dot}"));
        extractions.push(extract_file(path, src, pack).unwrap());
    }
    let dir = tempfile::TempDir::new().unwrap();
    let backend = KuzuBackend::open(&dir.path().join("graph")).unwrap();
    backend.upsert_files_bulk(&extractions, true).unwrap();
    (dir, Box::new(backend))
}

fn make_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    for (rel, content) in files {
        let p = dir.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
    dir
}

fn repo_entry(name: &str, path: &Path) -> RepoEntry {
    RepoEntry {
        name: name.to_string(),
        path: path.to_path_buf(),
        languages: vec![],
        symbol_count: 0,
        module_count: 0,
        last_indexed_commit: None,
    }
}

fn two_repo_group(producer: (&str, &Path), consumer: (&str, &Path), group: &str) -> Registry {
    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry
        .repos
        .insert(producer.0.to_string(), repo_entry(producer.0, producer.1));
    registry
        .repos
        .insert(consumer.0.to_string(), repo_entry(consumer.0, consumer.1));
    registry.groups.insert(
        group.to_string(),
        Group {
            name: group.to_string(),
            org: String::new(),
            repos: vec![producer.0.to_string(), consumer.0.to_string()],
            contracts: vec![],
        },
    );
    registry
}

const SINGLE_RPC_PROTO: &str =
    "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (GetUserRequest) returns (User);\n}\n";

// ── Layer 1: contract extraction ─────────────────────────────────────────────

#[test]
fn test_contracts_single_rpc() {
    let (_d, backend) = backend_with(&[("user.proto", SINGLE_RPC_PROTO.as_bytes())]);
    let contracts = extract_grpc_contracts(backend.as_ref());
    assert_eq!(contracts.len(), 1, "one RPC → one contract: {contracts:?}");
    assert_eq!(contracts[0].kind, ContractKind::GrpcService);
    assert_eq!(contracts[0].path, "/UserService/GetUser");
    assert_eq!(contracts[0].method, "GRPC");
}

#[test]
fn test_contracts_multi_rpc() {
    let src = "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (Req) returns (User);\n  rpc ListUsers (Req) returns (Users);\n  rpc DeleteUser (Req) returns (Empty);\n}\n";
    let (_d, backend) = backend_with(&[("user.proto", src.as_bytes())]);
    let mut paths: Vec<String> = extract_grpc_contracts(backend.as_ref())
        .into_iter()
        .map(|c| c.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "/UserService/DeleteUser",
            "/UserService/GetUser",
            "/UserService/ListUsers",
        ],
        "one contract per RPC"
    );
}

#[test]
fn test_contracts_multiple_services_one_proto() {
    let src = "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (Req) returns (User);\n}\nservice OrderService {\n  rpc GetOrder (Req) returns (Order);\n}\n";
    let (_d, backend) = backend_with(&[("svc.proto", src.as_bytes())]);
    let mut paths: Vec<String> = extract_grpc_contracts(backend.as_ref())
        .into_iter()
        .map(|c| c.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/OrderService/GetOrder", "/UserService/GetUser"],
        "RPCs grouped under their own service, not cross-linked"
    );
}

#[test]
fn test_contracts_none_without_proto() {
    // A plain Python file must yield no gRPC contracts.
    let (_d, backend) = backend_with(&[("app.py", b"def handler():\n    return 1\n")]);
    assert!(extract_grpc_contracts(backend.as_ref()).is_empty());
}

// ── Layer 2: client detection (source-scan, per stub pattern) ────────────────
//
// Client detection is source-scan based (#21c) — a stub import/usage leaves no
// queryable Symbol node — so these exercise the REAL wired path: a 2-repo group
// (proto producer + consumer with a stub reference) → detect_cross_service_deps,
// asserting a GRPC dep from consumer→producer. One case per stub naming pattern.

const PROTO_USER_SVC: &str =
    "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (Req) returns (User);\n}\n";

/// Run a producer(.proto) + consumer(source) group and return the gRPC deps.
fn grpc_deps_for(consumer_file: &str, consumer_src: &str) -> Vec<multi::CrossServiceDep> {
    let producer = make_repo(&[("user.proto", PROTO_USER_SVC)]);
    let consumer = make_repo(&[(consumer_file, consumer_src)]);
    let group = format!("grpc-client-{}", consumer_file.replace(['.', '/'], "-"));
    let mut registry = two_repo_group(
        ("user-service", producer.path()),
        ("api-gateway", consumer.path()),
        &group,
    );
    multi::index_group(&mut registry, &group, true, bundled_registry).expect("index_group");
    multi::sync_group_contracts(&mut registry, &group, bundled_registry)
        .expect("sync_group_contracts");
    multi::detect_cross_service_deps(&registry, &group, bundled_registry)
        .expect("detect_cross_service_deps")
        .into_iter()
        .filter(|d| d.target_method == "GRPC")
        .collect()
}

#[test]
fn test_client_python_pb2_grpc() {
    // Python generated stub: user_service_pb2_grpc import + UserServiceStub use.
    let deps = grpc_deps_for(
        "gateway.py",
        "from user_service_pb2_grpc import UserServiceStub\n\ndef call(chan):\n    return UserServiceStub(chan)\n",
    );
    assert!(
        deps.iter()
            .any(|d| d.target_service == "user-service" && d.caller_service == "api-gateway"),
        "should detect Python pb2_grpc / Stub client: {deps:?}"
    );
}

#[test]
fn test_client_go_stub_suffix() {
    let deps = grpc_deps_for(
        "client.go",
        "package main\n\nfunc dial() {\n    var s UserServiceStub\n    _ = s\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "should detect Go {{Service}}Stub reference: {deps:?}"
    );
}

#[test]
fn test_client_ts_client_suffix() {
    let deps = grpc_deps_for(
        "client.ts",
        "export function make(): void {\n  const c: UserServiceClient | null = null;\n  void c;\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "should detect TS {{Service}}Client reference: {deps:?}"
    );
}

#[test]
fn test_client_no_false_match_on_unrelated_name() {
    // A name that merely contains the service word but not a stub token must not link.
    let deps = grpc_deps_for(
        "helper.py",
        "class UserServiceHelper:\n    pass\n\ndef unrelated():\n    return 1\n",
    );
    assert!(
        deps.is_empty(),
        "UserServiceHelper is not a stub token — must not link: {deps:?}"
    );
}

#[test]
fn test_client_proto_producer_not_scanned_as_consumer() {
    // The stub token appears only in the producer's own .proto-adjacent code —
    // but a repo must not be scanned for a service it owns, and .proto files are
    // excluded from the client scan. No consumer here → no gRPC dep.
    let deps = grpc_deps_for("notes.md", "This file mentions UserServiceStub in prose.\n");
    // A markdown/doc file is skipped by the scanner, so no dep.
    assert!(
        deps.is_empty(),
        "doc/prose mention must not produce a gRPC dep: {deps:?}"
    );
}

// ── Layer 2b: per-language idiomatic call-site patterns (#30-34) ─────────────
//
// Beyond the bare {Svc}Stub/{Svc}Client type name, each language's generated
// gRPC client is USED via a codegen-specific idiom. These assert the real
// call/construction sites are detected, not just a type annotation.

/// Python (grpcio): the generated module import is the strongest signal. #30
#[test]
fn test_client_py_pb2_grpc_module_import() {
    let deps = grpc_deps_for(
        "svc.py",
        "import user_service_pb2_grpc as g\n\ndef make(ch):\n    return g.UserServiceStub(ch)\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "Python `import {{svc}}_pb2_grpc` must link: {deps:?}"
    );
}

/// Python server registration is a cross-service coupling too. #30
#[test]
fn test_client_py_add_servicer_to_server() {
    let deps = grpc_deps_for(
        "server.py",
        "def serve(server, impl):\n    add_UserServiceServicer_to_server(impl, server)\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "Python add_{{Svc}}Servicer_to_server must link: {deps:?}"
    );
}

/// Go (protoc-gen-go-grpc): the New{Svc}Client constructor at the call site. #31
#[test]
fn test_client_go_new_client_constructor() {
    let deps = grpc_deps_for(
        "main.go",
        "package main\n\nfunc dial(conn *grpc.ClientConn) {\n    c := pb.NewUserServiceClient(conn)\n    _ = c\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "Go New{{Svc}}Client(conn) constructor must link: {deps:?}"
    );
}

/// Java (grpc-java): {Svc}Grpc.newBlockingStub(channel). #32
#[test]
fn test_client_java_grpc_blocking_stub() {
    let deps = grpc_deps_for(
        "Client.java",
        "class Client {\n  void go(Channel ch) {\n    var s = UserServiceGrpc.newBlockingStub(ch);\n  }\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "Java {{Svc}}Grpc.newBlockingStub must link: {deps:?}"
    );
}

/// TS/JS (connect-es): createPromiseClient(Service, transport) — client built
/// from the service symbol, no {Svc}Client token. #33
#[test]
fn test_client_ts_connect_es_create_client() {
    let deps = grpc_deps_for(
        "client.ts",
        "import { createPromiseClient } from '@connectrpc/connect';\n\nexport function make(t) {\n  return createPromiseClient(UserService, t);\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "TS connect-es createPromiseClient(Svc, ...) must link: {deps:?}"
    );
}

/// Rust (tonic): {snake}_client module path + {Svc}Client::connect. #34
#[test]
fn test_client_rust_tonic_module_path() {
    let deps = grpc_deps_for(
        "client.rs",
        "use user_service_client::UserServiceClient;\n\nasync fn dial() {\n    let _c = UserServiceClient::connect(\"http://x\").await;\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "Rust tonic {{snake}}_client::{{Svc}}Client must link: {deps:?}"
    );
}

/// connect-es factory WITHOUT the service name on the line must not link
/// (guards the same-line requirement for the createClient marker). #33
#[test]
fn test_client_connect_es_requires_service_on_same_line() {
    let deps = grpc_deps_for(
        "client.ts",
        "import { createPromiseClient } from '@connectrpc/connect';\n\nexport function make(t) {\n  return createPromiseClient(SomeOtherService, t);\n}\n",
    );
    assert!(
        deps.is_empty(),
        "createPromiseClient for a different service must not link user-service: {deps:?}"
    );
}

/// C++ (grpc C++ codegen): {Svc}::NewStub(channel) factory call. #34
#[test]
fn test_client_cpp_newstub_constructor() {
    let deps = grpc_deps_for(
        "client.cc",
        "#include \"user_service.grpc.pb.h\"\n\nvoid Connect(std::shared_ptr<grpc::Channel> channel) {\n  std::unique_ptr<UserService::Stub> stub = UserService::NewStub(channel);\n}\n",
    );
    assert!(
        deps.iter().any(|d| d.target_service == "user-service"),
        "C++ {{Svc}}::NewStub(channel) constructor must link: {deps:?}"
    );
}

// ── Layer 3: end-to-end local group build ────────────────────────────────────

const PROTO_PRODUCER: &str =
    "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (GetUserRequest) returns (User);\n}\n";

const PY_CONSUMER: &str = r#"from user_service_pb2_grpc import UserServiceStub

class Gateway:
    def __init__(self, channel):
        self.stub = UserServiceStub(channel)

    def fetch(self, uid):
        return self.stub.GetUser(uid)
"#;

/// PRODUCER side (works today): a .proto producer's service becomes a
/// GrpcService contract in the group after index → sync_group_contracts.
#[test]
fn test_local_grpc_producer_contract_lands_in_group() {
    let producer = make_repo(&[("user.proto", PROTO_PRODUCER)]);
    let consumer = make_repo(&[("gateway.py", PY_CONSUMER)]);
    let mut registry = two_repo_group(
        ("user-service", producer.path()),
        ("api-gateway", consumer.path()),
        "grpc-contract-group",
    );

    multi::index_group(&mut registry, "grpc-contract-group", true, bundled_registry)
        .expect("index_group");
    let contract_count =
        multi::sync_group_contracts(&mut registry, "grpc-contract-group", bundled_registry)
            .expect("sync_group_contracts");
    assert!(
        contract_count > 0,
        "expected a GrpcService contract from the .proto producer"
    );

    let group = registry.groups.get("grpc-contract-group").unwrap();
    assert!(
        group
            .contracts
            .iter()
            .any(|c| c.kind == ContractKind::GrpcService && c.path.starts_with("/UserService/")),
        "group should carry the UserService gRPC contract: {:?}",
        group.contracts
    );
}

/// FULL end-to-end (#21c): consumer references UserServiceStub, so
/// link_cross_service_calls emits a cross-service edge into the producer. Client
/// detection is source-scan based (the stub reference leaves no graph trace).
#[test]
fn test_local_grpc_end_to_end_links_consumer_to_producer() {
    let producer = make_repo(&[("user.proto", PROTO_PRODUCER)]);
    let consumer = make_repo(&[("gateway.py", PY_CONSUMER)]);
    let mut registry = two_repo_group(
        ("user-service", producer.path()),
        ("api-gateway", consumer.path()),
        "grpc-test-group",
    );

    multi::index_group(&mut registry, "grpc-test-group", true, bundled_registry)
        .expect("index_group");
    multi::sync_group_contracts(&mut registry, "grpc-test-group", bundled_registry)
        .expect("sync_group_contracts");

    let linked = multi::link_cross_service_calls(&registry, "grpc-test-group", bundled_registry)
        .expect("link_cross_service_calls");
    assert!(
        linked > 0,
        "expected a cross-service edge from api-gateway (UserServiceStub) into \
         user-service's gRPC service"
    );
}

/// The producer repo must not be recorded as its own consumer: the .proto file
/// defines the service, but that is not a client dependency.
#[test]
fn test_local_grpc_producer_does_not_self_link() {
    let producer = make_repo(&[("user.proto", PROTO_PRODUCER)]);
    // Consumer here has NO stub reference — so the only gRPC symbols are in the
    // producer's own proto. No cross-service gRPC edge should be produced.
    let consumer = make_repo(&[("unrelated.py", "def noop():\n    return 0\n")]);
    let mut registry = two_repo_group(
        ("user-service", producer.path()),
        ("api-gateway", consumer.path()),
        "grpc-self-test-group",
    );

    multi::index_group(
        &mut registry,
        "grpc-self-test-group",
        true,
        bundled_registry,
    )
    .expect("index_group");
    multi::sync_group_contracts(&mut registry, "grpc-self-test-group", bundled_registry)
        .expect("sync_group_contracts");
    let deps =
        multi::detect_cross_service_deps(&registry, "grpc-self-test-group", bundled_registry)
            .expect("detect_cross_service_deps");
    assert!(
        !deps
            .iter()
            .any(|d| d.target_method == "GRPC" && d.caller_service == "user-service"),
        "producer must not self-link as a gRPC consumer: {deps:?}"
    );
}

// ── Layer 4: combined-graph promotion (#21b) ─────────────────────────────────

/// Combined-graph Phase 3 promotes the service-level gRPC CALLS_SERVICE edge
/// into a real CALLS edge landing on a concrete RPC symbol of the producer.
/// The dep is service-level (path "/UserService"); the GrpcService contract is
/// RPC-level (path "/UserService/GetUser") — Phase 3 reconciles the granularity
/// by keying the contract map at "/Service" (the fix in combined.rs).
///
#[test]
fn test_combined_graph_promotes_grpc_call_to_real_edge() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    let producer = make_repo(&[("user.proto", PROTO_PRODUCER)]);
    let consumer = make_repo(&[("gateway.py", PY_CONSUMER)]);
    let mut registry = two_repo_group(
        ("user-service", producer.path()),
        ("api-gateway", consumer.path()),
        "grpc-combined-group",
    );

    multi::index_group(&mut registry, "grpc-combined-group", true, bundled_registry)
        .expect("index_group");
    multi::sync_group_contracts(&mut registry, "grpc-combined-group", bundled_registry)
        .expect("sync_group_contracts");
    // link writes CALLS_SERVICE edges into the caller's per-repo graph, which
    // build_combined_graph then merges and promotes.
    let linked =
        multi::link_cross_service_calls(&registry, "grpc-combined-group", bundled_registry)
            .expect("link_cross_service_calls");
    assert!(
        linked > 0,
        "expected a gRPC CALLS_SERVICE edge before combining"
    );

    build_combined_graph(&registry, "grpc-combined-group").expect("build_combined_graph");

    // A real CALLS edge from the consumer's symbol into the producer's RPC
    // symbol (GetUser) must exist — this is the promotion the fix enables.
    let rows = combined_query(
        "grpc-combined-group",
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) WHERE b.name = 'GetUser' RETURN a.id, b.id",
    )
    .expect("combined_query");

    std::env::set_var("HOME", &orig_home);

    assert!(
        !rows.is_empty(),
        "combined graph should promote the gRPC CALLS_SERVICE edge into a real \
         CALLS edge landing on the producer's GetUser RPC symbol; got no rows"
    );
}

// Remote (Neo4j shared-graph) gRPC coverage lives with the other live-DB tests
// in remote_cross_service.rs — a real fixture there (not a placeholder here) is
// tracked as a separate task. The namespace-scoping the gRPC client scan relies
// on is the same org/repo prefix the HTTP scan uses and is exercised by the
// existing remote HTTP tests.
