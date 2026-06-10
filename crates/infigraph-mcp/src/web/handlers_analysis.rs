use std::path::PathBuf;

use serde_json::{json, Value};

use infigraph_core::embed;
use infigraph_core::graph::GraphQuery;

use super::open_prism;

pub(crate) fn api_architecture(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error": "not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);

            let langs = gq
                .raw_query("MATCH (m:Module) RETURN m.language, count(m)")
                .unwrap_or_default();
            let kinds = gq
                .raw_query("MATCH (s:Symbol) RETURN s.kind, count(s)")
                .unwrap_or_default();
            let hotspots = gq
                .raw_query(
                    "MATCH (s:Symbol) RETURN s.file, count(s) AS cnt ORDER BY cnt DESC LIMIT 10",
                )
                .unwrap_or_default();
            let hubs = gq.raw_query("MATCH ()-[:CALLS]->(s:Symbol) RETURN s.name, s.file, count(*) AS calls ORDER BY calls DESC LIMIT 10").unwrap_or_default();
            let stats = prism.stats().ok();

            json!({
                "languages": langs,
                "symbolKinds": kinds,
                "hotspots": hotspots,
                "hubs": hubs,
                "stats": stats.map(|s| json!({
                    "symbols": s.symbols,
                    "modules": s.modules,
                    "calls": s.calls,
                    "inherits": s.inherits,
                    "contains": s.contains,
                })),
            })
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_dead_code(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error": "not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);
            let rows = gq.raw_query(
                "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN s.name, s.kind, s.file ORDER BY s.file"
            ).unwrap_or_default();

            let entry_points = ["main", "__init__", "setUp", "tearDown"];
            let dead: Vec<Value> = rows
                .iter()
                .filter(|r| !entry_points.contains(&r[0].as_str()))
                .map(|r| json!({"name": r[0], "kind": r[1], "file": r[2]}))
                .collect();

            json!({"deadCode": dead, "count": dead.len()})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_graph_data(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error": "not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);

            let nodes = gq
                .raw_query("MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file")
                .unwrap_or_default();
            let calls = gq
                .raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")
                .unwrap_or_default();
            let inherits = gq
                .raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")
                .unwrap_or_default();

            let node_items: Vec<Value> = nodes
                .iter()
                .map(|r| json!({"id": r[0], "name": r[1], "kind": r[2], "file": r[3]}))
                .collect();
            let edge_items: Vec<Value> = calls
                .iter()
                .map(|r| json!({"from": r[0], "to": r[1], "type": "CALLS"}))
                .chain(
                    inherits
                        .iter()
                        .map(|r| json!({"from": r[0], "to": r[1], "type": "INHERITS"})),
                )
                .collect();

            json!({"nodes": node_items, "edges": edge_items})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_stats(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => match prism.stats() {
            Ok(s) => json!({
                "symbols": s.symbols,
                "modules": s.modules,
                "calls": s.calls,
                "inherits": s.inherits,
                "contains": s.contains,
            }),
            Err(e) => json!({"error": e.to_string()}),
        },
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_cluster(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error": "not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            match infigraph_core::cluster::detect_clusters(&conn) {
                Ok(stats) => json!({
                    "clusters": stats.num_clusters,
                    "modularity": stats.modularity,
                    "sizes": stats.cluster_sizes,
                }),
                Err(e) => json!({"error": e.to_string()}),
            }
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_routes(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error": "not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);
            match infigraph_core::routes::detect_routes(&gq) {
                Ok(routes) => {
                    let items: Vec<Value> = routes
                        .iter()
                        .map(|r| {
                            json!({
                                "method": r.method,
                                "path": r.path,
                                "handler": r.handler_id,
                                "file": r.file,
                            })
                        })
                        .collect();
                    json!({"routes": items, "count": items.len()})
                }
                Err(e) => json!({"error": e.to_string()}),
            }
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_groups(_params: &Value) -> Value {
    let registry_path = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".infigraph").join("registry.json"))
        .unwrap_or_default();
    if !registry_path.exists() {
        return json!({"groups": [], "count": 0});
    }
    match std::fs::read_to_string(&registry_path) {
        Ok(content) => {
            let reg: Value = serde_json::from_str(&content).unwrap_or(json!({}));
            let groups = reg.get("groups").cloned().unwrap_or(json!({}));
            let group_list: Vec<Value> = if let Some(obj) = groups.as_object() {
                obj.iter()
                    .map(|(name, g)| {
                        let repos = g.get("repos").and_then(|r| r.as_array()).map(|a| a.len()).unwrap_or(0);
                        let contracts = g.get("contracts").and_then(|c| c.as_array()).map(|a| a.len()).unwrap_or(0);
                        json!({"name": name, "repoCount": repos, "contractCount": contracts, "repos": g.get("repos"), "contracts": g.get("contracts")})
                    })
                    .collect()
            } else {
                vec![]
            };
            json!({"groups": group_list, "count": group_list.len()})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_contracts(params: &Value) -> Value {
    let group_name = params.get("group").and_then(|g| g.as_str()).unwrap_or("");
    let registry_path = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".infigraph").join("registry.json"))
        .unwrap_or_default();
    if !registry_path.exists() {
        return json!({"contracts": [], "count": 0});
    }
    match std::fs::read_to_string(&registry_path) {
        Ok(content) => {
            let reg: Value = serde_json::from_str(&content).unwrap_or(json!({}));
            let contracts = if group_name.is_empty() {
                // Return all contracts from all groups
                let mut all = Vec::new();
                if let Some(groups) = reg.get("groups").and_then(|g| g.as_object()) {
                    for (_name, g) in groups {
                        if let Some(cs) = g.get("contracts").and_then(|c| c.as_array()) {
                            all.extend(cs.clone());
                        }
                    }
                }
                all
            } else {
                reg.get("groups")
                    .and_then(|g| g.get(group_name))
                    .and_then(|g| g.get("contracts"))
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            };
            json!({"contracts": contracts, "count": contracts.len()})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_complexity(params: &Value) -> Value {
    let threshold = params
        .get("threshold")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as i64;
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error":"not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error":e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);
            let rows = gq.raw_query(
                "MATCH (s:Symbol) WHERE s.kind IN ['Function','Method','Test'] AND s.complexity IS NOT NULL RETURN s.id, s.name, s.kind, s.file, s.start_line, s.complexity ORDER BY s.complexity DESC"
            ).unwrap_or_default();

            let items: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r[0], "name": r[1], "kind": r[2], "file": r[3],
                        "line": r[4].parse::<u32>().unwrap_or(0),
                        "complexity": r[5].parse::<i64>().unwrap_or(1),
                    })
                })
                .collect();

            let hotspots: Vec<&Value> = items
                .iter()
                .filter(|v| v["complexity"].as_i64().unwrap_or(0) >= threshold)
                .collect();
            let avg = if items.is_empty() {
                0.0
            } else {
                items
                    .iter()
                    .map(|v| v["complexity"].as_f64().unwrap_or(1.0))
                    .sum::<f64>()
                    / items.len() as f64
            };
            json!({"symbols": items, "hotspots": hotspots, "avg": avg, "threshold": threshold})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_security(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => match infigraph_core::security::scan_project(prism.root()) {
            Ok(stats) => {
                let findings: Vec<Value> = stats
                    .findings
                    .iter()
                    .map(|f| {
                        json!({
                            "file": f.file, "line": f.line, "col": f.col,
                            "severity": f.severity.to_string(),
                            "category": f.category.to_string(),
                            "rule_id": f.rule_id,
                            "message": f.message,
                            "snippet": f.snippet,
                        })
                    })
                    .collect();
                json!({
                    "findings": findings,
                    "total": findings.len(),
                    "critical": stats.critical_count(),
                    "high": stats.high_count(),
                    "medium": stats.medium_count(),
                    "low": stats.low_count(),
                })
            }
            Err(e) => json!({"error": e.to_string()}),
        },
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_bridges(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => match infigraph_core::bridges::detect_bridges(prism.root()) {
            Ok(result) => {
                let items: Vec<Value> = result
                    .bridges
                    .iter()
                    .map(|b| {
                        json!({
                            "file": b.file, "line": b.line,
                            "kind": b.kind.as_str(),
                            "foreign_symbol": b.foreign_symbol,
                            "source_language": b.source_language,
                            "target_language": b.target_language,
                            "detail": b.detail,
                        })
                    })
                    .collect();
                json!({"bridges": items, "total": items.len()})
            }
            Err(e) => json!({"error": e.to_string()}),
        },
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_clones(params: &Value) -> Value {
    let threshold = params
        .get("threshold")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.92) as f32;
    match open_prism(params) {
        Ok(prism) => {
            let store = match prism.store() {
                Some(s) => s,
                None => return json!({"error":"not initialized"}),
            };
            let conn = match store.connection() {
                Ok(c) => c,
                Err(e) => return json!({"error":e.to_string()}),
            };
            let gq = GraphQuery::new(&conn);

            let rows = gq.raw_query(
                "MATCH (s:Symbol) WHERE s.kind IN ['Function','Method'] RETURN s.id, s.name, s.kind, s.file, s.docstring"
            ).unwrap_or_default();

            if rows.len() < 2 {
                return json!({"pairs": [], "total": 0});
            }

            let embedder = embed::best_embedder();
            let docs: Vec<(String, String)> = rows
                .iter()
                .map(|row| {
                    let id = row[0].clone();
                    let text = if row.get(4).is_some_and(|s| !s.is_empty()) {
                        format!("{} {}: {}", row[2], row[1], row[4])
                    } else {
                        format!("{} {}", row[2], row[1])
                    };
                    (id, text)
                })
                .collect();

            let emb_path = prism.root().join(".infigraph").join("embeddings.bin");
            let cached: std::collections::HashMap<String, Vec<f32>> = if emb_path.exists() {
                infigraph_core::embed::load_embeddings_cached(&emb_path)
                    .unwrap_or_default()
                    .into_iter()
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

            let vecs: Vec<(String, String, String, Vec<f32>)> = docs
                .iter()
                .map(|(id, text)| {
                    let emb = cached
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| embedder.embed(text).unwrap_or_default());
                    let row = rows.iter().find(|r| &r[0] == id).unwrap();
                    (id.clone(), row[1].clone(), row[3].clone(), emb)
                })
                .filter(|(_, _, _, e)| !e.is_empty())
                .collect();

            let n = vecs.len();
            let mut pairs: Vec<Value> = Vec::new();
            for i in 0..n {
                for j in (i + 1)..n {
                    if vecs[i].2 == vecs[j].2 {
                        continue;
                    }
                    let sim = infigraph_core::embed::cosine_similarity(&vecs[i].3, &vecs[j].3);
                    if sim >= threshold {
                        pairs.push(json!({
                            "score": sim,
                            "a": {"id": vecs[i].0, "name": vecs[i].1, "file": vecs[i].2},
                            "b": {"id": vecs[j].0, "name": vecs[j].1, "file": vecs[j].2},
                        }));
                    }
                }
            }
            pairs.sort_by(|a, b| {
                b["score"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            pairs.truncate(50);
            json!({"pairs": pairs, "total": pairs.len(), "checked": n})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}
