use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use infigraph_core::lang::{LanguagePack, LanguageRegistry};
use infigraph_core::multi::combined::{
    build_combined_graph, combined_graph_path, combined_query, has_combined_graph,
};
use infigraph_core::multi::{Group, Registry, RepoEntry};
use infigraph_core::Infigraph;

// Tests set HOME env var which is process-global, so must run sequentially.
static COMBINED_LOCK: Mutex<()> = Mutex::new(());

const PYTHON_ENTITIES: &str = r#"
(module
  (function_definition
    name: (identifier) @func.name
    body: (block
      (expression_statement
        (string) @func.docstring)?)) @func.def)

(module
  (decorated_definition
    (decorator) @func.decorator
    definition: (function_definition
      name: (identifier) @func.name
      body: (block
        (expression_statement
          (string) @func.docstring)?)) @func.def))

(class_definition
  name: (identifier) @class.name
  body: (block
    (expression_statement
      (string) @class.docstring)?)) @class.def

(class_definition
  body: (block
    (function_definition
      name: (identifier) @method.name
      body: (block
        (expression_statement
          (string) @method.docstring)?)) @method.def))

(class_definition
  body: (block
    (decorated_definition
      (decorator) @method.decorator
      definition: (function_definition
        name: (identifier) @method.name
        body: (block
          (expression_statement
            (string) @method.docstring)?)) @method.def)))

(module
  (function_definition
    name: (identifier) @test.name
    (#match? @test.name "^test_")
    body: (block
      (expression_statement
        (string) @test.docstring)?)) @test.def)

(module
  (expression_statement
    (assignment
      left: (identifier) @var.name)) @var.def)
"#;

const PYTHON_RELATIONS: &str = r#"
(call
  function: (identifier) @call.func) @call.site

(call
  function: (attribute
    object: (_) @call.receiver
    attribute: (identifier) @call.func)) @call.site

(import_statement
  name: (dotted_name) @import.module)

(import_from_statement
  module_name: (dotted_name) @import.module)

(class_definition
  name: (identifier) @inherit.child
  superclasses: (argument_list
    (identifier) @inherit.parent))
"#;

fn python_registry() -> LanguageRegistry {
    let grammar = tree_sitter_python::LANGUAGE.into();
    let mut reg = LanguageRegistry::new();
    reg.register(
        LanguagePack::new(
            "python",
            vec![".py"],
            grammar,
            PYTHON_ENTITIES,
            PYTHON_RELATIONS,
        )
        .unwrap(),
    );
    reg
}

fn make_repo(dir: &tempfile::TempDir, files: &[(&str, &str)]) -> PathBuf {
    for (name, content) in files {
        let p = dir.path().join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
    dir.path().to_path_buf()
}

fn index_repo(path: &std::path::Path) -> Infigraph {
    let reg = python_registry();
    let mut ig = Infigraph::open(path, reg).unwrap();
    ig.init().unwrap();
    ig.index().unwrap();
    ig
}

fn setup_two_repo_group(
    home: &tempfile::TempDir,
) -> (Registry, tempfile::TempDir, tempfile::TempDir) {
    std::env::set_var("HOME", home.path());

    let dir_a = tempfile::TempDir::new().unwrap();
    let path_a = make_repo(
        &dir_a,
        &[
            (
                "src/api.py",
                "\
from src.models import User

def get_user(user_id):
    return User(user_id)

def list_users():
    return [get_user(1), get_user(2)]
",
            ),
            (
                "src/models.py",
                "\
class User:
    def __init__(self, uid):
        self.uid = uid
    def display(self):
        return f'User({self.uid})'
",
            ),
        ],
    );

    let dir_b = tempfile::TempDir::new().unwrap();
    let path_b = make_repo(
        &dir_b,
        &[(
            "app.py",
            "\
def serve():
    return handle_request()

def handle_request():
    return 'ok'

def health_check():
    return 'healthy'
",
        )],
    );

    let ig_a = index_repo(&path_a);
    let ig_b = index_repo(&path_b);

    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };

    registry.repos.insert(
        "svc-a".to_string(),
        RepoEntry {
            name: "svc-a".to_string(),
            path: path_a,
            languages: vec!["Python".to_string()],
            symbol_count: ig_a.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_a.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );
    registry.repos.insert(
        "svc-b".to_string(),
        RepoEntry {
            name: "svc-b".to_string(),
            path: path_b,
            languages: vec!["Python".to_string()],
            symbol_count: ig_b.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_b.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );

    registry.groups.insert(
        "test-platform".to_string(),
        Group {
            name: "test-platform".to_string(),
            org: String::new(),
            repos: vec!["svc-a".to_string(), "svc-b".to_string()],
            contracts: vec![],
        },
    );

    (registry, dir_a, dir_b)
}

#[test]
fn test_build_combined_graph_merges_symbols() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    let (symbols, edges) = build_combined_graph(&registry, "test-platform").unwrap();
    assert!(symbols > 0, "combined graph should have symbols, got 0");
    assert!(edges > 0, "combined graph should have edges, got 0");

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_has_combined_graph_true_after_build() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    assert!(
        !has_combined_graph("test-platform"),
        "should not exist before build"
    );

    build_combined_graph(&registry, "test-platform").unwrap();

    assert!(
        has_combined_graph("test-platform"),
        "should exist after build"
    );

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_query_returns_symbols_from_both_repos() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    build_combined_graph(&registry, "test-platform").unwrap();

    let rows = combined_query(
        "test-platform",
        "MATCH (s:Symbol) WHERE s.kind = 'Function' RETURN s.name",
    )
    .unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();

    // svc-a functions
    assert!(
        names.contains(&"get_user"),
        "should contain svc-a function get_user, got: {:?}",
        names
    );
    // svc-b functions
    assert!(
        names.contains(&"serve"),
        "should contain svc-b function serve, got: {:?}",
        names
    );
    assert!(
        names.contains(&"handle_request"),
        "should contain svc-b function handle_request, got: {:?}",
        names
    );

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_query_cross_repo_edges() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    build_combined_graph(&registry, "test-platform").unwrap();

    // Verify CALLS edges exist within each repo
    let calls = combined_query(
        "test-platform",
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.name, b.name",
    )
    .unwrap();
    assert!(!calls.is_empty(), "combined graph should have CALLS edges");

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_graph_rebuild_replaces_old() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    let (sym1, edge1) = build_combined_graph(&registry, "test-platform").unwrap();
    let (sym2, edge2) = build_combined_graph(&registry, "test-platform").unwrap();

    assert_eq!(sym1, sym2, "rebuild should produce same symbol count");
    assert_eq!(edge1, edge2, "rebuild should produce same edge count");

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_graph_path_under_home() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    let path = combined_graph_path("my-group").unwrap();
    assert!(
        path.starts_with(home.path()),
        "combined graph path should be under HOME"
    );
    assert!(
        path.to_string_lossy().contains("my-group"),
        "path should contain group name"
    );

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_query_before_build_errors() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    let result = combined_query("nonexistent-group", "MATCH (s:Symbol) RETURN s.name");
    assert!(result.is_err(), "query before build should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found") || err.contains("Combined graph"),
        "error should mention missing graph: {}",
        err
    );

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_build_combined_graph_unknown_group_errors() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    let registry = Registry::default();
    let result = build_combined_graph(&registry, "no-such-group");
    assert!(result.is_err(), "unknown group should error");

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_graph_contains_modules() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    build_combined_graph(&registry, "test-platform").unwrap();

    let modules = combined_query("test-platform", "MATCH (m:Module) RETURN m.name").unwrap();
    assert!(!modules.is_empty(), "combined graph should contain modules");

    std::env::set_var("HOME", &orig_home);
}

#[test]
fn test_combined_graph_contains_files() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    let (registry, _dir_a, _dir_b) = setup_two_repo_group(&home);

    build_combined_graph(&registry, "test-platform").unwrap();

    let files = combined_query("test-platform", "MATCH (f:File) RETURN f.path").unwrap();
    assert!(
        files.len() >= 3,
        "combined graph should have files from both repos (api.py, models.py, app.py), got {}",
        files.len()
    );

    std::env::set_var("HOME", &orig_home);
}

/// Fix 0 regression: same-named classes in different modules must NOT
/// create false cross-repo INHERITS or CALLS edges.
#[test]
fn test_combined_graph_no_false_cross_repo_edges() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    let dir_a = tempfile::TempDir::new().unwrap();
    let path_a = make_repo(
        &dir_a,
        &[(
            "config.py",
            "\
class Settings:
    def __init__(self):
        self.debug = True

def get_settings():
    return Settings()
",
        )],
    );

    let dir_b = tempfile::TempDir::new().unwrap();
    let path_b = make_repo(
        &dir_b,
        &[(
            "app_config.py",
            "\
class Settings:
    def __init__(self):
        self.port = 8080

def get_settings():
    return Settings()
",
        )],
    );

    let ig_a = index_repo(&path_a);
    let ig_b = index_repo(&path_b);

    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry.repos.insert(
        "svc-a".to_string(),
        RepoEntry {
            name: "svc-a".to_string(),
            path: path_a,
            languages: vec!["Python".to_string()],
            symbol_count: ig_a.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_a.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );
    registry.repos.insert(
        "svc-b".to_string(),
        RepoEntry {
            name: "svc-b".to_string(),
            path: path_b,
            languages: vec!["Python".to_string()],
            symbol_count: ig_b.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_b.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );
    registry.groups.insert(
        "test-false-match".to_string(),
        Group {
            name: "test-false-match".to_string(),
            org: String::new(),
            repos: vec!["svc-a".to_string(), "svc-b".to_string()],
            contracts: vec![],
        },
    );

    build_combined_graph(&registry, "test-false-match").unwrap();

    let cross_inherits = combined_query(
        "test-false-match",
        "MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) \
         WHERE a.id STARTS WITH '[svc-a]' AND b.id STARTS WITH '[svc-b]' \
         RETURN a.id, b.id",
    )
    .unwrap();
    assert!(
        cross_inherits.is_empty(),
        "same-named Settings in different modules should NOT create cross-repo INHERITS, got {} edges",
        cross_inherits.len()
    );

    let cross_calls = combined_query(
        "test-false-match",
        "MATCH (a:Symbol)-[:CALLS]->(b:Symbol) \
         WHERE a.id STARTS WITH '[svc-a]' AND b.id STARTS WITH '[svc-b]' \
         RETURN a.id, b.id",
    )
    .unwrap();
    assert!(
        cross_calls.is_empty(),
        "same-named get_settings in different modules should NOT create cross-repo CALLS, got {} edges",
        cross_calls.len()
    );

    std::env::set_var("HOME", &orig_home);
}

/// Fix 0 positive case: when repo-A has class Child(BaseHandler) and repo-B
/// has the same base.py::BaseHandler, the cross-repo INHERITS bridge should fire.
#[test]
fn test_combined_graph_real_inherits_preserved() {
    let _guard = COMBINED_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let orig_home = std::env::var("HOME").unwrap_or_default();
    std::env::set_var("HOME", home.path());

    // repo-A: base.py defines BaseHandler, child.py inherits from it
    let dir_a = tempfile::TempDir::new().unwrap();
    let path_a = make_repo(
        &dir_a,
        &[
            (
                "base.py",
                "\
class BaseHandler:
    def handle(self):
        pass
",
            ),
            (
                "child.py",
                "\
from base import BaseHandler

class MyHandler(BaseHandler):
    def handle(self):
        return 'hello'
",
            ),
        ],
    );

    // repo-B: base.py also defines BaseHandler (same qualified key: base::BaseHandler)
    let dir_b = tempfile::TempDir::new().unwrap();
    let path_b = make_repo(
        &dir_b,
        &[(
            "base.py",
            "\
class BaseHandler:
    def handle(self):
        pass
",
        )],
    );

    let ig_a = index_repo(&path_a);
    let ig_b = index_repo(&path_b);

    let mut registry = Registry {
        repos: HashMap::new(),
        groups: HashMap::new(),
    };
    registry.repos.insert(
        "lib-a".to_string(),
        RepoEntry {
            name: "lib-a".to_string(),
            path: path_a,
            languages: vec!["Python".to_string()],
            symbol_count: ig_a.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_a.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );
    registry.repos.insert(
        "lib-b".to_string(),
        RepoEntry {
            name: "lib-b".to_string(),
            path: path_b,
            languages: vec!["Python".to_string()],
            symbol_count: ig_b.stats().map(|s| s.symbols).unwrap_or(0),
            module_count: ig_b.stats().map(|s| s.modules).unwrap_or(0),
            last_indexed_commit: None,
        },
    );
    registry.groups.insert(
        "test-real-inherit".to_string(),
        Group {
            name: "test-real-inherit".to_string(),
            org: String::new(),
            repos: vec!["lib-a".to_string(), "lib-b".to_string()],
            contracts: vec![],
        },
    );

    build_combined_graph(&registry, "test-real-inherit").unwrap();

    // MyHandler in lib-a inherits BaseHandler in lib-a.
    // BaseHandler exists in both repos with same qualified key.
    // Cross-repo bridge should create INHERITS from lib-a::MyHandler to lib-b::BaseHandler.
    let cross_inherits = combined_query(
        "test-real-inherit",
        "MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) \
         WHERE a.id STARTS WITH '[lib-a]' AND b.id STARTS WITH '[lib-b]' \
         RETURN a.id, b.id",
    )
    .unwrap();
    assert!(
        !cross_inherits.is_empty(),
        "MyHandler inheriting BaseHandler should bridge to lib-b's BaseHandler via same qualified key"
    );

    std::env::set_var("HOME", &orig_home);
}
