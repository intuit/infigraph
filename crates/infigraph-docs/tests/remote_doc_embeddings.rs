//! Integration tests for doc embeddings → pgvector remote storage.
//!
//! Requires running Postgres+pgvector container.
//! Run: cargo test --features remote -p infigraph-docs --test remote_doc_embeddings

#![cfg(feature = "remote")]

use infigraph_core::meta::PostgresMetaStore;
use infigraph_docs::chunk::Chunk;
use infigraph_docs::store::DocStore;
use std::path::Path;

fn connect_test_pg() -> Option<PostgresMetaStore> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost user=infigraph password=infigraph dbname=infigraph".into()
    });
    PostgresMetaStore::connect(&url).ok()
}

fn make_test_store(dir: &Path) -> DocStore {
    let db_path = dir.join("test_docs.kuzu");
    DocStore::open(&db_path).unwrap()
}

fn make_chunk(id: &str, doc_file: &str, text: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        doc_file: doc_file.to_string(),
        content_hash: "abc123".to_string(),
        index: 0,
        heading: Some("Test Heading".to_string()),
        text: text.to_string(),
        start_offset: 0,
        end_offset: text.len(),
        page: None,
    }
}

#[test]
fn test_doc_embeddings_remote_stores_to_pgvector() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    // Clean up any prior test data
    let prior = pg.all_embeddings("doc_chunk").unwrap_or_default();
    let prior_test: Vec<String> = prior
        .into_iter()
        .filter(|(id, _)| id.starts_with("test_doc"))
        .map(|(id, _)| id)
        .collect();
    if !prior_test.is_empty() {
        pg.delete_embeddings(&prior_test).unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let store = make_test_store(tmp.path());

    // Insert docs+chunks into DocStore
    let doc = infigraph_docs::extract::ExtractedDoc {
        title: Some("Test Doc".into()),
        file: "test_doc.md".into(),
        format: infigraph_docs::extract::DocFormat::Markdown,
        content_hash: "hash1".into(),
        text: "This is test content".into(),
        page_count: None,
    };
    let chunks = vec![
        make_chunk(
            "test_doc.md::chunk_0",
            "test_doc.md",
            "First chunk about testing",
        ),
        make_chunk(
            "test_doc.md::chunk_1",
            "test_doc.md",
            "Second chunk about embeddings",
        ),
        make_chunk(
            "test_doc.md::chunk_2",
            "test_doc.md",
            "Third chunk about pgvector",
        ),
    ];
    let doc_refs: Vec<&infigraph_docs::extract::ExtractedDoc> = vec![&doc];
    let chunk_refs: Vec<&Chunk> = chunks.iter().collect();
    store.upsert_all_parquet(&doc_refs, &chunk_refs).unwrap();

    // Call remote embedding
    let empty_chunks: Vec<&Chunk> = vec![];
    let changed: Vec<&str> = vec!["test_doc.md"];
    let count =
        infigraph_docs::embed::update_doc_embeddings_remote(&store, &pg, &empty_chunks, &changed)
            .unwrap();
    assert!(count >= 3, "should embed at least 3 chunks, got {}", count);

    // Verify in pgvector
    let stored = pg.all_embeddings("doc_chunk").unwrap();
    let test_stored: Vec<_> = stored
        .iter()
        .filter(|(id, _)| id.starts_with("test_doc"))
        .collect();
    assert_eq!(test_stored.len(), 3, "3 doc chunks should be in pgvector");

    // Verify dimension
    for (_, vec) in &test_stored {
        assert_eq!(vec.len(), 256, "doc embeddings should be 256-dim");
    }

    // Verify nearest-neighbor works
    let query_vec = &test_stored[0].1;
    let results = pg.search_nearest(query_vec, "doc_chunk", 3).unwrap();
    assert!(!results.is_empty());

    // Cleanup
    let ids: Vec<String> = test_stored.iter().map(|(id, _)| id.clone()).collect();
    pg.delete_embeddings(&ids).unwrap();
}

#[test]
fn test_doc_embeddings_remote_incremental() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let store = make_test_store(tmp.path());

    // Insert initial doc
    let doc = infigraph_docs::extract::ExtractedDoc {
        title: Some("Incremental Doc".into()),
        file: "incr_test.md".into(),
        format: infigraph_docs::extract::DocFormat::Markdown,
        content_hash: "hash_incr".into(),
        text: "Incremental test".into(),
        page_count: None,
    };
    let chunks = vec![make_chunk(
        "incr_test.md::chunk_0",
        "incr_test.md",
        "Initial chunk",
    )];
    let doc_refs = vec![&doc];
    let chunk_refs: Vec<&Chunk> = chunks.iter().collect();
    store.upsert_all_parquet(&doc_refs, &chunk_refs).unwrap();

    // First run — embeds 1 chunk
    let empty: Vec<&Chunk> = vec![];
    let changed: Vec<&str> = vec!["incr_test.md"];
    let count1 =
        infigraph_docs::embed::update_doc_embeddings_remote(&store, &pg, &empty, &changed).unwrap();
    assert!(count1 >= 1);

    // Second run with no changes — should embed 0 new (already in pgvector)
    let no_changes: Vec<&str> = vec![];
    let count2 =
        infigraph_docs::embed::update_doc_embeddings_remote(&store, &pg, &empty, &no_changes)
            .unwrap();
    // count2 includes existing count, but 0 new embeddings stored
    assert!(count2 >= 1);

    // Cleanup
    pg.delete_embeddings(&["incr_test.md::chunk_0".to_string()])
        .unwrap();
}

#[test]
fn test_doc_embeddings_remote_orphan_cleanup() {
    let pg = match connect_test_pg() {
        Some(pg) => pg,
        None => {
            eprintln!("SKIP: Postgres not available");
            return;
        }
    };
    pg.init_schema().unwrap();

    // Insert an orphan embedding directly
    let orphan_vec = vec![0.1f32; 256];
    pg.upsert_embedding("orphan_doc.md::chunk_0", "doc_chunk", &orphan_vec)
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let store = make_test_store(tmp.path());
    // Store is empty — no docs/chunks

    let empty: Vec<&Chunk> = vec![];
    let no_changes: Vec<&str> = vec![];
    infigraph_docs::embed::update_doc_embeddings_remote(&store, &pg, &empty, &no_changes).unwrap();

    // Orphan should be cleaned up
    let result = pg.get_embedding("orphan_doc.md::chunk_0").unwrap();
    assert!(result.is_none(), "orphan embedding should be deleted");
}
