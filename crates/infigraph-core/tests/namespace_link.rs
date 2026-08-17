use infigraph_core::extract::extract_file;
use infigraph_core::graph::{GraphBackend, KuzuBackend};
use infigraph_core::multi::namespace_link::link_cross_repo_namespace_calls;
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
    // upsert_files_bulk only writes raw nodes/edges from extraction — it does
    // not run call resolution. Namespace-qualified calls that don't resolve
    // locally (e.g. `tps::SetFormML` with no local `tps` symbol) only become
    // EXTERNAL_CALL/ExternalRef nodes via resolve_calls's write_external_calls
    // path (see resolve/calls.rs), so it must run here for the cross-repo
    // linker under test to have anything to find.
    backend.resolve_calls(&extractions, None).unwrap();
    (dir, backend)
}

#[test]
fn links_namespace_qualified_call_across_repos() {
    // Producer: TpsBridge-style repo defining tps::SetFormML
    let (_producer_dir, producer_backend) = backend_with(&[(
        "Src/High/HAPI/FormML/zhaSetFormML.cpp",
        br#"
namespace tps
{
    void SetFormML(int entity, const char* formML)
    {
        DoWork(entity, formML);
    }
}
"#,
    )]);
    // Consumer: tto-engine-style repo calling tps::SetFormML
    let (_consumer_dir, consumer_backend) = backend_with(&[(
        "Src/TaxApp/Server/grpc/service/TaxReturnService.cpp",
        br#"
void SetFormMLHandler(int entity, const char* formML) {
    tps::SetFormML(entity, formML);
}
"#,
    )]);

    let backends: Vec<(&str, &dyn GraphBackend)> = vec![
        ("tps-bridge", &producer_backend),
        ("tto-engine", &consumer_backend),
    ];
    let linked = link_cross_repo_namespace_calls(&backends).unwrap();
    assert_eq!(linked, 1, "expected exactly one cross-repo namespace edge");

    let rows = consumer_backend
        .raw_query(
            "MATCH (a:Symbol)-[r:CALLS_SERVICE]->(b:Symbol) \
             WHERE r.protocol = 'static_lib' RETURN a.id, b.id, r.qualifier",
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0][1].ends_with("tps::SetFormML"));
    assert_eq!(rows[0][2], "tps");
}

#[test]
fn does_not_link_when_qualifier_matches_multiple_repos() {
    // Two "producer" repos both defining tps::SetFormML — ambiguous, must not link.
    let (_p1_dir, producer1) = backend_with(&[(
        "a.cpp",
        br#"namespace tps { void SetFormML(int e) { DoA(e); } }"#,
    )]);
    let (_p2_dir, producer2) = backend_with(&[(
        "b.cpp",
        br#"namespace tps { void SetFormML(int e) { DoB(e); } }"#,
    )]);
    let (_consumer_dir, consumer_backend) =
        backend_with(&[("c.cpp", br#"void Handler(int e) { tps::SetFormML(e); }"#)]);

    let backends: Vec<(&str, &dyn GraphBackend)> = vec![
        ("repo-a", &producer1),
        ("repo-b", &producer2),
        ("repo-c", &consumer_backend),
    ];
    let linked = link_cross_repo_namespace_calls(&backends).unwrap();
    assert_eq!(linked, 0, "ambiguous match across 2 repos must not link");
}
