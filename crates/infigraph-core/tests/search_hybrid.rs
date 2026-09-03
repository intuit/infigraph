use std::collections::HashMap;
use std::io::Write;

use infigraph_core::embed::EmbedProvider;
use infigraph_core::search::{self, BM25Index, RawScores};

// ---------- Mock embedder ----------

struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl EmbedProvider for MockEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for (i, b) in t.bytes().enumerate() {
                    v[i % self.dim] += b as f32 / 255.0;
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(0.001);
                v.iter_mut().for_each(|x| *x /= norm);
                v
            })
            .collect())
    }
}

// ---------- BM25 tests ----------

#[test]
fn test_bm25_basic_search() {
    let docs = vec![
        (
            "sym::authenticate".to_string(),
            "authenticate user login".to_string(),
        ),
        (
            "sym::process_payment".to_string(),
            "process payment transaction".to_string(),
        ),
        (
            "sym::validate_email".to_string(),
            "validate email address format".to_string(),
        ),
    ];
    let index = BM25Index::build(docs);

    let results = index.search("authenticate login", 10);
    assert!(!results.is_empty());
    assert_eq!(index.doc_id(results[0].0), "sym::authenticate");
}

#[test]
fn test_bm25_no_match() {
    let docs = vec![("a".to_string(), "alpha beta gamma".to_string())];
    let index = BM25Index::build(docs);

    let results = index.search("zzzzz_nonexistent", 10);
    assert!(results.is_empty());
}

#[test]
fn test_bm25_empty_index() {
    let index = BM25Index::build(vec![]);
    let results = index.search("anything", 10);
    assert!(results.is_empty());
}

#[test]
fn test_bm25_limit() {
    let docs: Vec<(String, String)> = (0..20)
        .map(|i| (format!("sym_{i}"), format!("common word {i}")))
        .collect();
    let index = BM25Index::build(docs);

    let results = index.search("common word", 5);
    assert!(results.len() <= 5);
}

#[test]
fn test_bm25_doc_accessors() {
    let docs = vec![
        ("id_0".to_string(), "text_0".to_string()),
        ("id_1".to_string(), "text_1".to_string()),
    ];
    let index = BM25Index::build(docs);
    assert_eq!(index.doc_id(0), "id_0");
    assert_eq!(index.doc_text(1), "text_1");
}

#[test]
fn test_bm25_ranking_order() {
    let docs = vec![
        ("rare".to_string(), "authenticate".to_string()),
        ("common".to_string(), "the the the authenticate".to_string()),
        ("irrelevant".to_string(), "process payment".to_string()),
    ];
    let index = BM25Index::build(docs);

    let results = index.search("authenticate", 10);
    assert!(results.len() >= 2);
    // "rare" should rank higher — shorter doc with the term
    let top_id = index.doc_id(results[0].0);
    assert_eq!(
        top_id, "rare",
        "shorter doc should rank higher for exact term"
    );
}

// ---------- combine_scores ----------

#[test]
fn test_combine_scores_pure_bm25() {
    let raw = RawScores {
        bm25: HashMap::from([("a".to_string(), 0.9), ("b".to_string(), 0.5)]),
        vector: HashMap::from([("a".to_string(), 0.1), ("b".to_string(), 0.8)]),
    };

    let results = search::combine_scores(&raw, 0.0, 10);
    assert_eq!(results[0].symbol_id, "a", "alpha=0 should use only BM25");
}

#[test]
fn test_combine_scores_pure_vector() {
    let raw = RawScores {
        bm25: HashMap::from([("a".to_string(), 0.9), ("b".to_string(), 0.5)]),
        vector: HashMap::from([("a".to_string(), 0.1), ("b".to_string(), 0.8)]),
    };

    let results = search::combine_scores(&raw, 1.0, 10);
    assert_eq!(results[0].symbol_id, "b", "alpha=1 should use only vector");
}

#[test]
fn test_combine_scores_limit() {
    let raw = RawScores {
        bm25: HashMap::from([
            ("a".to_string(), 0.9),
            ("b".to_string(), 0.5),
            ("c".to_string(), 0.3),
        ]),
        vector: HashMap::new(),
    };

    let results = search::combine_scores(&raw, 0.0, 2);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_combine_scores_empty() {
    let raw = RawScores {
        bm25: HashMap::new(),
        vector: HashMap::new(),
    };
    let results = search::combine_scores(&raw, 0.5, 10);
    assert!(results.is_empty());
}

// ---------- hybrid_search ----------

#[test]
fn test_hybrid_search_end_to_end() {
    let docs = vec![
        (
            "sym::auth".to_string(),
            "authenticate user login session".to_string(),
        ),
        (
            "sym::pay".to_string(),
            "process payment stripe billing".to_string(),
        ),
        (
            "sym::email".to_string(),
            "validate email address smtp".to_string(),
        ),
    ];
    let index = BM25Index::build(docs.clone());

    let embedder = MockEmbedder::new(32);
    let embeddings: Vec<(String, Vec<f32>)> = docs
        .iter()
        .map(|(id, text)| {
            let emb = embedder.embed(text).unwrap();
            (id.clone(), emb)
        })
        .collect();

    let results = search::hybrid_search(
        "authenticate login",
        &index,
        &embedder,
        &embeddings,
        10,
        0.3,
        None,
        None,
    )
    .unwrap();

    assert!(!results.is_empty());
    assert_eq!(results[0].symbol_id, "sym::auth");
    assert!(results[0].score > 0.0);
    assert!(results[0].bm25_score > 0.0);
}

#[test]
fn test_hybrid_search_empty_query() {
    let docs = vec![("sym::a".to_string(), "hello world".to_string())];
    let index = BM25Index::build(docs.clone());
    let embedder = MockEmbedder::new(32);
    let embeddings: Vec<(String, Vec<f32>)> = docs
        .iter()
        .map(|(id, text)| (id.clone(), embedder.embed(text).unwrap()))
        .collect();

    let results =
        search::hybrid_search("", &index, &embedder, &embeddings, 10, 0.5, None, None).unwrap();
    // Empty query may return results (vector similarity to zero vec) or not — just don't panic
    let _ = results;
}

// ---------- grep_search ----------

#[test]
fn test_grep_search_basic() {
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("example.py");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "def authenticate():").unwrap();
        writeln!(f, "    pass").unwrap();
        writeln!(f, "def process():").unwrap();
        writeln!(f, "    authenticate()").unwrap();
    }

    let results = search::grep_search(dir.path(), "authenticate", None, 100).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].line_number, 1);
    assert_eq!(results[1].line_number, 4);
}

#[test]
fn test_grep_search_with_file_pattern() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.py"), "match_me\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "match_me\n").unwrap();

    let results = search::grep_search(dir.path(), "match_me", Some("*.py"), 100).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].file.ends_with(".py"));
}

#[test]
fn test_grep_search_limit() {
    let dir = tempfile::TempDir::new().unwrap();
    let content: String = (0..50).map(|i| format!("line_{i}_pattern\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &content).unwrap();

    let results = search::grep_search(dir.path(), "pattern", None, 5).unwrap();
    assert_eq!(results.len(), 5);
}

#[test]
fn test_grep_search_regex() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("code.rs"),
        "fn foo() {}\nfn bar() {}\nlet x = 42;\n",
    )
    .unwrap();

    let results = search::grep_search(dir.path(), r"^fn \w+", None, 100).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_grep_search_no_match() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();

    let results = search::grep_search(dir.path(), "zzzzz_nonexistent", None, 100).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_grep_search_skips_ignored_dirs() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("node_modules").join("dep.js"), "findme\n").unwrap();
    std::fs::write(dir.path().join("app.js"), "findme\n").unwrap();

    // Gitignored, non-hardcoded directory -- only a real .gitignore rule
    // can exclude it.
    std::fs::write(dir.path().join(".gitignore"), "scratchpad/\n").unwrap();
    std::fs::create_dir(dir.path().join("scratchpad")).unwrap();
    std::fs::write(dir.path().join("scratchpad").join("copy.js"), "findme\n").unwrap();

    let results = search::grep_search(dir.path(), "findme", None, 100).unwrap();
    assert_eq!(
        results.len(),
        1,
        "should skip node_modules and gitignored scratchpad/"
    );
    assert!(results[0].file.contains("app.js"));
}

// ---------- read_lines_from_file ----------

#[test]
fn test_read_lines_from_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("src.py");
    std::fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    let text = search::read_lines_from_file(&path, 2, 4).unwrap();
    assert_eq!(text, "line2\nline3\nline4");
}

#[test]
fn test_read_lines_out_of_range() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("short.py");
    std::fs::write(&path, "only\n").unwrap();

    let text = search::read_lines_from_file(&path, 100, 200).unwrap();
    assert!(text.is_empty());
}

#[test]
fn test_read_lines_single_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("one.py");
    std::fs::write(&path, "a\nb\nc\n").unwrap();

    let text = search::read_lines_from_file(&path, 2, 2).unwrap();
    assert_eq!(text, "b");
}
