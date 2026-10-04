use serde_json::Value;
use std::process::Command;

fn doctor(project: &std::path::Path, home: &std::path::Path, json: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_infigraph"));
    command.args(["--root"]).arg(project).arg("doctor");
    if json {
        command.arg("--json");
    }
    command
        .env("HOME", home)
        .env("INFIGRAPH_REGISTRY_HOME", home)
        .env("INFIGRAPH_BACKEND", "kuzu")
        .env_remove("INFIGRAPH_NO_WATCH")
        .env_remove("CI");
    command.output().unwrap()
}

#[test]
fn json_is_clean_with_expected_exit_and_no_startup_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = doctor(dir.path(), home.path(), true);
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "unhealthy");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "PROJECT_NOT_INDEXED"));
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {:?}",
        output.stderr
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    let human = doctor(dir.path(), home.path(), false);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Infigraph Doctor"));
    assert!(human.contains("[error] PROJECT_NOT_INDEXED"));
    assert!(human.contains("Result: UNHEALTHY"));
    assert!(human.contains("Index freshness: UNKNOWN"));
}

#[test]
fn indexed_project_and_wal_state_use_real_isolated_probe() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.py"), "def greet():\n    return 1\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_infigraph"))
        .arg("--root")
        .arg(dir.path())
        .args(["index", "--no-embed"])
        .env("HOME", home.path())
        .env("INFIGRAPH_REGISTRY_HOME", home.path())
        .env("INFIGRAPH_BACKEND", "kuzu")
        .env("INFIGRAPH_NO_WATCH", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let graph_path = dir.path().join(".infigraph/graph");
    let before = std::fs::read(&graph_path).unwrap();
    let output = doctor(dir.path(), home.path(), true);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "healthy");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "GRAPH_READABLE"));
    let human = doctor(dir.path(), home.path(), false);
    assert_eq!(human.status.code(), Some(0));
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Result: HEALTHY\nIndex freshness: UNKNOWN"));
    assert_eq!(std::fs::read(&graph_path).unwrap(), before);
    assert!(!dir.path().join(".infigraph/watch.lock").exists());
    // A registry revision match cannot establish freshness of working changes.
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "indexed",
    ]);
    let registry = serde_json::json!({"repos":{"fixture":{
        "name":"fixture", "path":dir.path(), "languages":[], "symbol_count":1,
        "module_count":0, "last_indexed_commit":git(&["rev-parse", "HEAD"])
    }},"groups":{}});
    std::fs::create_dir_all(home.path().join(".infigraph")).unwrap();
    std::fs::write(
        home.path().join(".infigraph/registry.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("app.py"), "def changed():\n    return 2\n").unwrap();
    let human = doctor(dir.path(), home.path(), false);
    assert_eq!(human.status.code(), Some(0));
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains(
        "Index freshness: UNKNOWN (recorded revision matches; working-tree freshness unverified)"
    ));
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "new HEAD",
    ]);
    let human = doctor(dir.path(), home.path(), false);
    assert_eq!(human.status.code(), Some(1));
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains(
        "Result: DEGRADED\nIndex freshness: STALE REVISION (working-tree freshness unverified)"
    ));
    let wal = dir.path().join(".infigraph/graph.wal");
    std::fs::write(&wal, b"must survive doctor").unwrap();
    let output = doctor(dir.path(), home.path(), true);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "GRAPH_STATUS_UNKNOWN"));
    assert_eq!(std::fs::read(wal).unwrap(), b"must survive doctor");
    assert_eq!(std::fs::read(graph_path).unwrap(), before);
}

#[test]
fn unsupported_remote_diagnostics_do_not_connect_or_create_local_state() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_infigraph"))
        .arg("--root")
        .arg(dir.path())
        .args(["doctor", "--json"])
        .env("HOME", home.path())
        .env("INFIGRAPH_REGISTRY_HOME", home.path())
        .env("INFIGRAPH_BACKEND", "neo4j")
        .env("NEO4J_URI", "bolt://127.0.0.1:1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "REMOTE_STATUS_UNKNOWN"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn native_probe_failure_is_contained_without_repair() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".infigraph")).unwrap();
    let path = project.path().join(".infigraph/graph");
    let invalid = vec![0u8; 8192];
    std::fs::write(&path, &invalid).unwrap();
    let output = doctor(project.path(), home.path(), true);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["code"] == "GRAPH_STATUS_UNKNOWN"));
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read(path).unwrap(), invalid);
    assert_eq!(
        std::fs::read_dir(project.path().join(".infigraph"))
            .unwrap()
            .count(),
        1
    );
}
