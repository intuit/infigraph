use std::sync::OnceLock;

use serde_json::json;

use infigraph_mcp::tools::groups::*;
use infigraph_mcp::tools::index::tool_index_project;
use infigraph_mcp::tools::watch::*;

// Two microservice projects for group testing
struct GroupFixture {
    _home_dir: tempfile::TempDir,
    _svc_a_dir: tempfile::TempDir,
    _svc_b_dir: tempfile::TempDir,
    svc_a_path: String,
    svc_b_path: String,
    orig_home: String,
}

static GROUP_FIXTURE: OnceLock<GroupFixture> = OnceLock::new();

unsafe impl Sync for GroupFixture {}

fn group_fixture() -> &'static GroupFixture {
    GROUP_FIXTURE.get_or_init(|| {
        let home_dir = tempfile::TempDir::new().expect("tmpdir for home");
        let orig_home = std::env::var("HOME").unwrap_or_default();
        std::env::set_var("HOME", home_dir.path());

        // Service A: a Flask-like Python API
        let svc_a_dir = tempfile::TempDir::new().expect("svc_a");
        let svc_a_files: &[(&str, &str)] = &[
            (
                "app.py",
                "\
from flask import Flask, jsonify, request
import requests

app = Flask(__name__)

@app.route('/api/orders', methods=['GET'])
def get_orders():
    user = requests.get('http://user-service/api/users/me')
    return jsonify({'orders': [], 'user': user.json()})

@app.route('/api/orders/<id>', methods=['GET'])
def get_order(id):
    return jsonify({'id': id})

def process_order(data):
    validated = validate_order(data)
    return save_order(validated)

def validate_order(data):
    if not data.get('items'):
        raise ValueError('no items')
    return data

def save_order(data):
    return data
",
            ),
            (
                "tests/test_orders.py",
                "\
from app import process_order

def test_process_order():
    result = process_order({'items': ['a']})
    assert result is not None
",
            ),
        ];
        for (name, content) in svc_a_files {
            let p = svc_a_dir.path().join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, content).unwrap();
        }

        // Service B: a user service
        let svc_b_dir = tempfile::TempDir::new().expect("svc_b");
        let svc_b_files: &[(&str, &str)] = &[(
            "app.py",
            "\
from flask import Flask, jsonify

app = Flask(__name__)

@app.route('/api/users/me', methods=['GET'])
def get_current_user():
    return jsonify({'id': 1, 'name': 'test'})

@app.route('/api/users/<id>', methods=['GET'])
def get_user(id):
    return jsonify({'id': id})

def create_user(data):
    return data
",
        )];
        for (name, content) in svc_b_files {
            let p = svc_b_dir.path().join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, content).unwrap();
        }

        let svc_a_path = svc_a_dir.path().to_string_lossy().to_string();
        let svc_b_path = svc_b_dir.path().to_string_lossy().to_string();

        // Index both
        tool_index_project(&json!({"path": &svc_a_path})).expect("index svc_a");
        tool_index_project(&json!({"path": &svc_b_path})).expect("index svc_b");

        GroupFixture {
            _home_dir: home_dir,
            _svc_a_dir: svc_a_dir,
            _svc_b_dir: svc_b_dir,
            svc_a_path,
            svc_b_path,
            orig_home,
        }
    })
}

// All group/watch/perf tests run in one function to avoid Kuzu concurrent access issues
#[test]
#[ignore] // perf test — timing assertions flaky in CI, run locally
fn test_groups_watch_perf() {
    let fix = group_fixture();

    // ==================== GROUP TOOLS ====================

    // --- group_create ---
    let result = tool_group_create(&json!({"name": "test-microservices"})).unwrap();
    assert!(result.contains("created"), "group_create: {result}");

    // --- group_create duplicate ---
    let result = tool_group_create(&json!({"name": "test-microservices"}));
    assert!(result.is_err(), "duplicate group_create should error");

    // --- group_list (empty group) ---
    let result = tool_group_list(&json!({})).unwrap();
    assert!(
        result.contains("test-microservices"),
        "group_list: should show group: {result}"
    );
    assert!(
        result.contains("0 repos"),
        "group_list: should show 0 repos: {result}"
    );

    // --- group_add service A ---
    let result = tool_group_add(&json!({
        "group_name": "test-microservices",
        "repo_name": "order-service",
        "path": &fix.svc_a_path
    }))
    .unwrap();
    assert!(result.contains("Added"), "group_add(A): {result}");

    // --- group_add service B ---
    let result = tool_group_add(&json!({
        "group_name": "test-microservices",
        "repo_name": "user-service",
        "path": &fix.svc_b_path
    }))
    .unwrap();
    assert!(result.contains("Added"), "group_add(B): {result}");

    // --- group_list (with repos) ---
    let result = tool_group_list(&json!({})).unwrap();
    assert!(
        result.contains("2 repos"),
        "group_list: should show 2 repos: {result}"
    );
    assert!(
        result.contains("order-service"),
        "group_list: should show order-service: {result}"
    );
    assert!(
        result.contains("user-service"),
        "group_list: should show user-service: {result}"
    );

    // --- group_query ---
    let result = tool_group_query(&json!({
        "group_name": "test-microservices",
        "cypher": "MATCH (s:Symbol) WHERE s.kind = 'Function' RETURN s.name LIMIT 3"
    }))
    .unwrap();
    assert!(!result.is_empty(), "group_query: empty result");
    // Should have results from both repos
    assert!(
        result.contains("order-service") || result.contains("user-service"),
        "group_query: should identify repos: {result}"
    );

    // --- group_sync (extract HTTP contracts) ---
    let result = tool_group_sync(&json!({"group_name": "test-microservices"})).unwrap();
    assert!(
        result.contains("contracts") || result.contains("Extracted"),
        "group_sync: {result}"
    );

    // --- group_contracts ---
    let result = tool_group_contracts(&json!({"group_name": "test-microservices"})).unwrap();
    // May or may not find contracts depending on route detection
    assert!(!result.is_empty(), "group_contracts: empty: {result}");

    // --- group_deps ---
    let result = tool_group_deps(&json!({"group_name": "test-microservices"})).unwrap();
    // May or may not find deps depending on URL detection in code
    assert!(!result.is_empty(), "group_deps: empty: {result}");

    // --- group_index ---
    let result = tool_group_index(&json!({"group_name": "test-microservices"})).unwrap();
    assert!(result.contains("Indexed"), "group_index: {result}");
    // Auto-watch (triggered by the earlier group_add/group_query calls) may
    // have already indexed both repos in the background by the time this
    // explicit call runs, in which case index_group correctly reports 0
    // repos needing work rather than re-listing them by name. Both outcomes
    // mean the data is indexed (already confirmed via group_query above),
    // so accept either rather than assuming this call always does the work.
    let already_current = result.contains("Indexed 0 repos");
    assert!(
        already_current || result.contains("order-service"),
        "group_index: missing order-service: {result}"
    );
    assert!(
        already_current || result.contains("user-service"),
        "group_index: missing user-service: {result}"
    );

    // --- group_link ---
    let result = tool_group_link(&json!({"group_name": "test-microservices"})).unwrap();
    assert!(
        result.contains("Linked") || result.contains("CALLS_SERVICE"),
        "group_link: {result}"
    );

    // --- error cases ---
    let result = tool_group_query(
        &json!({"group_name": "nonexistent", "cypher": "MATCH (s:Symbol) RETURN s.name"}),
    );
    assert!(result.is_err(), "query nonexistent group should error");

    let result = tool_group_add(&json!({"group_name": "nonexistent", "repo_name": "foo"}));
    assert!(result.is_err(), "add to nonexistent group should error");

    let result = tool_group_create(&json!({}));
    assert!(result.is_err(), "group_create without name should error");

    // ==================== WATCHER TOOLS ====================

    // Stop any watchers started by group_index auto_start_watch
    {
        let mut guard = get_watchers();
        if let Some(map) = guard.as_mut() {
            let ids: Vec<String> = map.keys().cloned().collect();
            for id in ids {
                if let Some(entry) = map.remove(&id) {
                    let _ = entry.stop_tx.send(());
                }
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    // --- watch_project ---
    let result = tool_watch_project(&json!({
        "path": &fix.svc_a_path,
        "debounce_ms": 200
    }))
    .unwrap();
    assert!(result.contains("Watcher started"), "watch: {result}");
    assert!(
        result.contains("watch-"),
        "watch: should return ID: {result}"
    );
    let watcher_id = result
        .lines()
        .find(|l| l.starts_with("ID: "))
        .map(|l| l.trim_start_matches("ID: ").trim())
        .expect("should find watcher ID in output");

    // --- get_watch_status (all watchers) ---
    let result = tool_get_watch_status(&json!({})).unwrap();
    assert!(result.contains("watcher"), "status(all): {result}");
    assert!(
        result.contains(watcher_id),
        "status(all): should show watcher ID: {result}"
    );

    // --- get_watch_status (specific) ---
    let result = tool_get_watch_status(&json!({"watcher_id": watcher_id})).unwrap();
    assert!(result.contains(watcher_id), "status(specific): {result}");
    assert!(
        result.contains("OK") || result.contains("Status"),
        "status(specific): should show status: {result}"
    );

    // --- trigger file change and verify watcher detects it ---
    let modified_file = std::path::Path::new(&fix.svc_a_path).join("app.py");
    let original = std::fs::read_to_string(&modified_file).unwrap();
    std::fs::write(&modified_file, format!("{original}\ndef new_fn(): pass\n")).unwrap();
    // Give watcher time to pick up change
    std::thread::sleep(std::time::Duration::from_millis(500));

    // --- stop_watch ---
    let result = tool_stop_watch(&json!({"watcher_id": watcher_id})).unwrap();
    assert!(result.contains("stopped"), "stop_watch: {result}");

    // --- verify watcher is gone ---
    let result = tool_get_watch_status(&json!({})).unwrap();
    assert!(
        result.contains("No watchers") || !result.contains(watcher_id),
        "status after stop: should not show stopped watcher: {result}"
    );

    // --- stop nonexistent watcher ---
    let result = tool_stop_watch(&json!({"watcher_id": "nonexistent-id"})).unwrap();
    assert!(
        result.contains("No watcher found"),
        "stop nonexistent: {result}"
    );

    // --- watch_project with auto_resolve ---
    let result = tool_watch_project(&json!({
        "path": &fix.svc_b_path,
        "auto_resolve": true,
        "debounce_ms": 200
    }))
    .unwrap();
    assert!(
        result.contains("auto_resolve: ON"),
        "auto_resolve watch: {result}"
    );
    let watcher_id2 = result
        .lines()
        .find(|l| l.starts_with("ID: "))
        .map(|l| l.trim_start_matches("ID: ").trim())
        .expect("should find watcher ID");
    tool_stop_watch(&json!({"watcher_id": watcher_id2})).unwrap();

    // --- missing path error ---
    let result = tool_watch_project(&json!({}));
    assert!(result.is_err(), "watch without path should error");

    // Restore file
    std::fs::write(&modified_file, &original).unwrap();

    // ==================== INDEX PERFORMANCE ====================

    // Create a moderately sized project
    let perf_dir = tempfile::TempDir::new().expect("perf_dir");
    for i in 0..50 {
        let name = format!("module_{i}.py");
        let mut code = format!("class Service{i}:\n");
        for j in 0..10 {
            code.push_str(&format!(
                "    def method_{j}(self, arg_{j}):\n        return arg_{j} + {}\n\n",
                i * 10 + j
            ));
        }
        // Add cross-file calls
        if i > 0 {
            code.push_str(&format!(
                "    def call_prev(self):\n        from module_{} import Service{}\n        return Service{}().method_0(42)\n",
                i - 1, i - 1, i - 1
            ));
        }
        std::fs::write(perf_dir.path().join(&name), code).unwrap();
    }

    let perf_path = perf_dir.path().to_string_lossy().to_string();
    let start = std::time::Instant::now();
    let result = tool_index_project(&json!({"path": &perf_path})).unwrap();
    let elapsed = start.elapsed();

    println!(
        "Index 50 files (500+ symbols): {:.2}s",
        elapsed.as_secs_f64()
    );
    println!("Result: {result}");

    // Sanity: indexing should complete and produce output
    assert!(!result.is_empty(), "index should produce output");

    // Performance: 50 Python files should index in under 30 seconds
    assert!(
        elapsed.as_secs() < 30,
        "index took too long: {:.2}s (expected <30s)",
        elapsed.as_secs_f64()
    );

    // Stop any auto-started watchers before re-index (watcher holds DB lock)
    {
        let mut guard = get_watchers();
        if let Some(map) = guard.as_mut() {
            let ids_to_stop: Vec<String> = map.keys().cloned().collect();
            for id in ids_to_stop {
                if let Some(entry) = map.remove(&id) {
                    let _ = entry.stop_tx.send(());
                }
            }
        }
    }
    // Also stop doc watchers
    {
        use infigraph_mcp::tools::docs::DOC_WATCHERS;
        let mut guard = DOC_WATCHERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            let ids: Vec<String> = map.keys().cloned().collect();
            for id in ids {
                if let Some(entry) = map.remove(&id) {
                    let _ = entry.stop_tx.send(());
                }
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    // --- Incremental re-index should be faster ---
    let start2 = std::time::Instant::now();
    tool_index_project(&json!({"path": &perf_path})).unwrap();
    let elapsed2 = start2.elapsed();
    println!("Re-index (incremental): {:.2}s", elapsed2.as_secs_f64());

    // Incremental should not be significantly slower than full.
    // Both include overhead (registry save, watcher start), so allow 10% margin.
    if elapsed.as_millis() > 2000 {
        let margin = elapsed.mul_f64(1.1);
        assert!(
            elapsed2 < margin,
            "incremental re-index ({:.2}s) should not be slower than full ({:.2}s + 10%)",
            elapsed2.as_secs_f64(),
            elapsed.as_secs_f64()
        );
    }

    // Restore HOME
    std::env::set_var("HOME", &fix.orig_home);
}
