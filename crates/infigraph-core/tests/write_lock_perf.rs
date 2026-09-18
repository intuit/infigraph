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
#[ignore] // perf test — run via pre-commit hook, not CI
fn test_lock_overhead_under_1ms() {
    let (_dir, store) = make_store();

    // Warm up: the very first locks pay one-time init/page-fault costs that
    // aren't representative of steady-state overhead.
    for _ in 0..100 {
        drop(store.write_lock().unwrap());
    }

    let start = Instant::now();
    let iterations = 1000;
    for _ in 0..iterations {
        let lock = store.write_lock().unwrap();
        drop(lock);
    }
    let elapsed = start.elapsed();
    let avg = elapsed / iterations;
    // An in-memory advisory lock/unlock is sub-microsecond; this guards against
    // a *gross* regression (e.g. accidentally adding disk I/O or a blocking
    // syscall to the lock path), not micro-jitter. The threshold is deliberately
    // loose (~1000x the real cost) so a loaded dev machine's scheduler jitter
    // doesn't flake it while a genuine 100x+ regression still trips it. The old
    // <1ms wall flaked constantly under machine load (AIF3X-331 #25).
    assert!(
        avg < Duration::from_millis(5),
        "avg lock/unlock should be well under 5ms (regression guard), got {:?}",
        avg
    );
}

#[test]
#[ignore] // perf test — run via pre-commit hook, not CI
fn test_contended_lock_throughput() {
    let (_dir, store) = make_store();
    let store = Arc::new(store);

    // Single-thread baseline
    let start = Instant::now();
    for _ in 0..100 {
        let lock = store.write_lock().unwrap();
        std::hint::black_box(&lock);
        drop(lock);
    }
    let single_thread = start.elapsed();

    // 4 threads contending
    let start = Instant::now();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                for _ in 0..100 {
                    let lock = s.write_lock().unwrap();
                    std::hint::black_box(&lock);
                    drop(lock);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let multi_thread = start.elapsed();

    // 4 threads doing 4x total work — should be < 8x single-thread time
    assert!(
        multi_thread < single_thread * 8,
        "contended throughput too slow: single={:?}, 4-thread={:?}",
        single_thread,
        multi_thread
    );
}

#[test]
#[ignore] // perf test — run via pre-commit hook, not CI
fn test_no_perf_regression_upsert_file() {
    let (_dir, store) = make_store();

    let warmup = make_extraction("warmup.py");
    store.upsert_file(&warmup).unwrap();

    let mut times = Vec::new();
    for i in 0..20 {
        let ext = make_extraction(&format!("perf{i}.py"));
        let start = Instant::now();
        store.upsert_file(&ext).unwrap();
        times.push(start.elapsed());
    }

    let avg = times.iter().sum::<Duration>() / times.len() as u32;
    // Lock overhead is <1ms; upsert is typically 5-50ms.
    // If avg > 200ms something is very wrong.
    assert!(
        avg < Duration::from_millis(200),
        "avg upsert_file too slow: {:?} (lock overhead should be negligible)",
        avg
    );
}
