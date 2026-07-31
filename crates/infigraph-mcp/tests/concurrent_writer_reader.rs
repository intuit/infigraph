//! Deterministic (not incidental/ambient) reproduction of a concurrency question
//! left open by the paused lbug 0.16.0 -> 0.18.3 version-bump investigation:
//! does a read-only query return CORRECT data (or fail cleanly) when a writer
//! is ACTIVELY reindexing concurrently, the way `infigraph watch` holds one
//! connection open and reindexes on every file-change batch
//! (crates/infigraph-core/src/watch/mod.rs)?
//!
//! Goes directly through `KuzuBackend::open_read_only` + `get_symbols_for_search`
//! (`MATCH (s:Symbol) RETURN ...`) -- the exact query at the heart of the
//! original finding (crates/infigraph-mcp/src/tools/search.rs's
//! `get_search_data_local` calls this to build BM25/vector search) -- rather
//! than through the full `search` MCP tool, whose BM25/embeddings rebuild cost
//! made two earlier attempts too slow to get enough concurrent read samples
//! (4 samples in 8s of wall-clock). This version does hundreds of reads per
//! second by skipping that layer entirely.
//!
//! One file (`mod_stable.py`) is indexed once and never touched again -- its
//! symbol's presence is guaranteed data-wise for the whole test, so any read
//! failure to find it while the writer churns a SEPARATE file can only be a
//! concurrent-access artifact, not a fixture bug.

use infigraph_core::graph::{GraphBackend, KuzuBackend};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn write_fixture(dir: &std::path::Path, filename: &str, content: &str) {
    std::fs::write(dir.join(filename), content).expect("write fixture file");
}

fn index_full(prism: &mut infigraph_core::Infigraph) {
    prism.index().expect("index");
}

#[test]
fn concurrent_writer_reader_raw_query_correctness_under_load() {
    let project = tempfile::tempdir().expect("project tmpdir");
    let root = project.path().to_path_buf();
    let db_path = root.join(".infigraph").join("graph");

    write_fixture(&root, "mod_stable.py", "def stable_marker_fn():\n    pass\n");
    write_fixture(&root, "mod_churn.py", "def churn_fn_0():\n    pass\n");
    {
        let registry = infigraph_languages::bundled_registry().expect("bundled registry");
        let mut prism = infigraph_core::Infigraph::open(&root, registry).expect("open initial");
        prism.init().expect("init initial");
        index_full(&mut prism);
    }

    // Control: confirm the stable symbol is findable via the exact same
    // direct-query path, with zero concurrent activity.
    {
        let reader = KuzuBackend::open_read_only(&db_path).expect("control open_read_only");
        let rows = reader.get_symbols_for_search().expect("control query");
        assert!(
            rows.iter().any(|r| r[1] == "stable_marker_fn"),
            "control case (no concurrent writer) should find the indexed symbol: {rows:?}"
        );
    }

    // --- Writer thread: opens ONE connection and keeps it alive for the
    // thread's whole life (matching watch_db's held-open-connection design),
    // repeatedly overwriting the SAME churn file (keeps corpus size, and so
    // per-iteration write cost, constant) so many fast iterations fit in a
    // short window. ---
    let writer_root = root.clone();
    let writer_done = std::sync::Arc::new(AtomicBool::new(false));
    let writer_done_flag = std::sync::Arc::clone(&writer_done);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        let registry = infigraph_languages::bundled_registry().expect("bundled registry writer");
        let mut prism =
            infigraph_core::Infigraph::open(&writer_root, registry).expect("open writer");
        prism.init().expect("init writer");
        let _ = ready_tx.send(());
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        let mut i = 0usize;
        while std::time::Instant::now() < deadline {
            i += 1;
            write_fixture(
                &writer_root,
                "mod_churn.py",
                &format!("def churn_fn_{i}():\n    pass\n"),
            );
            index_full(&mut prism);
        }
        writer_done_flag.store(true, Ordering::Relaxed);
        // `prism` (and its underlying KuzuBackend connection) stays alive
        // until this closure returns -- matching a live watcher.
    });

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("writer failed to signal ready");

    // --- Reader: fire direct read-only queries in a tight loop WHILE the
    // writer thread above is actively looping through reindex batches. ---
    let correct = AtomicUsize::new(0);
    let clean_errors = AtomicUsize::new(0);
    let wrong_results: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let error_samples: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    let mut attempts = 0usize;
    while !writer_done.load(Ordering::Relaxed) {
        attempts += 1;
        match KuzuBackend::open_read_only(&db_path) {
            Ok(reader) => match reader.get_symbols_for_search() {
                Ok(rows) if rows.iter().any(|r| r[1] == "stable_marker_fn") => {
                    correct.fetch_add(1, Ordering::Relaxed);
                }
                Ok(rows) => {
                    wrong_results
                        .lock()
                        .unwrap()
                        .push(format!("query succeeded but missing stable_marker_fn: {rows:?}"));
                }
                Err(e) => {
                    clean_errors.fetch_add(1, Ordering::Relaxed);
                    error_samples.lock().unwrap().push(format!("query error: {e}"));
                }
            },
            Err(e) => {
                clean_errors.fetch_add(1, Ordering::Relaxed);
                error_samples.lock().unwrap().push(format!("open error: {e}"));
            }
        }
    }

    writer.join().expect("writer thread panicked");

    let correct = correct.load(Ordering::Relaxed);
    let clean_errors = clean_errors.load(Ordering::Relaxed);
    let wrong = wrong_results.lock().unwrap();

    println!(
        "RESULT: {attempts} concurrent read attempts while writer actively reindexed -- \
         {correct} correct, {clean_errors} clean errors, {} silently wrong",
        wrong.len()
    );
    for sample in error_samples.lock().unwrap().iter().take(3) {
        println!("  error sample: {sample}");
    }

    assert!(
        wrong.is_empty(),
        "BUG REPRODUCED: {} of {attempts} concurrent read-only queries succeeded (no error) but \
         returned INCORRECT/INCOMPLETE data (missing 'stable_marker_fn', which is guaranteed \
         present and never touched by the writer) while a writer was actively reindexing \
         concurrently. This is the silent-partial-results failure mode -- a lock conflict should \
         either be transparent (correct data) or fail loudly (a clean error), never silently \
         wrong. First bad result:\n{}",
        wrong.len(),
        wrong.first().cloned().unwrap_or_default()
    );

    assert!(
        correct + clean_errors == attempts,
        "accounting mismatch: correct={correct} clean_errors={clean_errors} attempts={attempts}"
    );
}
