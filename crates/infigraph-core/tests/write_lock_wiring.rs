use std::sync::Arc;
use std::time::{Duration, Instant};

use infigraph_core::graph::GraphStore;
use infigraph_core::model::{FileExtraction, Span, Symbol, SymbolKind};
use tempfile::TempDir;

fn make_store() -> (TempDir, GraphStore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let store = GraphStore::open(&db_path).unwrap();
    (dir, store)
}

fn make_extraction(file: &str) -> FileExtraction {
    FileExtraction {
        file: file.to_string(),
        language: "python".to_string(),
        content_hash: format!("hash_{file}"),
        symbols: vec![Symbol {
            scip_id: None,
            id: format!("{file}::func"),
            name: "func".to_string(),
            kind: SymbolKind::Function,
            span: Span {
                file: file.to_string(),
                start_line: 1,
                start_col: 0,
                end_line: 3,
                end_col: 0,
            },
            signature_hash: "sig1".to_string(),
            parent: None,
            language: "python".to_string(),
            visibility: None,
            docstring: None,
            complexity: 1,
            parameters: None,
            return_type: None,
        }],
        relations: vec![],
        statements: vec![],
    }
}

#[test]
fn test_upsert_file_concurrent_correctness() {
    let (dir, store) = make_store();
    let _ = dir; // keep TempDir alive
    let store = Arc::new(store);

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                let file = format!("file{i}.py");
                let extraction = make_extraction(&file);
                s.upsert_file(&extraction).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let conn = store.connection().unwrap();
    let mut result = conn.query("MATCH (f:File) RETURN count(f)").unwrap();
    let count: u64 = result
        .next()
        .unwrap()
        .first()
        .unwrap()
        .to_string()
        .parse()
        .unwrap();
    assert_eq!(count, 4, "expected 4 files after concurrent upserts");
}

#[test]
fn test_remove_file_concurrent_correctness() {
    let (dir, store) = make_store();
    let _ = dir;
    let store = Arc::new(store);

    // Insert 4 files sequentially
    for i in 0..4 {
        let file = format!("file{i}.py");
        store.upsert_file(&make_extraction(&file)).unwrap();
    }

    // Verify all 4 exist
    let stats = store.stats().unwrap();
    assert_eq!(stats.files, 4);

    // Remove concurrently
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                s.remove_file(&format!("file{i}.py")).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let conn = store.connection().unwrap();
    let mut result = conn.query("MATCH (f:File) RETURN count(f)").unwrap();
    let count: u64 = result
        .next()
        .unwrap()
        .first()
        .unwrap()
        .to_string()
        .parse()
        .unwrap();
    assert_eq!(count, 0, "expected 0 files after concurrent removes");
}

#[test]
fn test_upsert_file_blocks_when_locked() {
    let (dir, store) = make_store();
    let _ = dir;
    let store = Arc::new(store);

    // Hold write lock on main thread
    let lock = store.write_lock().unwrap();

    let s = Arc::clone(&store);
    let handle = std::thread::spawn(move || {
        let start = Instant::now();
        let extraction = make_extraction("blocked.py");
        s.upsert_file(&extraction).unwrap();
        start.elapsed()
    });

    // Sleep while holding lock — the spawned thread should block
    std::thread::sleep(Duration::from_millis(100));
    drop(lock);

    let elapsed = handle.join().unwrap();
    assert!(
        elapsed >= Duration::from_millis(90),
        "upsert_file should have blocked ~100ms, but took {:?}",
        elapsed
    );
}

#[test]
fn test_try_write_lock_none_while_held() {
    let (_dir, store) = make_store();
    let _lock = store.write_lock().unwrap();
    let result = store.try_write_lock().unwrap();
    assert!(
        result.is_none(),
        "try_write_lock should return None while lock is held"
    );
}

#[test]
fn test_mixed_read_write_concurrent() {
    let (dir, store) = make_store();
    let _ = dir;
    let store = Arc::new(store);

    // Seed 2 files so readers have something to read
    store.upsert_file(&make_extraction("seed0.py")).unwrap();
    store.upsert_file(&make_extraction("seed1.py")).unwrap();

    let mut handles = Vec::new();

    // 2 writer threads — each upserts a unique file 10 times (idempotent)
    for writer_id in 0..2 {
        let s = Arc::clone(&store);
        handles.push(std::thread::spawn(move || {
            for iter in 0..10 {
                let file = format!("writer{writer_id}_iter{iter}.py");
                s.upsert_file(&make_extraction(&file)).unwrap();
            }
        }));
    }

    // 2 reader threads — each calls stats() 10 times
    for _ in 0..2 {
        let s = Arc::clone(&store);
        handles.push(std::thread::spawn(move || {
            for _ in 0..10 {
                let _stats = s.stats().unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // 2 seed files + 2 writers * 10 unique files = 22
    let stats = store.stats().unwrap();
    assert_eq!(
        stats.files, 22,
        "expected 22 files (2 seed + 20 written), got {}",
        stats.files
    );
}
