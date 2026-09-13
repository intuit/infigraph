//! Regression test for group cross-service linking (`index_group` +
//! `sync_group_contracts` + `link_cross_service_calls`) against a LIVE
//! Neo4j + Postgres backend.
//!
//! This exercises the exact multi-repo orchestration path that
//! `infigraph group build` runs in remote mode, covering two recent
//! refactors that had zero coverage against a real Neo4j+Postgres pair:
//!   - commit 109fb5c "fix: stop rebuilding LanguageRegistry per-repo in
//!     group cross-service linking"
//!   - commit e082437 "perf(remote): build LanguageRegistry once per group
//!     build, not once per repo"
//!
//! Requires:
//!   `docker run -d --name infigraph-test-neo4j -p 7687:7687 -e NEO4J_AUTH=neo4j/testpass neo4j:5-community`
//!   `docker run -d --name infigraph-test-pg -p 5432:5432 -e POSTGRES_USER=infigraph -e POSTGRES_PASSWORD=infigraph -e POSTGRES_DB=infigraph pgvector/pgvector:pg16`
//!
//! Run:
//!   `NEO4J_URI=127.0.0.1:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=testpass \
//!    DATABASE_URL="host=localhost user=infigraph password=infigraph dbname=infigraph" \
//!    cargo test -p infigraph-core --features neo4j,postgres --test remote_cross_service -- --ignored --test-threads=1`
//!
//! Shares the same live Neo4j/Postgres instances as `neo4j_backend.rs` /
//! `postgres_registry.rs` and cleans up after itself, so it MUST run with
//! `--test-threads=1` like those files.
//!
//! FIXED — `sync_group_contracts` / `extract_contracts` and
//! `detect_cross_service_deps` (multi/mod.rs, multi/cross_service.rs) used to
//! run hand-written `raw_query` Cypher with no repo/namespace `WHERE` filter
//! against the SHARED Neo4j graph. In multi-repo remote mode this meant one
//! repo's contract-sync saw every OTHER repo's symbols too, so
//! payment-service's own `/api/payments` route contract got attributed to
//! BOTH services. That corrupted the route lookup used to resolve the real
//! order-service -> payment-service call and produced a spurious
//! payment-service -> order-service "dependency" instead (payment-service's
//! own route decorator string-matched its own now-mislabeled contract).
//! Both functions now scope their Cypher to the calling repo's own
//! `{org}/{repo}` namespace prefix on `s.file` (matching how `index_group`
//! stamps that same prefix at write time), falling back to unscoped queries
//! in local/non-namespaced mode. This is a distinct bug from the
//! already-known Q5 gap in docs/DESIGN-language-agnostic-cross-repo-linking.md
//! (Q5 is about caller-symbol resolving to the enclosing class instead of the
//! innermost method; this was about contract attribution crossing repo
//! boundaries entirely).
//!
//! This test asserts the CORRECT behavior: a real order-service ->
//! payment-service `CALLS_SERVICE` edge, and also exercises `index_group`'s
//! shared-LanguageRegistry path (commits 109fb5c/e082437 — both repos index
//! successfully via the shared registry) as a regression guard.

#![cfg(all(feature = "neo4j", feature = "postgres"))]

use std::path::Path;

use infigraph_core::graph::{GraphBackend, Neo4jBackend};
use infigraph_core::meta::PostgresMetaStore;
use infigraph_core::multi::{self, Registry, RepoEntry};

// PAYMENT_PATH is deliberately its own constant (rather than inlined into the
// fetch call): `scan_source_for_urls` only registers a named url-constant when
// the constant's OWN definition line already looks route-like (starts with
// `/api/` or `/vN/`), then resolves later `${CONST}` references against that
// table (see credit_constant_references / test_resolve_url_constant_typescript).
// PAYMENT_SERVICE_URL alone (a bare host, no path) would never register.
const ORDER_SERVICE_TS_CLIENT: &str = r#"
const PAYMENT_SERVICE_URL = "http://payment-service:8080";
const PAYMENT_PATH = "/api/payments";

export interface PaymentRequest {
  orderId: string;
  amount: number;
}

export interface PaymentResult {
  success: boolean;
  paymentId?: string;
  error?: string;
}

export class OrderPaymentClient {
  async initiatePayment(request: PaymentRequest): Promise<PaymentResult> {
    const response = await fetch(`${PAYMENT_SERVICE_URL}${PAYMENT_PATH}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
    const data = await response.json();
    return { success: true, paymentId: data.id };
  }
}
"#;

const PAYMENT_SERVICE_RS_HANDLERS: &str = r#"
use actix_web::{post, web, HttpResponse, Responder};

pub struct CreatePaymentRequest {
    pub order_id: String,
    pub amount: f64,
}

#[post("/api/payments")]
pub async fn create_payment(body: web::Json<CreatePaymentRequest>) -> impl Responder {
    HttpResponse::Created().json(body.amount)
}
"#;

/// Copy a small in-repo fixture into a fresh tempdir so indexing's
/// `.infigraph/` writes never touch the real source tree, and so each test
/// run gets a clean, git-less repo (index_group's git_head_commit() just
/// returns None for these, which is fine since we always pass full=true).
fn make_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    for (rel_path, content) in files {
        let p = dir.path().join(rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&p, content).expect("write fixture file");
    }
    dir
}

fn connect_pg() -> PostgresMetaStore {
    let store =
        PostgresMetaStore::connect_from_env().expect("Postgres connection — is Docker running?");
    store.init_schema().expect("schema init");
    store
}

fn connect_neo4j() -> Neo4jBackend {
    let neo = Neo4jBackend::connect_from_env().expect("Neo4j connection — is Docker running?");
    // Pre-create schema (indexes/constraints) single-threaded before index_group runs.
    // index_group parallelizes indexing across repos on the Neo4j backend (rayon
    // par_iter), and each repo's Infigraph::init() calls init_schema() independently.
    // `CREATE INDEX/CONSTRAINT ... IF NOT EXISTS` is not atomic under concurrent
    // execution in Neo4j — two threads can both pass the existence check before either
    // commits, causing `Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists` (see
    // neo4j_backend.rs::init_schema). That race is orthogonal to what this test
    // regresses on, so it's sidestepped here rather than worked around in production
    // code; see the test report for the bug writeup.
    neo.init_schema().expect("schema init");
    neo
}

fn clean_pg(store: &PostgresMetaStore) {
    store.execute_raw("DELETE FROM group_repos").ok();
    store.execute_raw("DELETE FROM groups").ok();
    store.execute_raw("DELETE FROM repos").ok();
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

/// Full remote-mode regression: seed a Postgres-backed Registry with two
/// repos (order-service caller, payment-service callee) in one group, run
/// the real `index_group` -> `sync_group_contracts` -> `link_cross_service_calls`
/// orchestration against live Neo4j, and assert a CALLS_SERVICE edge into
/// payment-service's /api/payments route actually got created.
#[test]
#[ignore]
fn test_group_build_links_cross_service_call_neo4j_postgres() {
    let group_name = "remote-xsvc-test-group";

    std::env::set_var("INFIGRAPH_BACKEND", "neo4j");

    let pg = connect_pg();
    clean_pg(&pg);
    let neo = connect_neo4j();
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph before test");

    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);

    pg.upsert_repo(
        "order-service",
        &repo_entry("order-service", order_dir.path()),
    )
    .expect("seed order-service repo");
    pg.upsert_repo(
        "payment-service",
        &repo_entry("payment-service", payment_dir.path()),
    )
    .expect("seed payment-service repo");
    pg.create_group(group_name).expect("create group");
    pg.group_add(group_name, "order-service")
        .expect("add order-service to group");
    pg.group_add(group_name, "payment-service")
        .expect("add payment-service to group");

    let mut registry =
        Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");
    assert!(
        registry.groups.contains_key(group_name),
        "seeded group should round-trip through Postgres"
    );

    // Step 1: index both repos for real, against the live Neo4j backend.
    // full=true drives the exact path perf commit e082437 changed: a single
    // shared LanguageRegistry (Arc) built once via build_registry() and
    // reused across repos, rather than rebuilt per-repo.
    let index_results = multi::index_group(
        &mut registry,
        group_name,
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed against live Neo4j");
    assert_eq!(index_results.len(), 2, "both repos should have indexed");
    for (repo_name, indexed_files, _total_files) in &index_results {
        assert!(
            *indexed_files > 0,
            "repo '{repo_name}' should have indexed at least one file"
        );
    }

    // Step 2: extract HTTP route contracts (payment-service's #[post("/api/payments")]).
    let contract_count = multi::sync_group_contracts(
        &mut registry,
        group_name,
        infigraph_languages::bundled_registry,
    )
    .expect("sync_group_contracts should succeed");
    assert!(
        contract_count > 0,
        "expected at least one HTTP route contract extracted from payment-service"
    );

    // Step 3: the actual regression target — link_cross_service_calls, which
    // internally calls detect_cross_service_deps (commit 109fb5c's fix site).
    let linked = multi::link_cross_service_calls(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_service_calls should succeed");
    assert!(
        linked > 0,
        "expected at least one CALLS_SERVICE-derived ExternalService node to be created \
         (linked count from link_cross_service_calls)"
    );

    // The real regression assertion: a CALLS_SERVICE edge from an
    // order-service caller into payment-service's /api/payments route, with
    // target_service correctly attributed to payment-service (not the
    // reverse — see module doc comment for the bug this guards against).
    let rows = neo
        .raw_query(
            "MATCH (caller:Symbol)-[r:CALLS_SERVICE]->(target:Symbol) \
             WHERE target.id CONTAINS 'payment-service' AND target.id CONTAINS 'api/payments' \
             AND r.target_service = 'payment-service' \
             RETURN caller.id, target.id, r.target_service",
        )
        .expect("query for CALLS_SERVICE edge into payment-service's route");
    assert!(
        !rows.is_empty(),
        "expected a CALLS_SERVICE edge from an order-service caller into \
         payment-service's /api/payments route (target_service = 'payment-service'), \
         found none. This is the exact bug this test regresses on: unscoped \
         cross-repo Cypher misattributing payment-service's own route contract \
         to order-service too. Rows: {:?}",
        rows
    );

    // Cleanup.
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph after test");
    clean_pg(&pg);
    std::env::remove_var("INFIGRAPH_BACKEND");
}

// C++ namespace-qualified static-lib fixtures for the remote namespace-link
// test below (TpsBridge/tto-engine's real shape).
const TPS_BRIDGE_FORMML_CPP: &str = r#"
namespace tps
{
    void SetFormML(int entity, const char* formML)
    {
        DoWork(entity, formML);
    }
}
"#;

const TTO_ENGINE_TAX_RETURN_SERVICE_CPP: &str = r#"
void SetFormMLHandler(int entity, const char* formML) {
    tps::SetFormML(entity, formML);
}
"#;

/// Remote-mode C++ namespace-qualified cross-repo link (`link_cross_repo_namespace_calls`
/// via `link_cross_repo_namespace_calls_for_group`) against live Neo4j + Postgres.
///
/// tps-bridge (producer) defines `tps::SetFormML`; tto-engine (consumer) calls it
/// with no local definition — the same TpsBridge/tto-engine pair the feature's
/// own commit message names. On the SHARED Neo4j graph, every repo's `Infigraph`
/// handle is the SAME connection (`Neo4jBackend::connect_from_env()` reads only
/// global env vars, no per-repo parameter) — so without positive namespace
/// scoping on both (a) the caller's own EXTERNAL_CALL scan and (b) each
/// sibling's qualified-suffix scan, tps-bridge's own repo would spuriously
/// "match itself" during its own EXTERNAL_CALL scan (bug 1) and/or the
/// suffix-match query would return hits from every repo in the shared
/// instance, not just the intended sibling (bug 2) — either way tripping the
/// "0 or 2+ matches, don't guess" guard and silently producing ZERO edges
/// where local (separate-Kuzu-per-repo) mode produces exactly one. This test
/// asserts the fixed, correct behavior: exactly one CALLS_SERVICE{protocol:
/// 'static_lib'} edge, matching local mode's `namespace_link.rs` unit tests.
///
/// `#[ignore]`: needs live containers (see module header). Run with
/// `--ignored --test-threads=1` alongside the other remote tests.
#[test]
#[ignore]
fn test_group_build_links_namespace_cross_repo_call_neo4j_postgres() {
    let group_name = "remote-namespace-xsvc-test-group";

    std::env::set_var("INFIGRAPH_BACKEND", "neo4j");

    let pg = connect_pg();
    clean_pg(&pg);
    let neo = connect_neo4j();
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph before test");

    let producer_dir = make_repo(&[(
        "Src/High/HAPI/FormML/zhaSetFormML.cpp",
        TPS_BRIDGE_FORMML_CPP,
    )]);
    let consumer_dir = make_repo(&[(
        "Src/TaxApp/Server/grpc/service/TaxReturnService.cpp",
        TTO_ENGINE_TAX_RETURN_SERVICE_CPP,
    )]);

    pg.upsert_repo("tps-bridge", &repo_entry("tps-bridge", producer_dir.path()))
        .expect("seed tps-bridge repo");
    pg.upsert_repo("tto-engine", &repo_entry("tto-engine", consumer_dir.path()))
        .expect("seed tto-engine repo");
    pg.create_group(group_name).expect("create group");
    pg.group_add(group_name, "tps-bridge")
        .expect("add tps-bridge to group");
    pg.group_add(group_name, "tto-engine")
        .expect("add tto-engine to group");

    let mut registry =
        Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");
    assert!(
        registry.groups.contains_key(group_name),
        "seeded namespace-link group should round-trip through Postgres"
    );

    let index_results = multi::index_group(
        &mut registry,
        group_name,
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed against live Neo4j");
    assert_eq!(index_results.len(), 2, "both repos should have indexed");

    // The actual regression target: link_cross_repo_namespace_calls_for_group,
    // called as group build's Step 3b in production (group_commands.rs /
    // tools/groups.rs), right after link_cross_service_calls.
    let linked = multi::namespace_link::link_cross_repo_namespace_calls_for_group(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_repo_namespace_calls_for_group should succeed against live Neo4j");
    assert_eq!(
        linked, 1,
        "expected exactly one cross-repo namespace-qualified CALLS_SERVICE edge \
         (tto-engine -> tps-bridge's tps::SetFormML) on the SHARED Neo4j graph; \
         a count of 0 here is the exact bug this test regresses on — see module \
         doc comment"
    );

    // The real regression assertion: a CALLS_SERVICE{protocol:'static_lib'}
    // edge from a tto-engine caller into tps-bridge's tps::SetFormML, correctly
    // scoped to exactly one edge despite both repos sharing one Neo4j instance.
    let rows = neo
        .raw_query(
            "MATCH (caller:Symbol)-[r:CALLS_SERVICE]->(target:Symbol) \
             WHERE r.protocol = 'static_lib' AND r.qualifier = 'tps' \
             RETURN caller.id, target.id, r.qualifier",
        )
        .expect("query for namespace-qualified CALLS_SERVICE edge into tps-bridge");
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one CALLS_SERVICE{{protocol:'static_lib'}} edge \
         (not zero — the bug; not more than one — a scoping fix that's too \
         narrow could instead double-count across repos). Rows: {:?}",
        rows
    );
    assert!(
        rows[0][1].ends_with("tps::SetFormML"),
        "target should be tps-bridge's tps::SetFormML symbol, got: {}",
        rows[0][1]
    );

    // Cleanup.
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph after test");
    clean_pg(&pg);
    std::env::remove_var("INFIGRAPH_BACKEND");
}

// gRPC producer/consumer fixtures for the remote gRPC test (AIF3X-331 #35).
const USER_SERVICE_PROTO: &str = "syntax = \"proto3\";\nservice UserService {\n  rpc GetUser (GetUserRequest) returns (User);\n}\n";

const API_GATEWAY_PY_CLIENT: &str = r#"from user_service_pb2_grpc import UserServiceStub

class Gateway:
    def __init__(self, channel):
        self.stub = UserServiceStub(channel)

    def fetch(self, uid):
        return self.stub.GetUser(uid)
"#;

/// Remote-mode gRPC cross-service link against live Neo4j + Postgres (#35).
///
/// user-service (producer) defines `UserService` in a .proto; api-gateway
/// (consumer) references `UserServiceStub`. On the SHARED Neo4j graph,
/// detect_cross_service_deps' gRPC client scan must namespace-scope its source
/// resolution to each repo's `{org}/{repo}` prefix (same as the HTTP scan) —
/// otherwise the stub reference in one repo's namespace could misattribute.
/// Asserts a gRPC CALLS_SERVICE edge from api-gateway into user-service, with
/// target_service correctly attributed and method 'GRPC'.
///
/// `#[ignore]`: needs live containers (see module header). Run with
/// `--ignored --test-threads=1` alongside the other remote tests.
#[test]
#[ignore]
fn test_group_build_links_grpc_cross_service_call_neo4j_postgres() {
    let group_name = "remote-grpc-xsvc-test-group";

    std::env::set_var("INFIGRAPH_BACKEND", "neo4j");

    let pg = connect_pg();
    clean_pg(&pg);
    let neo = connect_neo4j();
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph before test");

    let producer_dir = make_repo(&[("proto/user.proto", USER_SERVICE_PROTO)]);
    let consumer_dir = make_repo(&[("app/gateway.py", API_GATEWAY_PY_CLIENT)]);

    pg.upsert_repo(
        "user-service",
        &repo_entry("user-service", producer_dir.path()),
    )
    .expect("seed user-service repo");
    pg.upsert_repo(
        "api-gateway",
        &repo_entry("api-gateway", consumer_dir.path()),
    )
    .expect("seed api-gateway repo");
    pg.create_group(group_name).expect("create group");
    pg.group_add(group_name, "user-service")
        .expect("add user-service to group");
    pg.group_add(group_name, "api-gateway")
        .expect("add api-gateway to group");

    let mut registry =
        Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");
    assert!(
        registry.groups.contains_key(group_name),
        "seeded gRPC group should round-trip through Postgres"
    );

    let index_results = multi::index_group(
        &mut registry,
        group_name,
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed against live Neo4j");
    assert_eq!(index_results.len(), 2, "both repos should have indexed");

    let contract_count = multi::sync_group_contracts(
        &mut registry,
        group_name,
        infigraph_languages::bundled_registry,
    )
    .expect("sync_group_contracts should succeed");
    assert!(
        contract_count > 0,
        "expected a GrpcService contract from user-service's .proto"
    );

    let linked = multi::link_cross_service_calls(
        &registry,
        group_name,
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_service_calls should succeed");
    assert!(
        linked > 0,
        "expected a gRPC CALLS_SERVICE edge from api-gateway into user-service"
    );

    // A gRPC CALLS_SERVICE edge into user-service, method 'GRPC', correctly
    // attributed (not leaked to another repo's namespace on the shared graph).
    let rows = neo
        .raw_query(
            "MATCH (caller:Symbol)-[r:CALLS_SERVICE]->(target:Symbol) \
             WHERE r.method = 'GRPC' AND r.target_service = 'user-service' \
             RETURN caller.id, target.id, r.target_service, r.path",
        )
        .expect("query for gRPC CALLS_SERVICE edge into user-service");
    assert!(
        !rows.is_empty(),
        "expected a gRPC CALLS_SERVICE edge from an api-gateway caller into \
         user-service (target_service = 'user-service', method = 'GRPC'); the \
         namespaced source scan must resolve the UserServiceStub reference in \
         api-gateway's own namespace. Rows: {:?}",
        rows
    );

    // Cleanup.
    neo.raw_query("MATCH (n) DETACH DELETE n")
        .expect("clear neo4j graph after test");
    clean_pg(&pg);
    std::env::remove_var("INFIGRAPH_BACKEND");
}
