use anyhow::Result;

use crate::graph::GraphBackend;

pub fn promote_bridges_to_calls(backend: &dyn GraphBackend) -> Result<usize> {
    let query = "MATCH (a:Symbol)-[b:BRIDGE_TO]->(t:Symbol) RETURN a.id, t.id, b.bridge_kind";
    let bridges = backend.raw_query(query)?;

    let mut promoted = 0;
    for row in &bridges {
        if row.len() < 2 {
            continue;
        }
        let source_id = &row[0];
        let target_id = &row[1];

        let check = format!(
            "MATCH (a:Symbol {{id: '{}'}})-[:CALLS]->(b:Symbol {{id: '{}'}}) RETURN a.id",
            source_id.replace('\'', "\\'"),
            target_id.replace('\'', "\\'"),
        );
        let existing = backend.raw_query(&check).unwrap_or_default();
        if !existing.is_empty() {
            continue;
        }

        let insert = format!(
            "MATCH (a:Symbol {{id: '{}'}}), (b:Symbol {{id: '{}'}}) CREATE (a)-[:CALLS]->(b)",
            source_id.replace('\'', "\\'"),
            target_id.replace('\'', "\\'"),
        );
        if backend.raw_query(&insert).is_ok() {
            promoted += 1;
        }
    }
    Ok(promoted)
}
