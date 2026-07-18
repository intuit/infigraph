//! Integration tests for PostgresMetaStore registry operations.
//!
//! Requires: `docker run -d -p 5432:5432 -e POSTGRES_USER=infigraph -e POSTGRES_PASSWORD=infigraph -e POSTGRES_DB=infigraph pgvector/pgvector:pg16`
//! Run: `DATABASE_URL="host=localhost user=infigraph password=infigraph dbname=infigraph" cargo test -p infigraph-core --features postgres --test postgres_registry -- --ignored`

#![cfg(feature = "postgres")]

use std::collections::HashMap;
use std::path::PathBuf;

use infigraph_core::meta::PostgresMetaStore;
use infigraph_core::multi::{Contract, ContractKind, Group, Registry, RepoEntry};

fn connect() -> PostgresMetaStore {
    let store =
        PostgresMetaStore::connect_from_env().expect("Postgres connection — is Docker running?");
    store.init_schema().expect("schema init");
    store
}

fn clean(store: &PostgresMetaStore) {
    store.execute_raw("DELETE FROM group_repos").ok();
    store.execute_raw("DELETE FROM groups").ok();
    store.execute_raw("DELETE FROM repos").ok();
    store.execute_raw("DELETE FROM file_hashes").ok();
    store.execute_raw("DELETE FROM sessions").ok();
}

fn sample_entry(name: &str) -> RepoEntry {
    RepoEntry {
        name: name.to_string(),
        path: PathBuf::from(format!("/tmp/repos/{name}")),
        languages: vec!["python".to_string(), "rust".to_string()],
        symbol_count: 42,
        module_count: 3,
    }
}

// ── Registry load/save round-trip ────────────────────────────────────

#[test]
#[ignore]
fn test_postgres_registry_round_trip() {
    let store = connect();
    clean(&store);

    let mut registry = Registry::default();
    registry.repos.insert("svc-a".into(), sample_entry("svc-a"));
    registry.repos.insert("svc-b".into(), sample_entry("svc-b"));
    registry.groups.insert(
        "org".into(),
        Group {
            name: "org".into(),
            repos: vec!["svc-a".into(), "svc-b".into()],
            contracts: vec![Contract {
                kind: ContractKind::HttpRoute,
                service: "svc-a".into(),
                method: "GET".into(),
                path: "/api/users".into(),
                symbol_id: "svc-a/routes.py::get_users".into(),
                file: "routes.py".into(),
            }],
        },
    );

    store.save_registry(&registry).expect("save");
    let loaded = store.load_registry().expect("load");

    assert_eq!(loaded.repos.len(), 2);
    assert!(loaded.repos.contains_key("svc-a"));
    assert!(loaded.repos.contains_key("svc-b"));

    let entry = &loaded.repos["svc-a"];
    assert_eq!(entry.symbol_count, 42);
    assert_eq!(entry.languages, vec!["python", "rust"]);

    assert_eq!(loaded.groups.len(), 1);
    let group = &loaded.groups["org"];
    assert_eq!(group.repos.len(), 2);
    assert_eq!(group.contracts.len(), 1);
    assert_eq!(group.contracts[0].method, "GET");
    assert_eq!(group.contracts[0].path, "/api/users");
}

// ── Individual operations ────────────────────────────────────────────

#[test]
#[ignore]
fn test_postgres_upsert_repo() {
    let store = connect();

    let entry = sample_entry("test-repo");
    store.upsert_repo("test-repo", &entry).expect("upsert");

    let registry = store.load_registry().expect("load");
    assert!(registry.repos.contains_key("test-repo"));
    assert_eq!(registry.repos["test-repo"].symbol_count, 42);

    // Update
    let mut updated = entry.clone();
    updated.symbol_count = 100;
    store
        .upsert_repo("test-repo", &updated)
        .expect("upsert update");

    let registry = store.load_registry().expect("load");
    assert_eq!(registry.repos["test-repo"].symbol_count, 100);
}

#[test]
#[ignore]
fn test_postgres_create_group() {
    let store = connect();

    store.create_group("test-group").expect("create");
    store.create_group("test-group").expect("idempotent create");

    let registry = store.load_registry().expect("load");
    assert!(registry.groups.contains_key("test-group"));
}

#[test]
#[ignore]
fn test_postgres_group_add_remove() {
    let store = connect();

    // Setup: repo + group must exist
    store
        .upsert_repo("gr-repo", &sample_entry("gr-repo"))
        .expect("repo");
    store.create_group("gr-test").expect("group");

    store.group_add("gr-test", "gr-repo").expect("add");

    let registry = store.load_registry().expect("load");
    let group = &registry.groups["gr-test"];
    assert!(group.repos.contains(&"gr-repo".to_string()));

    store.group_remove("gr-test", "gr-repo").expect("remove");
    let registry = store.load_registry().expect("load");
    let group = &registry.groups["gr-test"];
    assert!(!group.repos.contains(&"gr-repo".to_string()));
}

// ── File hashes ──────────────────────────────────────────────────────

#[test]
#[ignore]
fn test_postgres_file_hashes() {
    let store = connect();

    let mut hashes = HashMap::new();
    hashes.insert("src/main.py".to_string(), "abc123".to_string());
    hashes.insert("src/lib.py".to_string(), "def456".to_string());

    store
        .upsert_file_hashes("hash-repo", &hashes)
        .expect("upsert");

    let loaded = store.get_file_hashes("hash-repo").expect("load");
    assert_eq!(
        loaded.get("src/main.py").map(|s| s.as_str()),
        Some("abc123")
    );
    assert_eq!(loaded.get("src/lib.py").map(|s| s.as_str()), Some("def456"));

    // Update one
    let mut update = HashMap::new();
    update.insert("src/main.py".to_string(), "updated".to_string());
    store
        .upsert_file_hashes("hash-repo", &update)
        .expect("update");

    let loaded = store.get_file_hashes("hash-repo").expect("reload");
    assert_eq!(
        loaded.get("src/main.py").map(|s| s.as_str()),
        Some("updated")
    );

    // Delete
    store
        .delete_file_hashes("hash-repo", &["src/lib.py".to_string()])
        .expect("delete");
    let loaded = store.get_file_hashes("hash-repo").expect("after delete");
    assert!(!loaded.contains_key("src/lib.py"));
}

// ── Session operations ───────────────────────────────────────────────

#[test]
#[ignore]
fn test_postgres_session_crud() {
    use infigraph_core::graph::SessionData;

    let store = connect();

    let session = SessionData {
        id: "test-session-001".into(),
        name: "test".into(),
        summary: "Test session".into(),
        pending_tasks: "task1".into(),
        decisions: "decided X".into(),
        files_touched: "a.rs, b.rs".into(),
        constraints: "".into(),
        assumptions: "".into(),
        blockers: "".into(),
        confidence: 0.9,
        created_at: 1000,
        updated_at: 2000,
        last_accessed: 3000,
    };

    store.save_session(&session).expect("save");

    let loaded = store.load_session("test-session-001").expect("load");
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.summary, "Test session");
    assert_eq!(loaded.confidence, 0.9);

    // Update
    let mut updated = session.clone();
    updated.summary = "Updated summary".into();
    updated.updated_at = 4000;
    store.save_session(&updated).expect("update");

    let loaded = store.load_session("test-session-001").expect("reload");
    assert_eq!(loaded.unwrap().summary, "Updated summary");

    // List recent
    let recent = store.list_sessions_recent(10).expect("list");
    assert!(recent.iter().any(|s| s.id == "test-session-001"));

    // Delete
    store.delete_session("test-session-001").expect("delete");
    let loaded = store
        .load_session("test-session-001")
        .expect("after delete");
    assert!(loaded.is_none());
}

// ── Registry env-var routing ─────────────────────────────────────────

#[test]
#[ignore]
fn test_registry_load_save_postgres_mode() {
    // This test verifies Registry::load()/save() route to Postgres
    // when INFIGRAPH_BACKEND=neo4j
    let store = connect();

    // Pre-populate via direct store access
    store
        .upsert_repo("env-test-repo", &sample_entry("env-test-repo"))
        .expect("setup");
    store.create_group("env-test-group").expect("setup group");
    store
        .group_add("env-test-group", "env-test-repo")
        .expect("setup add");

    // Set env var and load via Registry::load()
    std::env::set_var("INFIGRAPH_BACKEND", "neo4j");
    let registry = Registry::load().expect("load via env");
    std::env::remove_var("INFIGRAPH_BACKEND");

    assert!(registry.repos.contains_key("env-test-repo"));
    assert!(registry.groups.contains_key("env-test-group"));
}
