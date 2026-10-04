use infigraph_core::{
    index_status::{self, StageOutcome, StageState},
    Infigraph,
};
use infigraph_languages::bundled_registry;
use infigraph_mcp::tools::analysis::call_graph::{
    tool_trace_callees, tool_trace_callers, tool_transitive_impact,
};
use serde_json::json;

#[test]
fn queries_show_persistent_warnings_for_populated_and_empty_results_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.ts"),
        "const helper = () => 1; const status = () => helper();",
    )
    .unwrap();
    let mut prism = Infigraph::open(dir.path(), bundled_registry().unwrap()).unwrap();
    prism.init().unwrap();
    prism.index().unwrap();
    // AST and compiler enrichment can both contribute the same caller pair.
    prism.backend().unwrap().raw_query("MATCH (a:Symbol), (b:Symbol) WHERE a.id = 'a.ts::status' AND b.id = 'a.ts::helper' CREATE (a)-[:CALLS]->(b)").unwrap();
    let run = index_status::load(dir.path()).unwrap().run_id;
    index_status::update_current(dir.path(), &run, prism.backend(), |r| {
        r.scip.insert(
            "scip-typescript".into(),
            StageOutcome::new(StageState::Failed, "SCIP_PROCESS_FAILED"),
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    let expected = prism.coverage_notice();
    drop(prism);
    let report = dir.path().join(".infigraph/index-status.json");
    let before = std::fs::read(&report).unwrap();
    let args = json!({"path":dir.path(), "symbol_id":"a.ts::helper"});
    let callers = tool_trace_callers(&args).unwrap();
    assert!(callers.starts_with(&expected));
    assert!(callers.contains("a.ts::status"));
    assert_eq!(callers.matches("a.ts::status").count(), 1);
    let populated_callees =
        tool_trace_callees(&json!({"path":dir.path(), "symbol_id":"a.ts::status"})).unwrap();
    assert_eq!(populated_callees.matches("a.ts::helper").count(), 1);
    let impact = tool_transitive_impact(&args).unwrap();
    assert!(impact.starts_with(&expected));
    assert!(impact.contains("status"));
    let callees = tool_trace_callees(&args).unwrap();
    assert!(callees.starts_with(&expected));
    assert!(callees.contains("current graph"));
    let empty_args = json!({"path":dir.path(), "symbol_id":"a.ts::missing"});
    for tool in [
        tool_trace_callers,
        tool_trace_callees,
        tool_transitive_impact,
    ] {
        let output = tool(&empty_args).unwrap();
        assert!(output.starts_with(&expected));
        assert!(output.contains("current graph"));
    }
    assert_eq!(std::fs::read(&report).unwrap(), before);
    std::fs::remove_file(&report).unwrap();
    let unknown = tool_trace_callers(&args).unwrap();
    assert!(unknown.contains("coverage unknown"));
    assert!(!report.exists());
}
