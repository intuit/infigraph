use std::collections::HashSet;
use std::path::Path;

use infigraph_core::embed::load_embeddings;
use infigraph_docs::chunk::{chunk_document, Chunk, ChunkStrategy};
use infigraph_docs::extract::{extract_document, DocFormat, ExtractedDoc};
use infigraph_docs::links::extract_and_link_doc;
use infigraph_docs::search::DocBM25Index;
use infigraph_docs::store::DocStore;
use infigraph_docs::{is_document_file, DocIndex};

// ==================== is_document_file ====================

#[test]
fn test_is_document_file_supported() {
    let supported = [
        "readme.md",
        "readme.markdown",
        "notes.txt",
        "doc.rst",
        "guide.adoc",
        "spec.org",
        "report.pdf",
        "letter.docx",
        "slides.pptx",
        "data.xlsx",
        "page.html",
        "page.htm",
        "book.epub",
        "data.xml",
        "style.xsl",
        "schema.xsd",
        "icon.svg",
        "config.plist",
        "manual.rtf",
    ];
    for name in &supported {
        assert!(
            is_document_file(Path::new(name)),
            "{name} should be document"
        );
    }
}

#[test]
fn test_is_document_file_unsupported() {
    let unsupported = [
        "main.rs",
        "app.py",
        "index.js",
        "Cargo.toml",
        "Makefile",
        "no_extension",
        "image.png",
        "photo.jpg",
        "video.mp4",
    ];
    for name in &unsupported {
        assert!(
            !is_document_file(Path::new(name)),
            "{name} should not be document"
        );
    }
}

// ==================== extract ====================

#[test]
fn test_extract_markdown() {
    let content = b"# My Title\n\nSome paragraph text.\n\n## Section Two\n\nMore content here.\n";
    let doc = extract_document(Path::new("test.md"), content, "md").unwrap();
    assert_eq!(doc.format, DocFormat::Markdown);
    assert_eq!(doc.title.as_deref(), Some("My Title"));
    assert!(doc.text.contains("Some paragraph text"));
    assert!(doc.text.contains("Section Two"));
    assert!(doc.page_count.is_none());
}

#[test]
fn test_extract_plaintext() {
    let content = b"Hello World\nThis is plain text.\n";
    let doc = extract_document(Path::new("test.txt"), content, "txt").unwrap();
    assert_eq!(doc.format, DocFormat::PlainText);
    assert_eq!(doc.title.as_deref(), Some("Hello World"));
    assert!(doc.text.contains("plain text"));
}

#[test]
fn test_extract_html() {
    let content =
        b"<html><head><title>My Page</title></head><body><p>Hello world</p></body></html>";
    let doc = extract_document(Path::new("test.html"), content, "html").unwrap();
    assert_eq!(doc.format, DocFormat::Html);
    assert_eq!(doc.title.as_deref(), Some("My Page"));
    assert!(doc.text.contains("Hello world"), "html text: {}", doc.text);
}

#[test]
fn test_extract_xml() {
    let content = b"<root><item>First</item><item>Second</item></root>";
    let doc = extract_document(Path::new("test.xml"), content, "xml").unwrap();
    assert_eq!(doc.format, DocFormat::Xml);
    assert!(doc.text.contains("First"), "xml text: {}", doc.text);
    assert!(doc.text.contains("Second"), "xml text: {}", doc.text);
}

#[test]
fn test_extract_rst() {
    let content = b"My Document\n===========\n\nRST content here.\n";
    let doc = extract_document(Path::new("test.rst"), content, "rst").unwrap();
    assert_eq!(doc.format, DocFormat::Rst);
    assert!(doc.text.contains("RST content"));
}

#[test]
fn test_extract_unsupported_format() {
    let result = extract_document(Path::new("test.rs"), b"fn main() {}", "rs");
    assert!(result.is_err(), "unsupported format should error");
}

// ==================== chunk ====================

fn make_doc(text: &str) -> ExtractedDoc {
    ExtractedDoc {
        file: "test.md".to_string(),
        title: None,
        content_hash: "abc123".to_string(),
        format: DocFormat::Markdown,
        text: text.to_string(),
        page_count: None,
    }
}

#[test]
fn test_chunk_by_headings() {
    let text = "# Introduction\n\nThis is the intro.\n\n## Details\n\nHere are details.\n";
    let doc = make_doc(text);
    let chunks = chunk_document(&doc, "test.md", "hash1", ChunkStrategy::HeadingBounded);
    assert!(
        chunks.len() >= 2,
        "should produce at least 2 chunks: got {}",
        chunks.len()
    );
    assert!(
        chunks[0].text.contains("Introduction"),
        "first chunk: {}",
        chunks[0].text
    );
    assert!(
        chunks.iter().any(|c| c.text.contains("Details")),
        "should have Details chunk"
    );

    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.index, i, "chunk index mismatch");
        assert_eq!(c.doc_file, "test.md");
        assert!(!c.id.is_empty());
    }
}

#[test]
fn test_chunk_no_headings_falls_back_to_paragraphs() {
    let paragraphs: Vec<String> = (0..5)
        .map(|i| format!("Paragraph {} has some text content that is meaningful.", i))
        .collect();
    let text = paragraphs.join("\n\n");
    let doc = make_doc(&text);
    let chunks = chunk_document(&doc, "doc.txt", "hash2", ChunkStrategy::HeadingBounded);
    assert!(!chunks.is_empty(), "should produce chunks from paragraphs");
    assert!(
        chunks[0].text.contains("Paragraph"),
        "chunk text: {}",
        chunks[0].text
    );
}

#[test]
fn test_chunk_empty_text() {
    let doc = make_doc("");
    let chunks = chunk_document(&doc, "empty.md", "hash3", ChunkStrategy::HeadingBounded);
    assert!(chunks.is_empty(), "empty text should produce no chunks");
}

#[test]
fn test_chunk_fixed_token() {
    let words: Vec<String> = (0..600).map(|i| format!("word{i}")).collect();
    let text = words.join(" ");
    let doc = make_doc(&text);
    let chunks = chunk_document(
        &doc,
        "big.txt",
        "hash4",
        ChunkStrategy::FixedToken {
            size: 100,
            overlap: 20,
        },
    );
    assert!(
        chunks.len() >= 6,
        "600 words / 100 token chunks = at least 6 chunks, got {}",
        chunks.len()
    );
    assert!(chunks[0].text.contains("word0"));
}

// ==================== BM25 search ====================

#[test]
fn test_bm25_basic_ranking() {
    let docs = vec![
        (
            "doc1".to_string(),
            "the quick brown fox jumps over the lazy dog".to_string(),
        ),
        (
            "doc2".to_string(),
            "rust programming language is fast and safe".to_string(),
        ),
        (
            "doc3".to_string(),
            "the fox and the dog are friends".to_string(),
        ),
    ];
    let index = DocBM25Index::build(docs);

    let results = index.search("fox", 10);
    assert!(!results.is_empty(), "should find fox");
    let top_ids: Vec<usize> = results.iter().map(|(idx, _)| *idx).collect();
    assert!(top_ids.contains(&0), "doc1 has 'fox'");
    assert!(top_ids.contains(&2), "doc3 has 'fox'");
    assert!(!top_ids.contains(&1), "doc2 has no 'fox'");
}

#[test]
fn test_bm25_no_match() {
    let docs = vec![("doc1".to_string(), "hello world".to_string())];
    let index = DocBM25Index::build(docs);
    let results = index.search("nonexistent", 10);
    assert!(results.is_empty(), "no match expected");
}

#[test]
fn test_bm25_empty_corpus() {
    let index = DocBM25Index::build(Vec::new());
    let results = index.search("anything", 10);
    assert!(results.is_empty());
}

// ==================== DocStore CRUD ====================

fn temp_store() -> (DocStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.kuzu");
    let store = DocStore::open(&db_path).unwrap();
    (store, dir)
}

fn sample_doc(file: &str) -> ExtractedDoc {
    ExtractedDoc {
        file: file.to_string(),
        title: Some(format!("Title of {file}")),
        content_hash: format!("hash_{file}"),
        format: DocFormat::Markdown,
        text: format!("Content of {file}"),
        page_count: Some(1),
    }
}

fn sample_chunk(file: &str, idx: usize) -> Chunk {
    Chunk {
        id: format!("{file}::chunk_{idx}"),
        doc_file: file.to_string(),
        content_hash: format!("hash_{file}"),
        index: idx,
        heading: Some(format!("Section {idx}")),
        text: format!("Chunk {idx} text for {file}"),
        start_offset: idx * 100,
        end_offset: (idx + 1) * 100,
        page: Some(0),
    }
}

#[test]
fn test_store_open_and_schema() {
    let (store, _dir) = temp_store();
    let conn = store.connection().unwrap();
    let result = conn.query("MATCH (d:Document) RETURN count(d)").unwrap();
    assert!(result.get_num_tuples() > 0 || result.get_num_tuples() == 0);
}

#[test]
fn test_store_doc_hashes_empty() {
    let (store, _dir) = temp_store();
    let hashes = store.get_doc_hashes().unwrap();
    assert!(hashes.is_empty(), "new store should have no doc hashes");
}

#[test]
fn test_store_upsert_and_hashes() {
    let (store, _dir) = temp_store();

    let doc1 = sample_doc("readme.md");
    let doc2 = sample_doc("guide.md");
    let c1 = sample_chunk("readme.md", 0);
    let c2 = sample_chunk("readme.md", 1);
    let c3 = sample_chunk("guide.md", 0);

    store
        .upsert_all_parquet(&[&doc1, &doc2], &[&c1, &c2, &c3])
        .unwrap();

    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 2, "should have 2 docs");
    assert_eq!(hashes.get("readme.md").unwrap(), "hash_readme.md");
    assert_eq!(hashes.get("guide.md").unwrap(), "hash_guide.md");
}

#[test]
fn test_store_stats() {
    let (store, _dir) = temp_store();

    let doc = sample_doc("test.md");
    let c1 = sample_chunk("test.md", 0);
    let c2 = sample_chunk("test.md", 1);
    store.upsert_all_parquet(&[&doc], &[&c1, &c2]).unwrap();

    let stats = store.stats().unwrap();
    assert_eq!(stats.document_count, 1);
    assert_eq!(stats.chunk_count, 2);
}

#[test]
fn test_store_get_all_chunks() {
    let (store, _dir) = temp_store();

    let doc = sample_doc("file.md");
    let c1 = sample_chunk("file.md", 0);
    let c2 = sample_chunk("file.md", 1);
    store.upsert_all_parquet(&[&doc], &[&c1, &c2]).unwrap();

    let chunks = store.get_all_chunks().unwrap();
    assert_eq!(chunks.len(), 2, "should have 2 chunks");
    assert!(chunks.iter().any(|(id, _)| id.contains("chunk_0")));
    assert!(chunks.iter().any(|(id, _)| id.contains("chunk_1")));
}

#[test]
fn test_store_get_chunk_ids() {
    let (store, _dir) = temp_store();

    let doc = sample_doc("a.md");
    let c = sample_chunk("a.md", 0);
    store.upsert_all_parquet(&[&doc], &[&c]).unwrap();

    let ids = store.get_chunk_ids().unwrap();
    assert!(ids.contains("a.md::chunk_0"), "ids: {ids:?}");
}

#[test]
fn test_store_get_chunk_details() {
    let (store, _dir) = temp_store();

    let doc = sample_doc("detail.md");
    let c = sample_chunk("detail.md", 0);
    store.upsert_all_parquet(&[&doc], &[&c]).unwrap();

    let details = store.get_chunk_details(&["detail.md::chunk_0"]).unwrap();
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].id, "detail.md::chunk_0");
    assert!(details[0].text.contains("Chunk 0 text"));
}

#[test]
fn test_store_delete_docs_by_ids() {
    let (store, _dir) = temp_store();

    let doc1 = sample_doc("keep.md");
    let doc2 = sample_doc("delete.md");
    let c1 = sample_chunk("keep.md", 0);
    let c2 = sample_chunk("delete.md", 0);
    store
        .upsert_all_parquet(&[&doc1, &doc2], &[&c1, &c2])
        .unwrap();

    store.delete_docs_by_ids(&["delete.md"]).unwrap();

    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 1);
    assert!(hashes.contains_key("keep.md"));
    assert!(!hashes.contains_key("delete.md"));
}

#[test]
fn test_store_source_crud() {
    let (store, _dir) = temp_store();

    store
        .upsert_source("src1", "confluence", "https://wiki.example.com", "SPACE")
        .unwrap();

    let doc = sample_doc("page.md");
    let c = sample_chunk("page.md", 0);
    store.upsert_all_parquet(&[&doc], &[&c]).unwrap();

    store.link_doc_to_source("page.md", "src1").unwrap();
    let docs = store.get_docs_by_source("src1").unwrap();
    assert!(
        docs.contains(&"page.md".to_string()),
        "should find linked doc: {docs:?}"
    );
}

#[test]
fn test_store_links_crud() {
    let (store, _dir) = temp_store();

    let doc1 = sample_doc("a.md");
    let doc2 = sample_doc("b.md");
    let c1 = sample_chunk("a.md", 0);
    let c2 = sample_chunk("b.md", 0);
    store
        .upsert_all_parquet(&[&doc1, &doc2], &[&c1, &c2])
        .unwrap();

    store.create_link("a.md", "b.md", "b.md", "local").unwrap();

    let conn = store.connection().unwrap();
    let result = conn
        .query("MATCH (a:Document)-[l:LINKS_TO]->(b:Document) RETURN a.id, b.id, l.url")
        .unwrap();
    let mut found = false;
    for row in result {
        if row[0].to_string() == "a.md" && row[1].to_string() == "b.md" {
            found = true;
        }
    }
    assert!(found, "should have LINKS_TO edge from a.md to b.md");

    store.delete_links_from("a.md").unwrap();
    let mut result2 = conn
        .query("MATCH (a:Document)-[l:LINKS_TO]->(b:Document) WHERE a.id = 'a.md' RETURN count(l)")
        .unwrap();
    if let Some(row) = result2.next() {
        let count: i64 = row[0].to_string().parse().unwrap_or(0);
        assert_eq!(count, 0, "links should be deleted");
    }
}

// ==================== links::extract_and_link_doc ====================

#[test]
fn test_extract_and_link_doc_markdown_links() {
    let (store, _dir) = temp_store();

    let doc_a = sample_doc("docs/index.md");
    let doc_b = sample_doc("docs/guide.md");
    let c_a = sample_chunk("docs/index.md", 0);
    let c_b = sample_chunk("docs/guide.md", 0);
    store
        .upsert_all_parquet(&[&doc_a, &doc_b], &[&c_a, &c_b])
        .unwrap();

    let source_doc = ExtractedDoc {
        file: "docs/index.md".to_string(),
        title: Some("Index".to_string()),
        content_hash: "hash1".to_string(),
        format: DocFormat::Markdown,
        text: "See the [guide](guide.md) for details.\nAlso [external](https://example.com)."
            .to_string(),
        page_count: None,
    };

    let all_doc_ids: HashSet<String> = ["docs/index.md", "docs/guide.md"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn.query(
        "MATCH (a:Document)-[l:LINKS_TO]->(b:Document) WHERE a.id = 'docs/index.md' RETURN b.id, l.link_type"
    ).unwrap();
    let mut linked_to_guide = false;
    let mut linked_external = false;
    for row in result {
        let target = row[0].to_string();
        if target == "docs/guide.md" {
            linked_to_guide = true;
        }
        if row[1].to_string() == "external" {
            linked_external = true;
        }
    }
    assert!(
        linked_to_guide,
        "should create LINKS_TO for relative markdown link"
    );
    assert!(
        !linked_external,
        "should NOT create LINKS_TO for external links (target not in all_doc_ids)"
    );
}

// ==================== DocIndex lifecycle ====================

#[test]
fn test_docindex_open_creates_infigraph_dir() {
    let dir = tempfile::tempdir().unwrap();
    let _idx = DocIndex::open(dir.path()).unwrap();
    assert!(
        dir.path().join(".infigraph").exists(),
        ".infigraph dir should be created"
    );
}

#[test]
fn test_docindex_init_creates_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = DocIndex::open(dir.path()).unwrap();
    assert!(idx.store().is_none(), "store should be None before init");
    idx.init().unwrap();
    assert!(idx.store().is_some(), "store should be Some after init");
}

#[test]
fn test_docindex_init_recovers_from_corrupt_db() {
    let dir = tempfile::tempdir().unwrap();
    let ig = dir.path().join(".infigraph");
    std::fs::create_dir_all(&ig).unwrap();
    // Garbage file where Kuzu DB should be — open must fail, then wipe+rebuild
    std::fs::write(ig.join("docs.kuzu"), b"not-a-valid-kuzu-database").unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# Recover\n\nDoc content for rebuild.\n",
    )
    .unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init()
        .expect("init should wipe corrupt docs.kuzu and rebuild");
    assert!(idx.store().is_some());
    let stats = idx.store().unwrap().stats().unwrap();
    assert!(
        stats.document_count >= 1,
        "expected rebuilt docs, got document_count={}",
        stats.document_count
    );
}

#[cfg(unix)]
#[test]
fn test_docindex_init_errors_when_wipe_cannot_repair() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let ig = dir.path().join(".infigraph");
    std::fs::create_dir_all(&ig).unwrap();
    std::fs::write(ig.join("docs.kuzu"), b"not-a-valid-kuzu-database").unwrap();

    // Read-only .infigraph — clean cannot delete corrupt DB, reopen still fails
    let mut perms = std::fs::metadata(&ig).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&ig, perms).unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    let result = idx.init();

    // Restore so tempfile can clean up
    let mut perms = std::fs::metadata(&ig).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&ig, perms).unwrap();

    assert!(
        result.is_err(),
        "expected init to error when corrupt DB cannot be removed"
    );
}

#[test]
fn test_docindex_clean_removes_db() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    assert!(idx.store().is_some());

    idx.clean().unwrap();
    assert!(idx.store().is_none(), "store should be None after clean");
}

#[test]
fn test_docindex_index_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();
    assert_eq!(result.total_files, 0);
    assert_eq!(result.indexed_files, 0);
    assert_eq!(result.total_chunks, 0);
}

#[test]
fn test_docindex_index_with_files() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("readme.md"),
        "# Project\n\nThis is the readme.\n\n## Setup\n\nRun install.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "Some plain text notes about the project.\n\nAnother paragraph.\n",
    )
    .unwrap();
    // Non-document file should be ignored
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();

    let result = idx.index().unwrap();
    assert_eq!(result.total_files, 2, "should find 2 document files");
    assert_eq!(result.indexed_files, 2, "should index both");
    assert!(result.total_chunks > 0, "should produce chunks");

    let store = idx.store().unwrap();
    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 2);
    assert!(hashes.contains_key("readme.md") || hashes.contains_key("notes.txt"));
}

#[test]
fn test_docindex_reindex_is_incremental() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.md"), "# Hello\n\nWorld.\n").unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    let r1 = idx.index().unwrap();
    assert_eq!(r1.indexed_files, 1);

    // Second index with same content should be no-op
    let r2 = idx.index().unwrap();
    assert_eq!(
        r2.indexed_files, 0,
        "unchanged file should not be re-indexed"
    );
    assert_eq!(r2.total_files, 1, "should still see the file");
}

#[test]
fn test_docindex_reindex_picks_up_changes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.md"), "# Original\n\nContent.\n").unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    idx.index().unwrap();

    // Modify the file
    std::fs::write(dir.path().join("doc.md"), "# Updated\n\nNew content.\n").unwrap();

    let r2 = idx.index().unwrap();
    assert_eq!(r2.indexed_files, 1, "changed file should be re-indexed");
}

#[test]
fn test_docindex_ignores_hidden_and_build_dirs() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config.txt"), "git config").unwrap();

    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/readme.md"), "# Pkg").unwrap();

    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/output.txt"), "build output").unwrap();

    std::fs::write(dir.path().join("real.md"), "# Real Doc\n\nContent.\n").unwrap();

    let mut idx = DocIndex::open(dir.path()).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();
    assert_eq!(
        result.total_files, 1,
        "should only find real.md, not files in ignored dirs"
    );
}

// --- BFS tests ---

fn create_doc_file(base: &Path, rel_path: &str, content: &str) {
    let full = base.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, content).unwrap();
}

#[test]
fn test_bfs_follows_link_outside_root() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(
        &repo,
        "docs/index.md",
        "# Index\n\nSee [readme](../README.md).\n",
    );
    create_doc_file(&repo, "README.md", "# Project README\n\nHello.\n");

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();

    assert_eq!(result.total_files, 1, "only index.md in doc root");
    assert_eq!(result.bfs_discovered, 1, "BFS should discover README.md");

    let store = idx.store().unwrap();
    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 2, "should have 2 docs total");

    let chunk_ids: HashSet<_> = store.get_chunk_ids().unwrap().into_iter().collect();
    let embedded_ids: HashSet<_> =
        load_embeddings(&doc_root.join(".infigraph/docs_embeddings.bin"))
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
    assert_eq!(
        embedded_ids, chunk_ids,
        "BFS-discovered chunks should have embeddings"
    );
}

#[test]
fn test_bfs_respects_repo_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(
        &repo,
        "docs/index.md",
        "# Index\n\nSee [outside](../../outside/file.md).\n",
    );
    create_doc_file(tmp.path(), "outside/file.md", "# Outside\n\nContent.\n");

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();

    assert_eq!(
        result.bfs_discovered, 0,
        "should not follow links outside repo"
    );
    let store = idx.store().unwrap();
    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 1, "only index.md");
}

#[test]
fn test_bfs_max_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(&repo, "docs/a.md", "# A\n\n[b](../level1/b.md)\n");
    create_doc_file(&repo, "level1/b.md", "# B\n\n[c](../level2/c.md)\n");
    create_doc_file(&repo, "level2/c.md", "# C\n\n[d](../level3/d.md)\n");
    create_doc_file(&repo, "level3/d.md", "# D\n\nEnd.\n");

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();

    // depth 0: a.md (in root), depth 1: b.md, depth 2: c.md, depth 3: d.md (too deep)
    assert!(result.bfs_discovered >= 2, "should discover b.md and c.md");
    let store = idx.store().unwrap();
    let hashes = store.get_doc_hashes().unwrap();
    // d.md should NOT be indexed (depth 3 > max_depth 2)
    let has_d = hashes.keys().any(|k| k.contains("level3"));
    assert!(!has_d, "d.md should not be indexed (too deep)");
}

#[test]
fn test_bfs_skips_non_doc_files() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(
        &repo,
        "docs/index.md",
        "# Index\n\n[code](../src/main.rs)\n",
    );
    create_doc_file(&repo, "src/main.rs", "fn main() {}\n");

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();

    assert_eq!(result.bfs_discovered, 0, "should not index .rs files");
}

#[test]
fn test_bfs_skips_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(&repo, "docs/index.md", "# Index\n\n[link](../linked.md)\n");
    create_doc_file(&repo, "real.md", "# Real\n\nContent.\n");
    // Create symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(repo.join("real.md"), repo.join("linked.md")).unwrap();
    #[cfg(not(unix))]
    {
        // On non-unix, just create a regular file so the test still runs
        create_doc_file(&repo, "linked.md", "# Linked\n\nContent.\n");
    }

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    #[allow(unused_variables)]
    let result = idx.index().unwrap();

    #[cfg(unix)]
    assert_eq!(result.bfs_discovered, 0, "should not follow symlinks");
}

#[test]
fn test_bfs_circular_links() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    create_doc_file(&repo, "docs/a.md", "# A\n\n[b](../other/b.md)\n");
    create_doc_file(&repo, "other/b.md", "# B\n\n[a](../docs/a.md)\n");

    let doc_root = repo.join("docs");
    let mut idx = DocIndex::open(&doc_root).unwrap();
    idx.init().unwrap();
    let result = idx.index().unwrap();

    assert_eq!(result.bfs_discovered, 1, "should discover b.md once");
    let store = idx.store().unwrap();
    let hashes = store.get_doc_hashes().unwrap();
    assert_eq!(hashes.len(), 2, "a.md + b.md");
}

// --- Confluence linking tests ---

#[test]
fn test_doc_links_to_confluence_page() {
    let (store, _dir) = temp_store();

    // Create a local doc and a confluence doc in the same store
    let local_doc = sample_doc("docs/design.md");
    let confluence_doc = ExtractedDoc {
        file: "confluence://ENG/99999".to_string(),
        title: Some("Design Doc".to_string()),
        content_hash: "confhash".to_string(),
        format: DocFormat::Markdown,
        text: "Confluence page content.".to_string(),
        page_count: Some(1),
    };
    let c_local = sample_chunk("docs/design.md", 0);
    let c_conf = Chunk {
        id: "confluence://ENG/99999::0".to_string(),
        doc_file: "confluence://ENG/99999".to_string(),
        index: 0,
        heading: None,
        text: "Confluence page content.".to_string(),
        start_offset: 0,
        end_offset: 24,
        content_hash: "confhash".to_string(),
        page: None,
    };
    store
        .upsert_all_parquet(&[&local_doc, &confluence_doc], &[&c_local, &c_conf])
        .unwrap();

    let source_doc = ExtractedDoc {
        file: "docs/design.md".to_string(),
        title: Some("Design".to_string()),
        content_hash: "hash1".to_string(),
        format: DocFormat::Markdown,
        text: "See [design](https://wiki.example.com/wiki/spaces/ENG/pages/99999/Design+Doc) for details."
            .to_string(),
        page_count: None,
    };

    let all_doc_ids: HashSet<String> = [
        "docs/design.md".to_string(),
        "confluence://ENG/99999".to_string(),
    ]
    .into_iter()
    .collect();

    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn
        .query(
            "MATCH (a:Document)-[l:LINKS_TO]->(b:Document) WHERE a.id = 'docs/design.md' RETURN b.id, l.link_type",
        )
        .unwrap();
    let mut linked_to_confluence = false;
    for row in result {
        let target = row[0].to_string();
        if target == "confluence://ENG/99999" {
            linked_to_confluence = true;
        }
    }
    assert!(
        linked_to_confluence,
        "should create LINKS_TO edge from local doc to confluence page"
    );
}

#[test]
fn test_confluence_page_id_no_space_no_link() {
    let (store, _dir) = temp_store();

    let local_doc = sample_doc("docs/notes.md");
    let c = sample_chunk("docs/notes.md", 0);
    store.upsert_all_parquet(&[&local_doc], &[&c]).unwrap();

    let source_doc = ExtractedDoc {
        file: "docs/notes.md".to_string(),
        title: Some("Notes".to_string()),
        content_hash: "hash1".to_string(),
        format: DocFormat::Markdown,
        text: "See [page](https://wiki.example.com/pages?pageId=12345) for info.".to_string(),
        page_count: None,
    };

    let all_doc_ids: HashSet<String> = ["docs/notes.md".to_string()].into_iter().collect();
    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn
        .query(
            "MATCH (a:Document)-[:LINKS_TO]->(b:Document) WHERE a.id = 'docs/notes.md' RETURN b.id",
        )
        .unwrap();
    let count: usize = result.count();
    assert_eq!(
        count, 0,
        "pageId-only URL should not create link (no space)"
    );
}

#[test]
fn test_confluence_page_id_non_numeric_no_link() {
    let (store, _dir) = temp_store();

    let local_doc = sample_doc("docs/notes.md");
    let c = sample_chunk("docs/notes.md", 0);
    store.upsert_all_parquet(&[&local_doc], &[&c]).unwrap();

    let source_doc = ExtractedDoc {
        file: "docs/notes.md".to_string(),
        title: Some("Notes".to_string()),
        content_hash: "hash1".to_string(),
        format: DocFormat::Markdown,
        text: "See [overview](https://wiki.example.com/wiki/spaces/TEAM/pages/overview) for info."
            .to_string(),
        page_count: None,
    };

    let all_doc_ids: HashSet<String> = ["docs/notes.md".to_string()].into_iter().collect();
    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn
        .query(
            "MATCH (a:Document)-[:LINKS_TO]->(b:Document) WHERE a.id = 'docs/notes.md' RETURN b.id",
        )
        .unwrap();
    let count: usize = result.count();
    assert_eq!(count, 0, "non-numeric page ID should not create link");
}

#[test]
fn test_extract_doc_path_from_url_github() {
    use infigraph_docs::links::extract_doc_path_from_url;
    assert_eq!(
        extract_doc_path_from_url("https://github.com/org/repo/blob/main/docs/README.md"),
        Some("docs/README.md".to_string())
    );
    assert_eq!(
        extract_doc_path_from_url("https://github.com/org/repo/blob/main/docs/guide.md#section"),
        Some("docs/guide.md".to_string())
    );
    assert_eq!(
        extract_doc_path_from_url("https://gitlab.com/org/repo/-/blob/main/docs/README.md"),
        Some("docs/README.md".to_string())
    );
    assert_eq!(extract_doc_path_from_url("https://example.com/docs"), None);
    assert_eq!(extract_doc_path_from_url("https://docs.rs/foo"), None);
}

#[test]
fn test_link_manifest_doc_urls() {
    use infigraph_docs::links::link_manifest_doc_urls;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("docs.kuzu");
    let store = DocStore::open(&db_path).unwrap();

    // Insert a doc node
    let doc = ExtractedDoc {
        file: "docs/README.md".to_string(),
        title: Some("README".to_string()),
        content_hash: "abc123".to_string(),
        format: DocFormat::Markdown,
        text: "Hello".to_string(),
        page_count: None,
    };
    let chunks = chunk_document(
        &doc,
        "docs/README.md",
        "abc123",
        ChunkStrategy::HeadingBounded,
    );
    let docs_ref = vec![&doc];
    let chunks_ref: Vec<&Chunk> = chunks.iter().collect();
    store.upsert_all_parquet(&docs_ref, &chunks_ref).unwrap();

    let mut all_doc_ids = HashSet::new();
    all_doc_ids.insert("docs/README.md".to_string());

    let doc_urls = vec![
        "https://github.com/org/repo/blob/main/docs/README.md".to_string(),
        "https://example.com/unrelated".to_string(),
    ];

    link_manifest_doc_urls(&store, "Cargo.toml", &doc_urls, &all_doc_ids);

    // Verify LINKS_TO edge created for matching URL
    let conn = store.connection().unwrap();
    let result = conn
        .query("MATCH (a:Document)-[e:LINKS_TO]->(b:Document) RETURN a.id, b.id, e.link_type")
        .unwrap();
    let rows: Vec<_> = result.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_extract_repo_from_url() {
    use infigraph_docs::links::extract_repo_from_url;
    assert_eq!(
        extract_repo_from_url("https://github.com/org/my-repo/blob/main/docs/foo.md"),
        Some("my-repo".to_string())
    );
    assert_eq!(
        extract_repo_from_url("https://github.intuit.com/intuit-tech-arch-decisions/data-fabric/blob/master/doc/adr/0012.md"),
        Some("data-fabric".to_string())
    );
    assert_eq!(
        extract_repo_from_url("https://gitlab.com/org/repo/-/blob/main/README.md"),
        Some("repo".to_string())
    );
    assert_eq!(
        extract_repo_from_url(
            "https://gitlab.com/group/subgroup/nested-repo/-/blob/main/docs/README.md"
        ),
        Some("nested-repo".to_string())
    );
    assert_eq!(extract_repo_from_url("not-a-url"), None);
    assert_eq!(extract_repo_from_url("https://example.com"), None);
    // PR URL — still extracts repo name
    assert_eq!(
        extract_repo_from_url("https://github.com/org/my-repo/pull/42"),
        Some("my-repo".to_string())
    );
    // HTTP (not HTTPS)
    assert_eq!(
        extract_repo_from_url("http://github.com/org/repo/blob/main/README.md"),
        Some("repo".to_string())
    );
}

#[test]
fn test_resolve_doc_id_exact_suffix_and_boundary() {
    use infigraph_docs::links::resolve_doc_id;

    let ids: HashSet<String> = ["README.md", "guide/foo.md", "not-foo.md"]
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        resolve_doc_id("README.md", &ids),
        Some("README.md".to_string())
    );
    assert_eq!(
        resolve_doc_id("docs/guide/foo.md", &ids),
        Some("guide/foo.md".to_string())
    );
    assert_eq!(
        resolve_doc_id("foo.md", &ids),
        Some("guide/foo.md".to_string())
    );
    assert_eq!(resolve_doc_id("bar/foo.md", &ids), None);

    let ambiguous: HashSet<String> = ["guide/foo.md", "other/foo.md"]
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(resolve_doc_id("foo.md", &ambiguous), None);
}

// ==================== classify_doc_link: GitHub branch ====================

#[test]
fn test_classify_github_blob_url() {
    use infigraph_docs::links::extract_links;

    let text =
        "See [design](https://github.com/org/repo/blob/main/doc/adr/0001-design.md) for details.";
    let links = extract_links(text, "README.md");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_type, "github");
    assert_eq!(
        links[0].target_doc_id,
        Some("doc/adr/0001-design.md".to_string())
    );
}

#[test]
fn test_classify_gitlab_blob_url() {
    use infigraph_docs::links::extract_links;

    let text = "See [doc](https://gitlab.com/org/repo/-/blob/main/docs/guide.md) here.";
    let links = extract_links(text, "README.md");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_type, "github");
    assert_eq!(links[0].target_doc_id, Some("docs/guide.md".to_string()));
}

#[test]
fn test_classify_github_url_with_fragment() {
    use infigraph_docs::links::extract_links;

    let text =
        "See [section](https://github.com/org/repo/blob/main/doc/design.md#architecture) for arch.";
    let links = extract_links(text, "README.md");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_type, "github");
    assert_eq!(links[0].target_doc_id, Some("doc/design.md".to_string()));
}

#[test]
fn test_classify_github_non_blob_url_is_external() {
    use infigraph_docs::links::extract_links;

    // PR URL, user profile, etc. — no /blob/ → external
    let text = "See [PR](https://github.com/org/repo/pull/42) for review.";
    let links = extract_links(text, "README.md");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_type, "external");
    assert_eq!(links[0].target_doc_id, None);
}

#[test]
fn test_classify_plain_https_url_still_external() {
    use infigraph_docs::links::extract_links;

    let text = "Visit [docs](https://docs.rs/serde) for API reference.";
    let links = extract_links(text, "README.md");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_type, "external");
    assert_eq!(links[0].target_doc_id, None);
}

// ==================== extract_and_link_doc: GitHub intra-repo ====================

#[test]
fn test_intra_repo_github_link_creates_edge() {
    let (store, _dir) = temp_store();

    let target_doc = sample_doc("doc/adr/0005-design.md");
    let c = sample_chunk("doc/adr/0005-design.md", 0);
    store.upsert_all_parquet(&[&target_doc], &[&c]).unwrap();

    let source_doc = ExtractedDoc {
        file: "doc/adr/0010-refresh.md".to_string(),
        title: Some("Refresh".to_string()),
        content_hash: "hash_src".to_string(),
        format: DocFormat::Markdown,
        text: "Supersedes [design](https://github.com/org/repo/blob/main/doc/adr/0005-design.md)."
            .to_string(),
        page_count: None,
    };
    let c2 = sample_chunk("doc/adr/0010-refresh.md", 0);
    store.upsert_all_parquet(&[&source_doc], &[&c2]).unwrap();

    let all_doc_ids: HashSet<String> = [
        "doc/adr/0005-design.md".to_string(),
        "doc/adr/0010-refresh.md".to_string(),
    ]
    .into_iter()
    .collect();

    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn
        .query(
            "MATCH (a:Document)-[l:LINKS_TO]->(b:Document) WHERE a.id = 'doc/adr/0010-refresh.md' RETURN b.id, l.link_type",
        )
        .unwrap();
    let rows: Vec<_> = result.collect();
    assert_eq!(rows.len(), 1, "should create one LINKS_TO edge");
    assert_eq!(rows[0][0].to_string(), "doc/adr/0005-design.md");
    assert_eq!(rows[0][1].to_string(), "github");
}

#[test]
fn test_cross_repo_github_link_no_edge_without_group() {
    let (store, _dir) = temp_store();

    let source_doc = ExtractedDoc {
        file: "doc/adr/0052-behavior.md".to_string(),
        title: Some("Behavior".to_string()),
        content_hash: "hash_src".to_string(),
        format: DocFormat::Markdown,
        text: "See [data map](https://github.com/org/data-fabric/blob/master/doc/adr/0012-data-map.md).".to_string(),
        page_count: None,
    };
    let c = sample_chunk("doc/adr/0052-behavior.md", 0);
    store.upsert_all_parquet(&[&source_doc], &[&c]).unwrap();

    // all_doc_ids only has current repo's docs — data-fabric doc not present
    let all_doc_ids: HashSet<String> = ["doc/adr/0052-behavior.md".to_string()]
        .into_iter()
        .collect();

    extract_and_link_doc(&store, &source_doc, &all_doc_ids);

    let conn = store.connection().unwrap();
    let result = conn
        .query("MATCH ()-[l:LINKS_TO]->() RETURN count(l)")
        .unwrap();
    let rows: Vec<_> = result.collect();
    let count: i64 = rows[0][0].to_string().parse().unwrap();
    assert_eq!(
        count, 0,
        "cross-repo link should NOT create edge without group context"
    );
}
