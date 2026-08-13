// Coverage for `infigraph group build`'s 5 steps (index_group, sync_group_contracts,
// link_cross_service_calls, build_combined_graph, doc indexing), local and remote.
//
// Local tests run for real in normal CI. Remote (`#[ignore]`) tests require live
// Neo4j + Postgres (see remote_cross_service.rs's connect_neo4j/connect_pg) and are
// not exercised by a plain `cargo test` run — they document the same invariants for
// the Neo4j-backed path but need `cargo test -- --ignored` against real containers
// to actually prove anything.
//
// The crown-jewel case (test_local_incremental_build_links_unchanged_caller_to_newly_changed_producer_route)
// is the regression guard for a fix that was almost shipped and would have broken this:
// skipping "unchanged" repos in Steps 2/3 the same way Step 5's doc-index skip (592b03d)
// skips unchanged repos. Steps 2/3 are cross-repo by construction — group.contracts is a
// full replace (sync_group_contracts), and route_lookup is built from ALL contracts, so an
// unchanged caller must still be rescanned whenever any OTHER repo's routes change.

use infigraph_core::multi::{self, Group, Registry, RepoEntry};
use std::collections::HashMap;
use std::path::Path;

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

// A second route added to payment-service AFTER the first build, to prove an
// unchanged caller still gets linked to it on a later incremental build.
const PAYMENT_SERVICE_RS_HANDLERS_V2: &str = r#"
use actix_web::{post, web, HttpResponse, Responder};

pub struct CreatePaymentRequest {
    pub order_id: String,
    pub amount: f64,
}

#[post("/api/payments")]
pub async fn create_payment(body: web::Json<CreatePaymentRequest>) -> impl Responder {
    HttpResponse::Created().json(body.amount)
}

#[post("/api/refunds")]
pub async fn create_refund(body: web::Json<CreatePaymentRequest>) -> impl Responder {
    HttpResponse::Created().json(body.amount)
}
"#;

const ORDER_SERVICE_TS_CLIENT_V2_CALLS_REFUNDS: &str = r#"
const PAYMENT_SERVICE_URL = "http://payment-service:8080";
const PAYMENT_PATH = "/api/payments";
const REFUND_PATH = "/api/refunds";

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

  async requestRefund(orderId: string): Promise<PaymentResult> {
    const response = await fetch(`${PAYMENT_SERVICE_URL}${REFUND_PATH}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ orderId }),
    });
    const data = await response.json();
    return { success: true, paymentId: data.id };
  }
}
"#;

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

// index_group's "unchanged since last index" skip compares git HEAD commits
// (last_indexed_commit vs current). A plain tempdir with no git repo makes
// git_head_commit return None every time, and (None, None) is NOT treated as
// unchanged — so a real git repo is required for any test that exercises the
// skip path.
fn git_commit_all(dir: &Path, message: &str) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git")
    };
    run(&["init", "-q"]);
    run(&["add", "."]);
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=test",
            "commit",
            "-q",
            "-m",
            message,
            "--allow-empty",
        ])
        .current_dir(dir)
        .output()
        .expect("run git commit");
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

const PUBLISHER_SVC_PYPROJECT: &str = r#"[project]
name = "my-shared-lib"
version = "1.0.0"
dependencies = []
"#;

const PUBLISHER_SVC_MAIN_PY: &str = r#"def publish():
    return "published"
"#;

const CONSUMER_SVC_PYPROJECT: &str = r#"[project]
name = "consumer-app"
version = "1.0.0"
dependencies = ["my-shared-lib>=1.0.0", "requests>=2.0"]
"#;

const CONSUMER_SVC_MAIN_PY: &str = r#"def consume():
    return "consumed"
"#;

fn shared_package_group(publisher_dir: &Path, consumer_dir: &Path) -> Registry {
    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry.repos.insert(
        "publisher-svc".to_string(),
        repo_entry("publisher-svc", publisher_dir),
    );
    registry.repos.insert(
        "consumer-svc".to_string(),
        repo_entry("consumer-svc", consumer_dir),
    );
    registry.groups.insert(
        "shared-pkg-test-group".to_string(),
        Group {
            name: "shared-pkg-test-group".to_string(),
            org: String::new(),
            repos: vec!["publisher-svc".to_string(), "consumer-svc".to_string()],
            contracts: vec![],
        },
    );
    registry
}

fn local_group(order_dir: &Path, payment_dir: &Path) -> Registry {
    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry.repos.insert(
        "order-service".to_string(),
        repo_entry("order-service", order_dir),
    );
    registry.repos.insert(
        "payment-service".to_string(),
        repo_entry("payment-service", payment_dir),
    );
    registry.groups.insert(
        "local-xsvc-test-group".to_string(),
        Group {
            name: "local-xsvc-test-group".to_string(),
            org: String::new(),
            repos: vec!["order-service".to_string(), "payment-service".to_string()],
            contracts: vec![],
        },
    );
    registry
}

// ---------------------------------------------------------------------------
// LOCAL MODE — runs in real CI, no external services.
// ---------------------------------------------------------------------------

/// Step 1: index_group indexes both repos and returns per-repo file counts.
#[test]
fn test_local_step1_index_group_indexes_all_repos() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    let results = multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed locally");

    assert_eq!(results.len(), 2, "both repos should have indexed");
    for (repo_name, indexed_files, _total) in &results {
        assert!(
            *indexed_files > 0,
            "repo '{repo_name}' should index at least one file"
        );
    }
}

/// Step 1 (incremental): a repo whose HEAD commit hasn't changed since the last
/// index is skipped and absent from `results` — the exact signal Step 5's gate
/// (commit 592b03d) and any future Step 2/3 gate would key off of.
#[test]
fn test_local_step1_skips_unchanged_repo_on_second_run() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    git_commit_all(order_dir.path(), "init order-service");
    git_commit_all(payment_dir.path(), "init payment-service");
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    let first = multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        false,
        infigraph_languages::bundled_registry,
    )
    .expect("first index_group run should succeed");
    assert_eq!(
        first.len(),
        2,
        "first run has no history — both repos index"
    );

    let second = multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        false,
        infigraph_languages::bundled_registry,
    )
    .expect("second index_group run should succeed");
    assert!(
        second.is_empty(),
        "neither repo's commit changed since the first run — both should be skipped; got: {second:?}"
    );
}

/// Step 2: sync_group_contracts extracts payment-service's route as a contract.
#[test]
fn test_local_step2_sync_group_contracts_extracts_route() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed");

    let count = multi::sync_group_contracts(
        &mut registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("sync_group_contracts should succeed");

    assert!(
        count > 0,
        "expected at least one HTTP route contract from payment-service"
    );
}

/// Step 3, crown jewel: order-service calls payment-service's /api/payments route.
/// This is the base case the "skip unchanged repos in Steps 2/3" idea was checked
/// against — and the second half of this file is the actual regression guard.
#[test]
fn test_local_step3_links_cross_service_call() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed");
    multi::sync_group_contracts(
        &mut registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("sync_group_contracts should succeed");

    let linked = multi::link_cross_service_calls(
        &registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_service_calls should succeed");

    assert!(
        linked > 0,
        "expected at least one CALLS_SERVICE edge from order-service into payment-service"
    );
}

const TPS_BRIDGE_CPP: &str = r#"
namespace tps
{
    void SetFormML(int entity, const char* formML)
    {
        DoWork(entity, formML);
    }
}
"#;

const TTO_ENGINE_CPP: &str = r#"
void SetFormMLHandler(int entity, const char* formML) {
    tps::SetFormML(entity, formML);
}
"#;

fn namespace_link_group(producer_dir: &Path, consumer_dir: &Path) -> Registry {
    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry.repos.insert(
        "tps-bridge".to_string(),
        repo_entry("tps-bridge", producer_dir),
    );
    registry.repos.insert(
        "tto-engine".to_string(),
        repo_entry("tto-engine", consumer_dir),
    );
    registry.groups.insert(
        "local-namespace-test-group".to_string(),
        Group {
            name: "local-namespace-test-group".to_string(),
            org: String::new(),
            repos: vec!["tps-bridge".to_string(), "tto-engine".to_string()],
            contracts: vec![],
        },
    );
    registry
}

/// Step 3b: tto-engine calls tps::SetFormML, a namespace-qualified C++ symbol
/// defined only in tps-bridge's own repo. Proves
/// `link_cross_repo_namespace_calls_for_group` works end-to-end through the
/// real Registry/Group machinery (index -> link), the same way
/// `test_local_step3_links_cross_service_call` above proves the HTTP
/// cross-service linker's wrapper works — not just the lower-level
/// `link_cross_repo_namespace_calls` primitive, which is covered separately
/// by `tests/namespace_link.rs`'s unit tests against raw backends.
#[test]
fn test_local_step3b_links_cross_repo_namespace_call() {
    let producer_dir = make_repo(&[("Src/High/HAPI/FormML/zhaSetFormML.cpp", TPS_BRIDGE_CPP)]);
    let consumer_dir = make_repo(&[(
        "Src/TaxApp/Server/grpc/service/TaxReturnService.cpp",
        TTO_ENGINE_CPP,
    )]);
    let mut registry = namespace_link_group(producer_dir.path(), consumer_dir.path());

    multi::index_group(
        &mut registry,
        "local-namespace-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed");

    let linked = multi::namespace_link::link_cross_repo_namespace_calls_for_group(
        &registry,
        "local-namespace-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_repo_namespace_calls_for_group should succeed");

    assert!(
        linked > 0,
        "expected at least one static_lib CALLS_SERVICE edge from tto-engine into tps-bridge"
    );
}

/// SharedPackage dependency kind: consumer-svc's manifest (pyproject.toml)
/// depends on publisher-svc's published package name. This exercises the
/// non-HTTP contract path in sync_group_contracts (publisher map + Dependency
/// node matching) and detect_cross_service_deps' manifest-driven branch —
/// verified empirically byte-for-byte identical before/after deleting the
/// dead, duplicate detect_shared_package_deps function (same inline logic
/// this test exercises was never the one that got deleted).
#[test]
fn test_local_step2_step3_shared_package_dependency() {
    let publisher_dir = make_repo(&[
        ("pyproject.toml", PUBLISHER_SVC_PYPROJECT),
        ("main.py", PUBLISHER_SVC_MAIN_PY),
    ]);
    let consumer_dir = make_repo(&[
        ("pyproject.toml", CONSUMER_SVC_PYPROJECT),
        ("main.py", CONSUMER_SVC_MAIN_PY),
    ]);
    let mut registry = shared_package_group(publisher_dir.path(), consumer_dir.path());

    multi::index_group(
        &mut registry,
        "shared-pkg-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed");

    let count = multi::sync_group_contracts(
        &mut registry,
        "shared-pkg-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("sync_group_contracts should succeed");
    assert!(
        count > 0,
        "expected at least one SharedPackage contract from publisher-svc's manifest"
    );

    let contracts = &registry
        .groups
        .get("shared-pkg-test-group")
        .unwrap()
        .contracts;
    let shared_pkg_contract = contracts
        .iter()
        .find(|c| c.kind == multi::ContractKind::SharedPackage && c.path == "my-shared-lib");
    assert!(
        shared_pkg_contract.is_some(),
        "expected a SharedPackage contract for 'my-shared-lib' published by publisher-svc; got {contracts:?}"
    );
    assert_eq!(shared_pkg_contract.unwrap().service, "publisher-svc");

    let linked = multi::link_cross_service_calls(
        &registry,
        "shared-pkg-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("link_cross_service_calls should succeed");
    assert!(
        linked > 0,
        "expected at least one CALLS_SERVICE-derived edge from consumer-svc's \
         manifest dependency on publisher-svc's shared package"
    );
}

/// THE regression guard: order-service (caller) is left completely unchanged.
/// payment-service (producer) gains a NEW route after the first build. A naive
/// "skip Steps 2/3 for repos not in Step 1's changed-set" optimization would skip
/// order-service (it didn't change) and never rescan it against the new route —
/// so the new order-service -> payment-service/api/refunds edge would never appear.
/// This test fails if that optimization is ever (re)introduced without accounting
/// for the fact that Steps 2/3 are cross-repo, not per-repo like Step 5's docs.
#[test]
fn test_local_incremental_build_links_unchanged_caller_to_newly_changed_producer_route() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT_V2_CALLS_REFUNDS)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    git_commit_all(order_dir.path(), "init order-service");
    git_commit_all(payment_dir.path(), "init payment-service");
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    // Build #1: payment-service only has /api/payments. order-service's refund
    // call has nothing to resolve to yet.
    multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("first index_group should succeed");
    multi::sync_group_contracts(
        &mut registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("first sync_group_contracts should succeed");
    multi::link_cross_service_calls(
        &registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("first link_cross_service_calls should succeed");

    // Producer (payment-service) gains a new route AND commits it. Caller
    // (order-service) is NOT touched at all — no file change, no new commit —
    // so Step 1 must genuinely skip-gate it as unchanged on the next build.
    std::fs::write(
        payment_dir.path().join("src/handlers.rs"),
        PAYMENT_SERVICE_RS_HANDLERS_V2,
    )
    .expect("update payment-service route file");
    git_commit_all(payment_dir.path(), "add /api/refunds route");

    // Build #2, incremental (full=false): only payment-service actually changed.
    let results = multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        false,
        infigraph_languages::bundled_registry,
    )
    .expect("second index_group should succeed");
    let changed_repos: Vec<&str> = results.iter().map(|(r, _, _)| r.as_str()).collect();
    assert!(
        changed_repos.contains(&"payment-service"),
        "payment-service's new commit should put it in Step 1's changed set; got {changed_repos:?}"
    );
    assert!(
        !changed_repos.contains(&"order-service"),
        "order-service's commit did not change — it must be genuinely skip-gated by \
         Step 1 for this test to guard anything; got {changed_repos:?}"
    );

    multi::sync_group_contracts(
        &mut registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("second sync_group_contracts should succeed");
    multi::link_cross_service_calls(
        &registry,
        "local-xsvc-test-group",
        infigraph_languages::bundled_registry,
    )
    .expect("second link_cross_service_calls should succeed");

    // The actual regression assertion: query order-service's own graph directly
    // for the new edge, rather than comparing linked-edge counts across builds
    // (edges persist on disk in local mode, so a raw count delta can't
    // distinguish "the refunds edge now exists" from an unrelated count change).
    let order_entry = registry.repos.get("order-service").unwrap().clone();
    let mut order_prism = infigraph_core::Infigraph::open(
        &order_entry.path,
        infigraph_core::lang::LanguageRegistry::new(),
    )
    .expect("open order-service graph");
    order_prism.init().expect("init order-service graph");
    let backend = order_prism
        .backend()
        .expect("order-service should have a backend");
    let rows = backend
        .raw_query(
            "MATCH (caller:Symbol)-[:CALLS_SERVICE]->(target:Symbol) \
             WHERE target.id CONTAINS 'refunds' RETURN caller.id, target.id",
        )
        .expect("query for refunds CALLS_SERVICE edge");
    assert!(
        !rows.is_empty(),
        "expected a CALLS_SERVICE edge from order-service into payment-service's new \
         /api/refunds route, even though order-service (the caller) was skip-gated as \
         unchanged by Step 1. If this regresses, Steps 2/3 were skip-gated per-repo like \
         Step 5 and unchanged callers are no longer rescanned against other repos' new \
         routes. Found rows: {rows:?}"
    );
}

/// Step 4: build_combined_graph merges both repos' symbols/edges (local mode only —
/// this step is a no-op in remote/Neo4j mode since the shared graph is already
/// namespaced; see group_commands.rs's `is_remote` branch).
#[test]
fn test_local_step4_build_combined_graph_merges_repos() {
    let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
    let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
    let mut registry = local_group(order_dir.path(), payment_dir.path());

    multi::index_group(
        &mut registry,
        "local-xsvc-test-group",
        true,
        infigraph_languages::bundled_registry,
    )
    .expect("index_group should succeed");

    let (symbols, _edges) =
        multi::combined::build_combined_graph(&registry, "local-xsvc-test-group")
            .expect("build_combined_graph should succeed");

    assert!(
        symbols > 0,
        "combined graph should contain symbols from both repos"
    );
}

// Step 5 (local mode) is covered separately in infigraph-docs/tests/group_build_docs.rs —
// infigraph-core has no dependency on infigraph-docs, so DocIndex/build_combined_docs
// aren't reachable from this crate's tests.

// ---------------------------------------------------------------------------
// REMOTE MODE (Neo4j + Postgres) — `#[ignore]`d. These require live containers
// (see remote_cross_service.rs's connect_neo4j/connect_pg for setup) and are not
// exercised by a plain `cargo test` run. Run with:
//   cargo test --features neo4j -- --ignored test_remote_
// against real Neo4j + Postgres to actually validate them.
// ---------------------------------------------------------------------------

#[cfg(feature = "neo4j")]
mod remote {
    use super::*;
    use infigraph_core::graph::{GraphBackend, Neo4jBackend};
    use infigraph_core::meta::PostgresMetaStore;

    fn connect_pg() -> PostgresMetaStore {
        let store = PostgresMetaStore::connect_from_env()
            .expect("Postgres connection — is Docker running?");
        store.init_schema().expect("schema init");
        store
    }

    fn connect_neo4j() -> Neo4jBackend {
        Neo4jBackend::connect_from_env().expect("Neo4j connection — is Docker running?")
    }

    fn clean_pg(pg: &PostgresMetaStore) {
        pg.execute_raw("DELETE FROM group_repos").ok();
        pg.execute_raw("DELETE FROM groups").ok();
        pg.execute_raw("DELETE FROM repos").ok();
    }

    fn setup_remote_group(order_dir: &Path, payment_dir: &Path, group_name: &str) -> Registry {
        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        let pg = connect_pg();
        clean_pg(&pg);
        let neo = connect_neo4j();
        neo.raw_query("MATCH (n) DETACH DELETE n")
            .expect("clear neo4j graph before test");

        pg.upsert_repo("order-service", &repo_entry("order-service", order_dir))
            .expect("seed order-service repo");
        pg.upsert_repo(
            "payment-service",
            &repo_entry("payment-service", payment_dir),
        )
        .expect("seed payment-service repo");
        pg.create_group(group_name).expect("create group");
        pg.group_add(group_name, "order-service")
            .expect("add order-service");
        pg.group_add(group_name, "payment-service")
            .expect("add payment-service");

        Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)")
    }

    fn teardown_remote() {
        let pg = connect_pg();
        clean_pg(&pg);
        let neo = connect_neo4j();
        let _ = neo.raw_query("MATCH (n) DETACH DELETE n");
        std::env::remove_var("INFIGRAPH_BACKEND");
    }

    /// Step 1 (remote): index_group against live Neo4j, both repos indexed.
    #[test]
    #[ignore]
    fn test_remote_step1_index_group_indexes_all_repos() {
        let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
        let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
        let mut registry = setup_remote_group(
            order_dir.path(),
            payment_dir.path(),
            "remote-xsvc-steps-group",
        );

        let results = multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("index_group should succeed against live Neo4j");

        assert_eq!(results.len(), 2, "both repos should have indexed");
        teardown_remote();
    }

    /// Step 1 (remote, incremental): unchanged repo is skipped and NOT re-sent
    /// to Neo4j on the second build.
    #[test]
    #[ignore]
    fn test_remote_step1_skips_unchanged_repo_on_second_run() {
        let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
        let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
        git_commit_all(order_dir.path(), "init order-service");
        git_commit_all(payment_dir.path(), "init payment-service");
        let mut registry = setup_remote_group(
            order_dir.path(),
            payment_dir.path(),
            "remote-xsvc-steps-group",
        );

        multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            false,
            infigraph_languages::bundled_registry,
        )
        .expect("first run should succeed");
        let second = multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            false,
            infigraph_languages::bundled_registry,
        )
        .expect("second run should succeed");

        assert!(
            second.is_empty(),
            "no repo changed — second run should skip both; got {second:?}"
        );
        teardown_remote();
    }

    /// Step 2 (remote): contract extraction against live Neo4j.
    #[test]
    #[ignore]
    fn test_remote_step2_sync_group_contracts_extracts_route() {
        let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
        let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
        let mut registry = setup_remote_group(
            order_dir.path(),
            payment_dir.path(),
            "remote-xsvc-steps-group",
        );

        multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("index_group should succeed");
        let count = multi::sync_group_contracts(
            &mut registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("sync_group_contracts should succeed");

        assert!(
            count > 0,
            "expected at least one HTTP route contract from payment-service"
        );
        teardown_remote();
    }

    /// Step 3 (remote), crown jewel: same as remote_cross_service.rs's existing
    /// test_group_build_links_cross_service_call_neo4j_postgres — kept here too so
    /// all 5 steps' remote coverage lives in one place.
    #[test]
    #[ignore]
    fn test_remote_step3_links_cross_service_call() {
        let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT)]);
        let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
        let mut registry = setup_remote_group(
            order_dir.path(),
            payment_dir.path(),
            "remote-xsvc-steps-group",
        );

        multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("index_group should succeed");
        multi::sync_group_contracts(
            &mut registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("sync_group_contracts should succeed");
        let linked = multi::link_cross_service_calls(
            &registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("link_cross_service_calls should succeed");

        assert!(linked > 0, "expected at least one CALLS_SERVICE edge");
        teardown_remote();
    }

    /// SharedPackage (remote): mirrors the local
    /// test_local_step2_step3_shared_package_dependency test above against live
    /// Neo4j + Postgres. setup_remote_group is hardcoded to
    /// order-service/payment-service repo names, so this sets up its own
    /// publisher-svc/consumer-svc pair the same way rather than editing the
    /// shared helper.
    #[test]
    #[ignore]
    fn test_remote_shared_package_dependency() {
        let publisher_dir = make_repo(&[
            ("pyproject.toml", PUBLISHER_SVC_PYPROJECT),
            ("main.py", PUBLISHER_SVC_MAIN_PY),
        ]);
        let consumer_dir = make_repo(&[
            ("pyproject.toml", CONSUMER_SVC_PYPROJECT),
            ("main.py", CONSUMER_SVC_MAIN_PY),
        ]);

        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        let pg = connect_pg();
        clean_pg(&pg);
        let neo = connect_neo4j();
        neo.raw_query("MATCH (n) DETACH DELETE n")
            .expect("clear neo4j graph before test");

        pg.upsert_repo(
            "publisher-svc",
            &repo_entry("publisher-svc", publisher_dir.path()),
        )
        .expect("seed publisher-svc repo");
        pg.upsert_repo(
            "consumer-svc",
            &repo_entry("consumer-svc", consumer_dir.path()),
        )
        .expect("seed consumer-svc repo");
        pg.create_group("remote-shared-pkg-group")
            .expect("create group");
        pg.group_add("remote-shared-pkg-group", "publisher-svc")
            .expect("add publisher-svc");
        pg.group_add("remote-shared-pkg-group", "consumer-svc")
            .expect("add consumer-svc");

        let mut registry =
            Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");

        multi::index_group(
            &mut registry,
            "remote-shared-pkg-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("index_group should succeed");
        let count = multi::sync_group_contracts(
            &mut registry,
            "remote-shared-pkg-group",
            infigraph_languages::bundled_registry,
        )
        .expect("sync_group_contracts should succeed");
        assert!(
            count > 0,
            "expected at least one SharedPackage contract from publisher-svc's manifest"
        );

        let linked = multi::link_cross_service_calls(
            &registry,
            "remote-shared-pkg-group",
            infigraph_languages::bundled_registry,
        )
        .expect("link_cross_service_calls should succeed");
        assert!(
            linked > 0,
            "expected at least one CALLS_SERVICE-derived edge from consumer-svc's \
             manifest dependency on publisher-svc's shared package"
        );

        neo.raw_query("MATCH (n) DETACH DELETE n").ok();
        clean_pg(&pg);
        std::env::remove_var("INFIGRAPH_BACKEND");
    }

    /// Step 3 (remote), THE regression guard — remote mirror of the local
    /// crown-jewel test above. Unchanged caller must still link to a producer's
    /// newly added route on an incremental remote build.
    #[test]
    #[ignore]
    fn test_remote_incremental_build_links_unchanged_caller_to_newly_changed_producer_route() {
        let order_dir = make_repo(&[("src/client.ts", ORDER_SERVICE_TS_CLIENT_V2_CALLS_REFUNDS)]);
        let payment_dir = make_repo(&[("src/handlers.rs", PAYMENT_SERVICE_RS_HANDLERS)]);
        git_commit_all(order_dir.path(), "init order-service");
        git_commit_all(payment_dir.path(), "init payment-service");
        let mut registry = setup_remote_group(
            order_dir.path(),
            payment_dir.path(),
            "remote-xsvc-steps-group",
        );

        multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("first index_group should succeed");
        multi::sync_group_contracts(
            &mut registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("first sync_group_contracts should succeed");
        multi::link_cross_service_calls(
            &registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("first link_cross_service_calls should succeed");

        std::fs::write(
            payment_dir.path().join("src/handlers.rs"),
            PAYMENT_SERVICE_RS_HANDLERS_V2,
        )
        .expect("update payment-service route file");
        git_commit_all(payment_dir.path(), "add /api/refunds route");

        let results = multi::index_group(
            &mut registry,
            "remote-xsvc-steps-group",
            false,
            infigraph_languages::bundled_registry,
        )
        .expect("second index_group should succeed");
        let changed_repos: Vec<&str> = results.iter().map(|(r, _, _)| r.as_str()).collect();
        assert!(
            changed_repos.contains(&"payment-service"),
            "payment-service's new commit should put it in Step 1's changed set; got {changed_repos:?}"
        );
        assert!(
            !changed_repos.contains(&"order-service"),
            "order-service's commit did not change — it must be genuinely skip-gated by \
             Step 1 for this test to guard anything; got {changed_repos:?}"
        );

        multi::sync_group_contracts(
            &mut registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("second sync_group_contracts should succeed");
        multi::link_cross_service_calls(
            &registry,
            "remote-xsvc-steps-group",
            infigraph_languages::bundled_registry,
        )
        .expect("second link_cross_service_calls should succeed");

        // Query order-service's own graph directly for the new edge, rather than
        // comparing linked-edge counts across builds — edges persist across
        // builds so a count delta can't distinguish "the refunds edge now
        // exists" from an unrelated count coincidence (see the identical fix
        // applied to the local-mode version of this test).
        let neo = connect_neo4j();
        let rows = neo
            .raw_query(
                "MATCH (caller:Symbol)-[:CALLS_SERVICE]->(target:Symbol) \
                 WHERE caller.id STARTS WITH 'order-service/' AND target.id CONTAINS 'refunds' \
                 RETURN caller.id, target.id",
            )
            .expect("query for refunds CALLS_SERVICE edge");
        assert!(
            !rows.is_empty(),
            "expected a CALLS_SERVICE edge from order-service into payment-service's new \
             /api/refunds route, even though order-service (the caller) was skip-gated as \
             unchanged by Step 1. Found rows: {rows:?}"
        );

        teardown_remote();
    }

    /// Step 4 (remote): a no-op — shared Neo4j graph is already namespaced, so
    /// there's nothing to "build". Documents the skip, not real work.
    #[test]
    #[ignore]
    fn test_remote_step4_is_skipped_shared_graph_already_namespaced() {
        // group_commands.rs's `is_remote` branch prints "Skipped combined graph
        // (shared Neo4j already namespaced)" and calls neither build_combined_graph
        // nor any equivalent. Nothing to assert against the graph itself — this
        // test exists so a future change that silently starts calling
        // build_combined_graph in remote mode (which assumes local per-repo Kuzu
        // DBs on disk) gets caught by someone reading this comment before it ships.
    }

    /// index_group must derive TESTED_BY edges per repo (AIF3X-331 e2e eval: the
    /// live pod always reported 0% test coverage because `group build` — the
    /// pod's only indexing path — called `prism.index()` directly and never
    /// invoked `derive_tested_by_edges` at all; only the CLI's separate
    /// single-repo `index` command did). Regression guard: fails if that call
    /// is ever removed from `index_one` again.
    #[test]
    #[ignore]
    fn test_remote_index_group_derives_tested_by_edges() {
        let svc_dir = make_repo(&[(
            "app/thing.py",
            "def do_work():\n    return 1\n\n\ndef test_do_work():\n    assert do_work() == 1\n",
        )]);
        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        let pg = connect_pg();
        clean_pg(&pg);
        let neo = connect_neo4j();
        neo.raw_query("MATCH (n) DETACH DELETE n")
            .expect("clear neo4j graph before test");

        pg.upsert_repo(
            "tested-by-svc",
            &repo_entry("tested-by-svc", svc_dir.path()),
        )
        .expect("seed tested-by-svc repo");
        pg.create_group("remote-tested-by-group")
            .expect("create group");
        pg.group_add("remote-tested-by-group", "tested-by-svc")
            .expect("add tested-by-svc");

        let mut registry =
            Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");
        multi::index_group(
            &mut registry,
            "remote-tested-by-group",
            true,
            infigraph_languages::bundled_registry,
        )
        .expect("index_group should succeed against live Neo4j");

        let rows = neo
            .raw_query(
                "MATCH (target:Symbol)<-[:TESTED_BY]-(test:Symbol) \
                 WHERE target.id CONTAINS 'do_work' AND NOT target.id CONTAINS 'test_do_work' \
                 RETURN target.id, test.id",
            )
            .expect("query for TESTED_BY edge");
        assert!(
            !rows.is_empty(),
            "expected a TESTED_BY edge from test_do_work onto do_work after `group build` \
             (index_group) -- got none. Found rows: {rows:?}"
        );

        teardown_remote();
    }

    /// Same as `test_remote_index_group_derives_tested_by_edges` but through an
    /// incremental (`full=false`) second build after a real code change, to
    /// exercise `derive_tested_by_edges`'s `Some(changed_files)` branch
    /// specifically -- the first test only ever exercises the unconditional
    /// `_` branch via `full=true`, and the two are different Cypher queries
    /// on the Neo4j backend.
    #[test]
    #[ignore]
    fn test_remote_index_group_derives_tested_by_edges_incrementally() {
        let svc_dir = make_repo(&[("app/thing.py", "def do_work():\n    return 1\n")]);
        git_commit_all(svc_dir.path(), "init tested-by-svc");
        std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
        let pg = connect_pg();
        clean_pg(&pg);
        let neo = connect_neo4j();
        neo.raw_query("MATCH (n) DETACH DELETE n")
            .expect("clear neo4j graph before test");

        pg.upsert_repo(
            "tested-by-svc-incr",
            &repo_entry("tested-by-svc-incr", svc_dir.path()),
        )
        .expect("seed tested-by-svc-incr repo");
        pg.create_group("remote-tested-by-incr-group")
            .expect("create group");
        pg.group_add("remote-tested-by-incr-group", "tested-by-svc-incr")
            .expect("add tested-by-svc-incr");

        let mut registry =
            Registry::load().expect("load registry via Postgres (INFIGRAPH_BACKEND=neo4j)");
        multi::index_group(
            &mut registry,
            "remote-tested-by-incr-group",
            false,
            infigraph_languages::bundled_registry,
        )
        .expect("first (full) build should succeed");

        // Add a test for the already-indexed function, then rebuild
        // incrementally -- only this changed file should be re-scanned, and
        // derive_tested_by_edges must still pick up the new TESTED_BY edge
        // via its scoped `changed_files` branch, not just a full rebuild.
        std::fs::write(
            svc_dir.path().join("app/test_thing.py"),
            "from app.thing import do_work\n\n\ndef test_do_work():\n    assert do_work() == 1\n",
        )
        .expect("write new test file");
        git_commit_all(svc_dir.path(), "add test for do_work");

        multi::index_group(
            &mut registry,
            "remote-tested-by-incr-group",
            false,
            infigraph_languages::bundled_registry,
        )
        .expect("incremental build should succeed");

        let rows = neo
            .raw_query(
                "MATCH (target:Symbol)<-[:TESTED_BY]-(test:Symbol) \
                 WHERE target.id CONTAINS 'do_work' AND NOT target.id CONTAINS 'test_do_work' \
                 RETURN target.id, test.id",
            )
            .expect("query for TESTED_BY edge");
        assert!(
            !rows.is_empty(),
            "expected a TESTED_BY edge from test_do_work onto do_work after an INCREMENTAL \
             `group build` -- got none. Found rows: {rows:?}"
        );

        teardown_remote();
    }

    // Step 5 (remote) — per-repo DocIndex.index() loop gated by Step 1's `results`
    // (592b03d) — is covered in infigraph-docs/tests/group_build_docs.rs since
    // infigraph-core has no dependency on infigraph-docs/DocIndex.
}
