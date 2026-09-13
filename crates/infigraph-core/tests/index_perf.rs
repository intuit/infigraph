use std::time::{Duration, Instant};

use infigraph_core::extract::extract_file;
use infigraph_core::graph::GraphStore;
use infigraph_core::lang::{LanguagePack, LanguageRegistry};
use infigraph_core::model::FileExtraction;
use infigraph_core::Infigraph;
use tempfile::TempDir;

const PYTHON_ENTITIES: &str = r#"
(module
  (function_definition
    name: (identifier) @func.name
    body: (block
      (expression_statement
        (string) @func.docstring)?)) @func.def)

(module
  (decorated_definition
    (decorator) @func.decorator
    definition: (function_definition
      name: (identifier) @func.name
      body: (block
        (expression_statement
          (string) @func.docstring)?)) @func.def))

(class_definition
  name: (identifier) @class.name
  body: (block
    (expression_statement
      (string) @class.docstring)?)) @class.def

(class_definition
  body: (block
    (function_definition
      name: (identifier) @method.name
      body: (block
        (expression_statement
          (string) @method.docstring)?)) @method.def))

(class_definition
  body: (block
    (decorated_definition
      (decorator) @method.decorator
      definition: (function_definition
        name: (identifier) @method.name
        body: (block
          (expression_statement
            (string) @method.docstring)?)) @method.def)))

(module
  (function_definition
    name: (identifier) @test.name
    (#match? @test.name "^test_")
    body: (block
      (expression_statement
        (string) @test.docstring)?)) @test.def)

(module
  (expression_statement
    (assignment
      left: (identifier) @var.name)) @var.def)
"#;

const PYTHON_RELATIONS: &str = r#"
(call
  function: (identifier) @call.func) @call.site

(call
  function: (attribute
    object: (_) @call.receiver
    attribute: (identifier) @call.func)) @call.site

(import_statement
  name: (dotted_name) @import.module)

(import_from_statement
  module_name: (dotted_name) @import.module)

(class_definition
  name: (identifier) @inherit.child
  superclasses: (argument_list
    (identifier) @inherit.parent))
"#;

fn python_pack() -> LanguagePack {
    let grammar = tree_sitter_python::LANGUAGE.into();
    LanguagePack::new(
        "python",
        vec![".py"],
        grammar,
        PYTHON_ENTITIES,
        PYTHON_RELATIONS,
    )
    .unwrap()
}

fn python_registry() -> LanguageRegistry {
    let mut reg = LanguageRegistry::new();
    reg.register(python_pack());
    reg
}

fn generate_python_source(file_idx: usize) -> Vec<u8> {
    let mut src = String::new();
    src.push_str("import os\nimport sys\nfrom pathlib import Path\n\n");
    src.push_str(&format!(
        "MAX_RETRIES_{} = 3\nDEBUG_{} = True\n\n",
        file_idx, file_idx
    ));
    src.push_str(&format!(
        "class Service{}:\n    \"\"\"Service number {}.\"\"\"\n",
        file_idx, file_idx
    ));
    for m in 0..5 {
        src.push_str(&format!(
            "    def method_{}(self, arg):\n        \"\"\"Method {}.\"\"\"\n        if arg > 0:\n            for i in range(arg):\n                self.helper_{}(i)\n        return arg * 2\n\n",
            m, m, m
        ));
    }
    for f in 0..3 {
        src.push_str(&format!(
            "def utility_{}_{}(x, y):\n    result = x + y\n    os.path.join(str(result), 'out')\n    return result\n\n",
            file_idx, f
        ));
    }
    src.push_str(&format!(
        "def test_service_{}():\n    svc = Service{}()\n    svc.method_0(42)\n    assert True\n",
        file_idx, file_idx
    ));
    src.into_bytes()
}

fn generate_extractions(count: usize) -> Vec<FileExtraction> {
    let pack = python_pack();
    (0..count)
        .map(|i| {
            let path = format!("src/module_{}.py", i);
            let source = generate_python_source(i);
            extract_file(&path, &source, &pack).unwrap()
        })
        .collect()
}

fn make_store() -> (TempDir, GraphStore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let store = GraphStore::open(&db_path).unwrap();
    (dir, store)
}

/// Create a temp project dir with N Python files, return (dir, Infigraph).
fn make_project(file_count: usize) -> (TempDir, Infigraph) {
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    for i in 0..file_count {
        let path = src_dir.join(format!("module_{}.py", i));
        std::fs::write(&path, generate_python_source(i)).unwrap();
    }

    let registry = python_registry();
    let mut ig = Infigraph::open(dir.path(), registry).unwrap();
    ig.init().unwrap();
    (dir, ig)
}

// ---------- Component-level benchmarks ----------

#[test]
fn test_extract_file_throughput() {
    let pack = python_pack();
    let sources: Vec<(String, Vec<u8>)> = (0..100)
        .map(|i| (format!("src/mod_{}.py", i), generate_python_source(i)))
        .collect();

    let start = Instant::now();
    for (path, src) in &sources {
        let _ = extract_file(path, src, &pack).unwrap();
    }
    let elapsed = start.elapsed();
    let per_file = elapsed / 100;

    assert!(
        per_file < Duration::from_millis(50),
        "extract_file should be <50ms/file, got {:?}/file ({:?} total for 100 files)",
        per_file,
        elapsed
    );
}

#[test]
fn test_parallel_extract_completes() {
    use rayon::prelude::*;

    let sources: Vec<(String, Vec<u8>)> = (0..50)
        .map(|i| (format!("src/par_{}.py", i), generate_python_source(i)))
        .collect();

    let start = Instant::now();
    let results: Vec<FileExtraction> = sources
        .par_iter()
        .map(|(path, src)| {
            let local_pack = python_pack();
            extract_file(path, src, &local_pack).unwrap()
        })
        .collect();
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 50);
    for ext in &results {
        assert!(!ext.symbols.is_empty(), "each file should have symbols");
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "parallel extract of 50 files took {:?}, expected <10s",
        elapsed
    );
}

#[test]
fn test_upsert_all_bulk_throughput() {
    let extractions = generate_extractions(50);
    let (_dir, store) = make_store();
    let conn = store.connection().unwrap();

    let _lock = store.write_lock().unwrap();
    let start = Instant::now();
    store.upsert_all_bulk(&conn, &extractions).unwrap();
    let elapsed = start.elapsed();

    let per_file = elapsed / 50;
    assert!(
        per_file < Duration::from_millis(100),
        "bulk upsert should be <100ms/file, got {:?}/file ({:?} total for 50 files)",
        per_file,
        elapsed
    );
}

#[test]
fn test_upsert_all_parquet_throughput() {
    let extractions = generate_extractions(150);
    let (_dir, store) = make_store();

    let start = Instant::now();
    store.upsert_all_parquet(&extractions).unwrap();
    let elapsed = start.elapsed();

    let per_file = elapsed / 150;
    assert!(
        per_file < Duration::from_millis(50),
        "parquet upsert should be <50ms/file, got {:?}/file ({:?} total for 150 files)",
        per_file,
        elapsed
    );
}

// ---------- End-to-end Infigraph::index() benchmarks ----------

#[test]
fn test_full_index_throughput() {
    let (_dir, ig) = make_project(50);

    let start = Instant::now();
    let result = ig.index().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.indexed_files, 50);
    assert_eq!(result.total_files, 50);
    let per_file = elapsed / 50;
    assert!(
        per_file < Duration::from_millis(200),
        "full index should be <200ms/file, got {:?}/file ({:?} total for 50 files)",
        per_file,
        elapsed
    );
    eprintln!(
        "[perf] full index: 50 files in {:?} ({:?}/file)",
        elapsed, per_file
    );
}

#[test]
#[ignore] // timing assertion flaky on CI — run via pre-commit hook
fn test_incremental_index_skips_unchanged() {
    let (_dir, ig) = make_project(30);

    // Full index
    let full_start = Instant::now();
    let full_result = ig.index().unwrap();
    let full_elapsed = full_start.elapsed();
    assert_eq!(full_result.indexed_files, 30);

    // Re-index with no changes — should skip all files
    let incr_start = Instant::now();
    let incr_result = ig.index().unwrap();
    let incr_elapsed = incr_start.elapsed();
    assert_eq!(
        incr_result.indexed_files, 0,
        "no files changed, should skip all"
    );
    assert_eq!(incr_result.total_files, 30);

    // Incremental no-op should be much faster than full index
    assert!(
        incr_elapsed < full_elapsed / 2,
        "incremental no-op should be >2x faster: full={:?}, incr={:?}",
        full_elapsed,
        incr_elapsed
    );
    eprintln!(
        "[perf] full={:?}, incremental no-op={:?} ({}x faster)",
        full_elapsed,
        incr_elapsed,
        full_elapsed
            .as_millis()
            .checked_div(incr_elapsed.as_millis().max(1))
            .unwrap_or(0)
    );
}

#[test]
fn test_incremental_index_only_changed() {
    let (dir, ig) = make_project(30);

    // Full index
    ig.index().unwrap();

    // Modify 3 files
    for i in 0..3 {
        let path = dir.path().join("src").join(format!("module_{}.py", i));
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(&format!("\ndef added_func_{}():\n    return {}\n", i, i));
        std::fs::write(&path, content).unwrap();
    }

    // Incremental index — should only process 3 changed files
    let start = Instant::now();
    let result = ig.index().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.indexed_files, 3, "only 3 files changed");
    assert_eq!(result.total_files, 30);
    eprintln!(
        "[perf] incremental 3/30 files: {:?} ({:?}/changed file)",
        elapsed,
        elapsed / 3
    );
}

/// Upgrading an extractor must reach existing indexes on its own.
///
/// Before `content_hash` covered the language pack, a query fix never re-ran on
/// unedited files: their content hash still matched, so they were skipped
/// indefinitely and the graph kept whatever the old, buggy extractor produced —
/// silently, unless the user knew to run `index --full`.
#[test]
fn test_extractor_change_reindexes_unedited_files() {
    let (dir, ig) = make_project(5);
    assert_eq!(ig.index().unwrap().indexed_files, 5, "first index is full");
    // Kuzu is single-writer: release the graph before reopening it.
    drop(ig);

    let reindex_with = |pack: LanguagePack| {
        let mut reg = LanguageRegistry::new();
        reg.register(pack);
        let mut ig = Infigraph::open(dir.path(), reg).unwrap();
        ig.init().unwrap();
        let indexed = ig.index().unwrap().indexed_files;
        drop(ig);
        indexed
    };

    assert_eq!(
        reindex_with(python_pack()),
        0,
        "same files and same extractor must still skip everything"
    );

    // Stand-in for shipping an extractor fix: one more pattern in entities.scm.
    let upgraded = || {
        LanguagePack::new(
            "python",
            vec![".py"],
            tree_sitter_python::LANGUAGE.into(),
            &format!("{PYTHON_ENTITIES}\n(lambda) @func.def\n"),
            PYTHON_RELATIONS,
        )
        .unwrap()
    };

    assert_eq!(
        reindex_with(upgraded()),
        5,
        "an upgraded extractor must re-extract its files without --full"
    );
    assert_eq!(
        reindex_with(upgraded()),
        0,
        "re-extraction must settle: the upgraded extractor is now the stored \
         fingerprint, so a further run skips again rather than reindexing forever"
    );
}

#[test]
fn test_large_project_index() {
    let (_dir, ig) = make_project(200);

    let start = Instant::now();
    let result = ig.index().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.indexed_files, 200);
    let per_file = elapsed / 200;
    // 200 files triggers parquet path (threshold > 100)
    assert!(
        elapsed < Duration::from_secs(120),
        "200-file index should complete within 120s, took {:?}",
        elapsed
    );
    eprintln!(
        "[perf] large index: 200 files in {:?} ({:?}/file, parquet path)",
        elapsed, per_file
    );
}
