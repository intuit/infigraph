use infigraph_core::embed::{load_embeddings, save_embeddings};

#[test]
fn corrupt_counts_are_rejected_before_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("embeddings.bin");
    for bytes in [
        b"corrupt".as_slice(),
        &u32::MAX.to_le_bytes(),
        &[1, 0, 0, 0, 0, 0, 0, 0],
    ] {
        std::fs::write(&path, bytes).unwrap();
        let error = load_embeddings(&path).unwrap_err();
        assert!(error.to_string().contains("truncated embeddings file"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn empty_ids_and_vectors_still_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("embeddings.bin");
    for entries in [
        vec![],
        vec![(String::new(), vec![])],
        vec![("symbol".into(), vec![0.25, -0.5])],
    ] {
        save_embeddings(&path, &entries).unwrap();
        assert_eq!(load_embeddings(&path).unwrap(), entries);
    }
}
