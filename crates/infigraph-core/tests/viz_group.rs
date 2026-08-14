use infigraph_core::extract::extract_file;
use infigraph_core::graph::{GraphBackend, KuzuBackend};
use infigraph_core::viz::generate_group_html;
use infigraph_languages::bundled_registry;

fn backend_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, KuzuBackend) {
    let registry = bundled_registry().unwrap();
    let mut extractions = Vec::new();
    for (path, src) in files {
        let ext_dot = format!(".{}", path.rsplit('.').next().unwrap_or(""));
        let pack = registry.for_extension(&ext_dot).unwrap();
        extractions.push(extract_file(path, src, pack).unwrap());
    }
    let dir = tempfile::TempDir::new().unwrap();
    let backend = KuzuBackend::open(&dir.path().join("graph")).unwrap();
    backend.upsert_files_bulk(&extractions, true).unwrap();
    (dir, backend)
}

#[test]
fn group_html_contains_repo_nodes_and_target_service_edge() {
    let (_a_dir, backend_a) = backend_with(&[("a.py", b"def foo():\n    pass\n")]);
    let (_b_dir, backend_b) = backend_with(&[("b.py", b"def bar():\n    pass\n")]);

    // Simulate an HTTP/gRPC CALLS_SERVICE edge as link_cross_service_calls produces:
    // target_service names the target repo directly.
    backend_a
        .raw_query(
            "MATCH (a:Symbol {id: 'a.py::foo'}) \
             CREATE (ext:Symbol {id: 'EXTERNAL::b.py::bar'}) \
             CREATE (a)-[:CALLS_SERVICE {method: 'GRPC', path: '/x', target_service: 'repo-b'}]->(ext)",
        )
        .unwrap();

    let backends: Vec<(&str, &dyn GraphBackend)> =
        vec![("repo-a", &backend_a), ("repo-b", &backend_b)];
    let out_dir = tempfile::TempDir::new().unwrap();
    let out_path = out_dir.path().join("group.html");
    // generate_group_html (like generate_html/generate_symbol_html) returns
    // the output path, not the HTML content — read the written file to check.
    let returned_path = generate_group_html(&backends, &out_path).unwrap();
    assert_eq!(returned_path, out_path.to_string_lossy());
    let html = std::fs::read_to_string(&out_path).unwrap();

    assert!(html.contains("repo-a"), "missing repo-a: {}", html);
    assert!(html.contains("repo-b"));
    assert!(html.contains("GRPC"));
    assert!(out_path.exists());
}

#[test]
fn group_html_resolves_xlib_proxy_target_repo() {
    let (_a_dir, backend_a) = backend_with(&[("a.cpp", b"void foo() {}\n")]);
    let (_b_dir, backend_b) = backend_with(&[("b.cpp", b"void bar() {}\n")]);

    // Simulate a static-lib/namespace CALLS_SERVICE edge as
    // link_cross_repo_namespace_calls produces: target repo is encoded in
    // the xlib::{repo}::{id} proxy id; these edges carry `protocol`
    // (e.g. "static_lib") and `qualifier`, not `method`/`target_service`.
    backend_a
        .raw_query(
            "MATCH (a:Symbol {id: 'a.cpp::foo'}) \
             CREATE (ext:Symbol {id: 'xlib::repo-b::b.cpp::bar'}) \
             CREATE (a)-[:CALLS_SERVICE {protocol: 'static_lib', qualifier: 'bar'}]->(ext)",
        )
        .unwrap();

    let backends: Vec<(&str, &dyn GraphBackend)> =
        vec![("repo-a", &backend_a), ("repo-b", &backend_b)];
    let out_dir = tempfile::TempDir::new().unwrap();
    let out_path = out_dir.path().join("group2.html");
    generate_group_html(&backends, &out_path).unwrap();
    let html = std::fs::read_to_string(&out_path).unwrap();

    assert!(html.contains("repo-a"));
    assert!(html.contains("repo-b"));
    assert!(html.contains("static_lib"));
}
