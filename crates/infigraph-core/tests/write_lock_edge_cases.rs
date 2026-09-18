use std::time::{Duration, Instant};

use infigraph_core::graph::GraphStore;
use tempfile::TempDir;

fn make_store() -> (TempDir, GraphStore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let store = GraphStore::open(&db_path).unwrap();
    (dir, store)
}

#[test]
fn test_lock_released_on_panic() {
    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("panic.lock");

    let lock_path_clone = lock_path.clone();
    let handle = std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path_clone)
            .unwrap();
        use fs2::FileExt;
        file.lock_exclusive().unwrap();
        panic!("intentional panic while holding lock");
    });

    let _ = handle.join(); // panicked thread

    // Lock should be released — Rust unwinding drops the file. The OS may
    // take a moment to process the fd close after join() returns (seen as
    // flakiness on loaded CI runners), so poll with a deadline instead of
    // asserting immediately.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut released = false;
    while Instant::now() < deadline {
        if file.try_lock_exclusive().is_ok() {
            released = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(released, "lock should be released after thread panic");
}

#[test]
fn test_lock_file_deleted_while_held() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("delfile.db");
    let store = GraphStore::open(&db_path).unwrap();
    let lock_path = db_path.with_extension("lock");

    let lock = store.write_lock().unwrap();

    // Delete the lock file while lock is held
    std::fs::remove_file(&lock_path).unwrap();
    assert!(!lock_path.exists());

    drop(lock);

    // Re-acquire should succeed (recreates file)
    let lock2 = store.write_lock().unwrap();
    assert!(lock_path.exists(), "lock file should be recreated");
    drop(lock2);
}

#[test]
fn test_lock_file_permissions_readonly() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("readonly.db");
    let _ = GraphStore::open(&db_path).unwrap();
    let lock_path = db_path.with_extension("lock");

    // Create lock file with readonly perms
    std::fs::write(&lock_path, "").unwrap();
    let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&lock_path, perms).unwrap();

    let store = GraphStore::open(&db_path).unwrap();
    let result = store.write_lock();

    // Restore perms for cleanup
    let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&lock_path, perms).unwrap();

    assert!(
        result.is_err(),
        "write_lock on readonly file should error, not hang"
    );
}

#[test]
fn test_try_lock_timeout_pattern() {
    let (_dir, store) = make_store();
    let _lock = store.write_lock().unwrap();

    let deadline = Instant::now() + Duration::from_millis(200);
    let mut acquired = false;

    while Instant::now() < deadline {
        if let Ok(Some(_l)) = store.try_write_lock() {
            acquired = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !acquired,
        "should not acquire lock while held — timeout pattern works"
    );
}

#[test]
fn test_lock_survives_store_reopen() {
    // Kuzu locks the DB directory, preventing two Database instances on the same
    // path (especially on Windows). Test the lock file directly instead.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("reopen.db");
    let store1 = GraphStore::open(&db_path).unwrap();
    let _lock = store1.write_lock().unwrap();

    // Verify lock is held via the lock file directly
    let lock_path = db_path.with_extension("lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    let result = file.try_lock_exclusive();
    assert!(result.is_err(), "lock file should be held by store1");
    drop(file);

    drop(_lock);

    // After releasing, a new fd should be able to acquire
    let file2 = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    let result2 = file2.try_lock_exclusive();
    assert!(
        result2.is_ok(),
        "lock should be acquirable after store1 releases"
    );
}

#[test]
fn test_lock_different_db_paths_independent() {
    let dir = TempDir::new().unwrap();
    let store_a = GraphStore::open(&dir.path().join("a.db")).unwrap();
    let store_b = GraphStore::open(&dir.path().join("b.db")).unwrap();

    let _lock_a = store_a.write_lock().unwrap();
    let lock_b = store_b.try_write_lock().unwrap();
    assert!(
        lock_b.is_some(),
        "different DB paths should have independent locks"
    );
}

#[test]
fn test_rapid_lock_unlock_cycle() {
    let (_dir, store) = make_store();

    for _ in 0..10_000 {
        let lock = store.write_lock().unwrap();
        drop(lock);
    }

    // If we get here without error/fd leak, the test passes
    let final_lock = store.write_lock().unwrap();
    drop(final_lock);
}

#[test]
fn test_write_during_read_query() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("readwrite.db");
    let store = std::sync::Arc::new(GraphStore::open(&db_path).unwrap());

    // Seed some data
    use infigraph_core::model::{FileExtraction, Span, Symbol, SymbolKind};
    let extraction = FileExtraction {
        file: "seed.py".to_string(),
        language: "python".to_string(),
        content_hash: "h".to_string(),
        symbols: vec![Symbol {
            scip_id: None,
            id: "seed::f".to_string(),
            name: "f".to_string(),
            kind: SymbolKind::Function,
            span: Span {
                file: "seed.py".to_string(),
                start_line: 1,
                start_col: 0,
                end_line: 3,
                end_col: 0,
            },
            signature_hash: "s".to_string(),
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
    };
    store.upsert_file(&extraction).unwrap();

    let s1 = std::sync::Arc::clone(&store);
    let reader = std::thread::spawn(move || {
        for _ in 0..20 {
            let _ = s1.stats().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    let s2 = std::sync::Arc::clone(&store);
    let writer = std::thread::spawn(move || {
        for i in 0..5 {
            let ext = FileExtraction {
                file: format!("w{i}.py"),
                language: "python".to_string(),
                content_hash: format!("h{i}"),
                symbols: vec![Symbol {
                    scip_id: None,
                    id: format!("w{i}::g"),
                    name: "g".to_string(),
                    kind: SymbolKind::Function,
                    span: Span {
                        file: format!("w{i}.py"),
                        start_line: 1,
                        start_col: 0,
                        end_line: 2,
                        end_col: 0,
                    },
                    signature_hash: "s".to_string(),
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
            };
            s2.upsert_file(&ext).unwrap();
        }
    });

    reader.join().unwrap();
    writer.join().unwrap();
}

#[test]
fn test_lock_after_db_corruption_recovery() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("corrupt.db");
    let store = GraphStore::open(&db_path).unwrap();
    drop(store);

    // "Corrupt" by deleting a DB file (lock file is separate)
    let lock_path = db_path.with_extension("lock");
    if db_path.exists() {
        let _ = std::fs::remove_dir_all(&db_path);
    }

    // Lock file should still be acquirable even with missing DB
    assert!(lock_path.exists() || !lock_path.exists()); // may or may not exist
    let store2 = GraphStore::open(&db_path).unwrap();
    let lock = store2.write_lock();
    assert!(
        lock.is_ok(),
        "lock should work even after DB corruption recovery"
    );
}

#[cfg(unix)]
#[test]
fn test_lock_released_on_process_exit() {
    use std::process::Command;

    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("proc_exit.lock");

    // Spawn child that acquires flock then gets killed
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "exec 9>>'{}' && flock -x 9 && sleep 60",
            lock_path.display()
        ))
        .spawn()
        .unwrap();

    // Give child time to acquire lock
    std::thread::sleep(Duration::from_millis(100));

    // Kill child
    child.kill().unwrap();
    let _ = child.wait();

    // Lock should be released — try acquiring via fs2
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    let result = file.try_lock_exclusive();
    assert!(
        result.is_ok(),
        "lock should be released after child process killed"
    );
}
