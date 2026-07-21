use serde_json::{json, Value};

use infigraph_core::embed;

use super::open_prism;

pub(crate) fn api_index(params: &Value) -> Value {
    match open_prism(params) {
        Ok(prism) => match prism.index() {
            Ok(result) => json!({
                "success": true,
                "files": result.indexed_files,
                "total": result.total_files,
                "resolve": format!("{}", result.resolve_stats),
            }),
            Err(e) => json!({"error": e.to_string()}),
        },
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_search(params: &Value) -> Value {
    let query = params.get("query").and_then(|q| q.as_str()).unwrap_or("");
    let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;

    match open_prism(params) {
        Ok(prism) => {
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error": "not initialized"}),
            };

            let rows = match backend
                .raw_query("MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.docstring")
            {
                Ok(r) => r,
                Err(e) => return json!({"error": e.to_string()}),
            };

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

            let bm25 = infigraph_core::search::BM25Index::build(docs.clone());
            let embedder = embed::best_embedder();
            let emb_path = prism.root().join(".infigraph").join("embeddings.bin");
            let embs: Vec<(String, Vec<f32>)> = if emb_path.exists() {
                match embed::load_embeddings_cached(&emb_path) {
                    Ok(e) => e,
                    Err(_) => docs
                        .iter()
                        .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
                        .collect(),
                }
            } else {
                docs.iter()
                    .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap_or_default()))
                    .collect()
            };

            let hnsw_path = prism.root().join(".infigraph").join("hnsw_index.usearch");
            match infigraph_core::search::hybrid_search(
                query,
                &bm25,
                embedder.as_ref(),
                &embs,
                limit,
                0.3,
                Some(&hnsw_path),
                Some(&emb_path),
            ) {
                Ok(results) => {
                    let items: Vec<Value> = results
                        .iter()
                        .filter_map(|r| {
                            rows.iter().find(|row| row[0] == r.symbol_id).map(|row| {
                                json!({
                                    "id": row[0],
                                    "name": row[1],
                                    "kind": row[2],
                                    "file": row[3],
                                    "score": r.score,
                                    "bm25": r.bm25_score,
                                    "vector": r.vector_score,
                                })
                            })
                        })
                        .collect();
                    json!({"results": items})
                }
                Err(e) => json!({"error": e.to_string()}),
            }
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_query(params: &Value) -> Value {
    let cypher = params.get("cypher").and_then(|c| c.as_str()).unwrap_or("");

    match open_prism(params) {
        Ok(prism) => {
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error": "not initialized"}),
            };
            match backend.raw_query(cypher) {
                Ok(rows) => json!({"rows": rows}),
                Err(e) => json!({"error": e.to_string()}),
            }
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_symbols(params: &Value) -> Value {
    let file = params.get("file").and_then(|f| f.as_str()).unwrap_or("");

    match open_prism(params) {
        Ok(prism) => {
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error": "not initialized"}),
            };
            let symbols = backend.symbols_in_file(file).unwrap_or_default();

            let items: Vec<Value> = symbols
                .iter()
                .map(|s| json!({"id": s.id, "name": s.name, "kind": s.kind, "startLine": s.start_line, "endLine": s.end_line}))
                .collect();

            json!({"symbols": items})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_symbol_context(params: &Value) -> Value {
    let symbol_id = params
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .unwrap_or("");

    match open_prism(params) {
        Ok(prism) => {
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error": "not initialized"}),
            };

            let detail = backend.find_symbol_by_id(symbol_id).ok().flatten();
            let callers = backend.callers_of(symbol_id).unwrap_or_default();
            let callees = backend.callees_of(symbol_id).unwrap_or_default();

            json!({
                "symbol": detail.map(|d| json!({
                    "id": d.id, "name": d.name, "kind": d.kind,
                    "file": d.file, "startLine": d.start_line, "endLine": d.end_line,
                })),
                "callers": callers,
                "callees": callees,
            })
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

pub(crate) fn api_snippet(params: &Value) -> Value {
    let symbol_id = params
        .get("symbol_id")
        .and_then(|s| s.as_str())
        .unwrap_or("");

    match open_prism(params) {
        Ok(prism) => {
            let backend = match prism.backend() {
                Some(b) => b,
                None => return json!({"error": "not initialized"}),
            };

            match backend.find_symbol_by_id(symbol_id) {
                Ok(Some(detail)) => {
                    let file_path = prism.root().join(&detail.file);
                    let snippet = infigraph_core::search::read_lines_from_file(
                        &file_path,
                        detail.start_line,
                        detail.end_line,
                    )
                    .unwrap_or_else(|_| "(source not available)".to_string());

                    json!({
                        "symbol": detail.name,
                        "file": detail.file,
                        "startLine": detail.start_line,
                        "endLine": detail.end_line,
                        "code": snippet,
                    })
                }
                _ => json!({"error": format!("symbol '{}' not found", symbol_id)}),
            }
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}
