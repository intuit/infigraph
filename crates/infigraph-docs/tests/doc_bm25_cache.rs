use infigraph_core::embed::{load_embeddings, save_embeddings};
use infigraph_docs::chunk::{chunk_document, ChunkStrategy};
use infigraph_docs::extract::{DocFormat, ExtractedDoc};
use infigraph_docs::store::DocStore;

fn index_doc(root: &std::path::Path, file: &str, text: &str) {
    let full_path = root.join(file);
    std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
    std::fs::write(&full_path, text).unwrap();

    let doc = ExtractedDoc {
        file: file.to_string(),
        title: Some(file.to_string()),
        content_hash: format!("hash-{file}"),
        format: DocFormat::Markdown,
        text: text.to_string(),
        page_count: None,
    };
    let chunks = chunk_document(&doc, file, &doc.content_hash, ChunkStrategy::HeadingBounded);
    let chunk_refs: Vec<_> = chunks.iter().collect();

    let db_path = root.join(".infigraph").join("docs.kuzu");
    let store = DocStore::open(&db_path).unwrap();
    store.upsert_all_parquet(&[&doc], &chunk_refs).unwrap();
    drop(store);

    let embeddings_path = root.join(".infigraph").join("docs_embeddings.bin");
    let mut embeddings = if embeddings_path.exists() {
        load_embeddings(&embeddings_path).unwrap()
    } else {
        Vec::new()
    };
    embeddings.extend(
        chunks
            .iter()
            .map(|chunk| (chunk.id.clone(), vec![0.1; 384]))
            .collect::<Vec<_>>(),
    );
    save_embeddings(&embeddings_path, &embeddings).unwrap();
}

fn open_store(root: &std::path::Path) -> DocStore {
    DocStore::open(&root.join(".infigraph").join("docs.kuzu")).unwrap()
}

#[test]
fn first_search_writes_cache_and_results_stay_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_doc(
        root,
        "docs/a.md",
        "# Alpha\nkuzu graph database embedded columnar",
    );
    index_doc(
        root,
        "docs/b.md",
        "# Beta\nhybrid bm25 vector search ranking",
    );

    let store = open_store(root);
    let artifact_dir = root.join(".infigraph");
    let cache_path = artifact_dir.join("docs_bm25_cache.bin");

    // alpha 0.0 = pure BM25: deterministic, independent of the embedder.
    let first = infigraph_docs::search::hybrid_doc_search_in_dir(
        "vector search",
        &store,
        &artifact_dir,
        5,
        0.0,
    )
    .unwrap();
    assert!(!first.is_empty());
    assert!(cache_path.exists(), "first search should write the cache");

    let second = infigraph_docs::search::hybrid_doc_search_in_dir(
        "vector search",
        &store,
        &artifact_dir,
        5,
        0.0,
    )
    .unwrap();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(a.chunk_id, b.chunk_id);
        assert!((a.score - b.score).abs() < 1e-6);
    }
}

#[test]
fn corrupt_cache_falls_back_and_is_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_doc(
        root,
        "docs/a.md",
        "# Alpha\nkuzu graph database embedded columnar",
    );

    let store = open_store(root);
    let artifact_dir = root.join(".infigraph");
    let cache_path = artifact_dir.join("docs_bm25_cache.bin");

    infigraph_docs::search::hybrid_doc_search_in_dir("kuzu", &store, &artifact_dir, 5, 0.0)
        .unwrap();
    assert!(cache_path.exists());

    // Corrupt it. fs::write also bumps its mtime, so it now looks "fresh" —
    // the corrupt content is genuinely attempted, must not panic or error.
    std::fs::write(&cache_path, b"garbage").unwrap();
    let res =
        infigraph_docs::search::hybrid_doc_search_in_dir("kuzu", &store, &artifact_dir, 5, 0.0)
            .unwrap();
    assert!(!res.is_empty(), "search must survive a corrupt cache");
    assert!(
        infigraph_docs::search::DocBM25Index::load(&cache_path).is_ok(),
        "corrupt cache should have been rebuilt with valid content"
    );
}

#[test]
fn stale_cache_is_rebuilt_when_embeddings_are_newer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_doc(
        root,
        "docs/a.md",
        "# Alpha\nkuzu graph database embedded columnar",
    );

    let store = open_store(root);
    let artifact_dir = root.join(".infigraph");
    let cache_path = artifact_dir.join("docs_bm25_cache.bin");
    let emb_path = artifact_dir.join("docs_embeddings.bin");

    infigraph_docs::search::hybrid_doc_search_in_dir("kuzu", &store, &artifact_dir, 5, 0.0)
        .unwrap();
    let cache_mtime_before = std::fs::metadata(&cache_path).unwrap().modified().unwrap();

    // Make the anchor newer than the cache (same technique as
    // bm25_cache_stale_when_embeddings_newer in infigraph-cli).
    std::thread::sleep(std::time::Duration::from_millis(50));
    let emb_bytes = std::fs::read(&emb_path).unwrap();
    std::fs::write(&emb_path, &emb_bytes).unwrap();

    infigraph_docs::search::hybrid_doc_search_in_dir("kuzu", &store, &artifact_dir, 5, 0.0)
        .unwrap();
    let cache_mtime_after = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
    assert!(
        cache_mtime_after > cache_mtime_before,
        "stale cache should have been rebuilt (mtime advanced)"
    );
}
