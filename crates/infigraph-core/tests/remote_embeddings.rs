//! Integration tests for remote embedding storage (pgvector).
//!
//! Requires a running Postgres+pgvector container:
//!   docker run --name postgres-test -p 5432:5432 -e POSTGRES_USER=infigraph \
//!     -e POSTGRES_PASSWORD=infigraph -e POSTGRES_DB=infigraph pgvector/pgvector:pg16
//!
//! Run: cargo test --features remote -p infigraph-core --test remote_embeddings

#![cfg(feature = "postgres")]

use infigraph_core::meta::PostgresMetaStore;

fn connect_test_pg() -> Option<PostgresMetaStore> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost user=infigraph password=infigraph dbname=infigraph".into()
    });
    PostgresMetaStore::connect(&url).ok()
}

#[test]
fn test_embedder_dimension_matches_schema() {
    let embedder = infigraph_core::embed::best_embedder();
    assert_eq!(
        embedder.dimension(),
        256,
        "best_embedder dimension must match vector(256) in pgvector schema"
    );

    let doc_embedder = infigraph_core::embed::doc_embedder();
    assert_eq!(
        doc_embedder.dimension(),
        256,
        "doc_embedder dimension must match vector(256) in pgvector schema"
    );
}

#[test]
fn test_upsert_embedding_single() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let id = "test::single_embed";
    let vec = vec![0.1f32; 256];
    pg.upsert_embedding(id, "test", &vec).unwrap();

    let got = pg.get_embedding(id).unwrap();
    assert!(got.is_some(), "embedding should exist after upsert");
    assert_eq!(got.unwrap().len(), 256);

    pg.delete_embeddings(&[id.to_string()]).unwrap();
    assert!(pg.get_embedding(id).unwrap().is_none());
}

#[test]
fn test_upsert_embeddings_bulk_prepared_stmt() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let embeddings: Vec<(String, Vec<f32>)> = (0..100)
        .map(|i| (format!("test::bulk_{}", i), vec![i as f32 / 100.0; 256]))
        .collect();

    let count = pg.upsert_embeddings_bulk(&embeddings, "test_bulk").unwrap();
    assert_eq!(count, 100);

    let all = pg.all_embeddings("test_bulk").unwrap();
    assert_eq!(all.len(), 100);

    // Upsert again (ON CONFLICT UPDATE) — idempotent
    let count2 = pg.upsert_embeddings_bulk(&embeddings, "test_bulk").unwrap();
    assert_eq!(count2, 100);

    let ids: Vec<String> = embeddings.iter().map(|(id, _)| id.clone()).collect();
    pg.delete_embeddings(&ids).unwrap();
    assert_eq!(pg.all_embeddings("test_bulk").unwrap().len(), 0);
}

#[test]
fn test_bulk_delete_batching() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    // 600 > BATCH size of 500 — tests batch boundary
    let embeddings: Vec<(String, Vec<f32>)> = (0..600)
        .map(|i| (format!("test::batch_del_{}", i), vec![0.5f32; 256]))
        .collect();
    pg.upsert_embeddings_bulk(&embeddings, "test_batch_del")
        .unwrap();

    let ids: Vec<String> = embeddings.iter().map(|(id, _)| id.clone()).collect();
    pg.delete_embeddings(&ids).unwrap();
    assert_eq!(pg.all_embeddings("test_batch_del").unwrap().len(), 0);
}

#[test]
fn test_search_nearest_cosine() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let embeddings: Vec<(String, Vec<f32>)> = vec![
        ("test::nn_a".into(), vec![1.0f32; 256]),
        ("test::nn_b".into(), vec![0.5f32; 256]),
        ("test::nn_c".into(), vec![0.0f32; 256]),
    ];
    pg.upsert_embeddings_bulk(&embeddings, "test_nn").unwrap();

    let query = vec![1.0f32; 256];
    let results = pg.search_nearest(&query, "test_nn", 3).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].0, "test::nn_a", "nearest should be exact match");
    assert!(results[0].1 < 0.01, "distance to self should be ~0");

    let ids: Vec<String> = embeddings.iter().map(|(id, _)| id.clone()).collect();
    pg.delete_embeddings(&ids).unwrap();
}

#[test]
fn test_wrong_dimension_rejected() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let vec768 = vec![0.1f32; 768];
    let result = pg.upsert_embedding("test::wrong_dim", "test", &vec768);
    assert!(result.is_err(), "768-dim should fail on vector(256) column");
}

#[test]
fn test_empty_bulk_ops() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    assert_eq!(pg.upsert_embeddings_bulk(&[], "test").unwrap(), 0);
    pg.delete_embeddings(&[]).unwrap(); // should not error
}

#[test]
fn test_kind_separation() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let sym = vec![("test::kind_sym".to_string(), vec![0.1f32; 256])];
    let doc = vec![("test::kind_doc".to_string(), vec![0.2f32; 256])];

    pg.upsert_embeddings_bulk(&sym, "symbol").unwrap();
    pg.upsert_embeddings_bulk(&doc, "doc_chunk").unwrap();

    assert_eq!(
        pg.all_embeddings("symbol")
            .unwrap()
            .iter()
            .filter(|(id, _)| id.starts_with("test::kind_"))
            .count(),
        1
    );
    assert_eq!(
        pg.all_embeddings("doc_chunk")
            .unwrap()
            .iter()
            .filter(|(id, _)| id.starts_with("test::kind_"))
            .count(),
        1
    );

    // search_nearest respects kind
    let query = vec![0.1f32; 256];
    let results = pg.search_nearest(&query, "symbol", 10).unwrap();
    assert!(results
        .iter()
        .all(|(id, _)| !id.starts_with("test::kind_doc")));

    pg.delete_embeddings(&["test::kind_sym".into(), "test::kind_doc".into()])
        .unwrap();
}
