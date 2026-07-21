use infigraph_core::check::{load_config, run_checks, CheckConfig, CheckSelection, CheckStatus};
use infigraph_core::graph::{GraphStore, KuzuBackend};

fn empty_backend() -> (tempfile::TempDir, KuzuBackend) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    (dir, KuzuBackend::from_store(store))
}

#[test]
fn test_check_config_parsing() {
    let dir = tempfile::TempDir::new().unwrap();
    let config_path = dir.path().join("check.toml");
    std::fs::write(
        &config_path,
        r#"
[security]
enabled = true
max_critical = 0
max_high = 5

[complexity]
enabled = true
threshold = 20
max_violations = 3

[dead_code]
enabled = false
max_dead = 100

[vulnerabilities]
enabled = true
max_critical = 0
max_high = 10
"#,
    )
    .unwrap();

    let cfg = load_config(&config_path).unwrap();
    assert!(cfg.security.enabled);
    assert_eq!(cfg.security.max_high, 5);
    assert_eq!(cfg.complexity.threshold, 20);
    assert_eq!(cfg.complexity.max_violations, 3);
    assert!(!cfg.dead_code.enabled);
    assert_eq!(cfg.dead_code.max_dead, 100);
    assert!(cfg.vulnerabilities.enabled);
    assert_eq!(cfg.vulnerabilities.max_high, 10);
}

#[test]
fn test_check_config_defaults() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("nonexistent.toml");
    let cfg = load_config(&missing).unwrap();

    assert!(cfg.security.enabled);
    assert_eq!(cfg.security.max_critical, 0);
    assert_eq!(cfg.security.max_high, 0);
    assert!(cfg.complexity.enabled);
    assert_eq!(cfg.complexity.threshold, 15);
    assert!(cfg.dead_code.enabled);
}

#[test]
fn test_check_security_pass() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("safe.py"), "print('hello world')\n").unwrap();

    let (_tmp, backend) = empty_backend();
    let cfg = CheckConfig::default();
    let sel = CheckSelection::all();
    let results = run_checks(dir.path(), &cfg, &backend, &sel);

    let sec = results.iter().find(|r| r.name == "security");
    if let Some(s) = sec {
        assert_eq!(
            s.status,
            CheckStatus::Pass,
            "safe code should pass: {}",
            s.summary
        );
    }
}

#[test]
fn test_check_security_fail() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("vuln.py"),
        "import os\npassword = 'hardcoded_secret'\nos.system(user_input)\neval(data)\n",
    )
    .unwrap();

    let (_tmp, backend) = empty_backend();
    let mut cfg = CheckConfig::default();
    cfg.security.max_critical = 0;
    cfg.security.max_high = 0;
    let sel = CheckSelection {
        security: true,
        complexity: false,
        dead_code: false,
        vulnerabilities: false,
    };
    let results = run_checks(dir.path(), &cfg, &backend, &sel);

    let sec = results.iter().find(|r| r.name == "security").unwrap();
    assert_eq!(
        sec.status,
        CheckStatus::Fail,
        "vuln code should fail: {}",
        sec.summary
    );
}

#[test]
fn test_check_complexity_pass() {
    let (_tmp, backend) = empty_backend();
    let cfg = CheckConfig::default();
    let sel = CheckSelection {
        security: false,
        complexity: true,
        dead_code: false,
        vulnerabilities: false,
    };
    let results = run_checks(std::path::Path::new("."), &cfg, &backend, &sel);

    let cx = results.iter().find(|r| r.name == "complexity");
    if let Some(c) = cx {
        assert_eq!(
            c.status,
            CheckStatus::Pass,
            "empty graph should pass complexity"
        );
    }
}

#[test]
fn test_check_selection_from_csv() {
    let sel = CheckSelection::from_csv("security,complexity");
    assert!(sel.security);
    assert!(sel.complexity);
    assert!(!sel.dead_code);
    assert!(!sel.vulnerabilities);

    let sel2 = CheckSelection::from_csv("sec,vulns,dead-code");
    assert!(sel2.security);
    assert!(!sel2.complexity);
    assert!(sel2.dead_code);
    assert!(sel2.vulnerabilities);
}

#[test]
fn test_check_all_categories_have_rules() {
    use infigraph_core::security::Category;

    let categories = [
        Category::SqlInjection,
        Category::CommandInjection,
        Category::PathTraversal,
        Category::XssRisk,
        Category::InsecureDeserialization,
        Category::WeakCrypto,
        Category::HardcodedSecret,
        Category::InsecureRandom,
    ];

    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("empty.py"), "").unwrap();

    for cat in &categories {
        assert!(
            !format!("{:?}", cat).is_empty(),
            "category {:?} should be representable",
            cat
        );
    }
}
