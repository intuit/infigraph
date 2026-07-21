use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use infigraph_core::embed::{load_embeddings, save_embeddings};
use infigraph_core::multi::{Group, Registry, RepoEntry};
use infigraph_docs::chunk::{chunk_document, ChunkStrategy};
use infigraph_docs::combined::{
    build_combined_docs, combined_doc_query, combined_doc_search, combined_docs_path,
    has_combined_docs, schedule_group_doc_refresh,
};
use infigraph_docs::extract::{DocFormat, ExtractedDoc};
use infigraph_docs::store::DocStore;

static COMBINED_DOCS_LOCK: Mutex<()> = Mutex::new(());

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

fn registry(repo_a: &std::path::Path, repo_b: &std::path::Path) -> Registry {
    let mut repos = HashMap::new();
    for (name, path) in [("repo-a", repo_a), ("server", repo_b)] {
        repos.insert(
            name.to_string(),
            RepoEntry {
                name: name.to_string(),
                path: path.to_path_buf(),
                languages: Vec::new(),
                symbol_count: 0,
                module_count: 0,
                last_indexed_commit: None,
            },
        );
    }
    let mut groups = HashMap::new();
    groups.insert(
        "fleet".to_string(),
        Group {
            name: "fleet".to_string(),
            org: String::new(),
            repos: vec!["repo-a".to_string(), "server".to_string()],
            contracts: Vec::new(),
        },
    );
    Registry { repos, groups }
}

#[test]
fn combined_docs_merge_search_link_and_rebuild() {
    let _guard = COMBINED_DOCS_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    let old_hnsw_threshold = std::env::var_os("INFIGRAPH_DOC_HNSW_THRESHOLD");
    std::env::set_var("HOME", home.path());
    std::env::set_var("INFIGRAPH_DOC_HNSW_THRESHOLD", "2");

    let repo_a = tempfile::tempdir().unwrap();
    let repo_b = tempfile::tempdir().unwrap();
    index_doc(
        repo_a.path(),
        "README.md",
        "# Client\nSee [server docs](https://github.com/org/repo-b/blob/main/docs/README.md).",
    );
    index_doc(
        repo_b.path(),
        "README.md",
        "# Server\nUnique combined target phrase.",
    );
    std::fs::create_dir_all(repo_b.path().join(".git")).unwrap();
    std::fs::write(
        repo_b.path().join(".git/config"),
        "[remote \"origin\"]\nurl = https://github.com/org/repo-b.git\n",
    )
    .unwrap();
    index_doc(repo_a.path(), "LOCAL.md", "# Local target");
    {
        let store = DocStore::open(&repo_a.path().join(".infigraph/docs.kuzu")).unwrap();
        store
            .create_link("README.md", "LOCAL.md", "LOCAL.md", "local")
            .unwrap();
        store
            .upsert_source("wiki", "confluence", "https://wiki", "SPACE")
            .unwrap();
        store.link_doc_to_source("README.md", "wiki").unwrap();
    }
    let registry = registry(repo_a.path(), repo_b.path());
    registry.save().unwrap();

    assert!(!has_combined_docs("fleet"));
    let first = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(first.documents, 3);
    assert_eq!(first.links, 2);
    assert_eq!(first.intra_repo_links, 1);
    assert_eq!(first.cross_repo_links, 1);
    assert_eq!(first.sources, 1);
    assert_eq!(first.embeddings, 3);
    assert!(has_combined_docs("fleet"));
    let first_generation = combined_docs_path("fleet").unwrap();

    let ids = combined_doc_query("fleet", "MATCH (d:Document) RETURN d.id").unwrap();
    let ids: Vec<_> = ids.into_iter().map(|row| row[0].clone()).collect();
    assert!(ids.contains(&"[repo-a]::README.md".to_string()));
    assert!(ids.contains(&"[server]::README.md".to_string()));

    let links = combined_doc_query(
        "fleet",
        "MATCH (a:Document)-[r:LINKS_TO]->(b:Document) \
         RETURN a.id, b.id, r.link_type",
    )
    .unwrap();
    assert!(links.contains(&vec![
        "[repo-a]::README.md".to_string(),
        "[repo-a]::LOCAL.md".to_string(),
        "local".to_string(),
    ]));
    assert!(links.contains(&vec![
        "[repo-a]::README.md".to_string(),
        "[server]::README.md".to_string(),
        "cross_repo".to_string(),
    ]));

    let sources = combined_doc_query(
        "fleet",
        "MATCH (d:Document)-[:FROM_SOURCE]->(s:Source) RETURN d.id, s.id",
    )
    .unwrap();
    assert_eq!(
        sources,
        vec![vec![
            "[repo-a]::README.md".to_string(),
            "[repo-a]::wiki".to_string(),
        ]]
    );

    let results = combined_doc_search("fleet", "unique combined target phrase", 5, 0.0).unwrap();
    assert_eq!(results[0].doc_file, "[server]::README.md");

    let embeddings_path = combined_docs_path("fleet")
        .unwrap()
        .parent()
        .unwrap()
        .join("docs_embeddings.bin");
    let embeddings = load_embeddings(&embeddings_path).unwrap();
    assert!(embeddings_path
        .parent()
        .unwrap()
        .join("docs_hnsw_index.usearch")
        .exists());
    assert!(embeddings
        .iter()
        .any(|(id, _)| id.starts_with("[repo-a]::README.md::chunk_")));
    assert!(embeddings
        .iter()
        .any(|(id, _)| id.starts_with("[server]::README.md::chunk_")));
    assert!(
        !combined_doc_search("fleet", "unique combined target phrase", 5, 1.0)
            .unwrap()
            .is_empty()
    );

    index_doc(
        repo_b.path(),
        "README.md",
        "# Server\nWatcher refreshed phrase.",
    );
    assert_eq!(schedule_group_doc_refresh(repo_b.path()).unwrap(), 1);
    let mut watcher_refreshed = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if combined_doc_search("fleet", "watcher refreshed phrase", 5, 0.0)
            .unwrap()
            .iter()
            .any(|result| result.doc_file == "[server]::README.md")
        {
            watcher_refreshed = true;
            break;
        }
    }
    assert!(watcher_refreshed);

    let second = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(first, second);
    let second_generation = combined_docs_path("fleet").unwrap();
    assert_ne!(first_generation, second_generation);

    {
        let store = DocStore::open(&repo_a.path().join(".infigraph/docs.kuzu")).unwrap();
        store.delete_docs_by_ids(&["LOCAL.md"]).unwrap();
    }
    let after_delete = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(after_delete.documents, 2);
    assert_eq!(after_delete.links, 1);
    assert_eq!(after_delete.intra_repo_links, 0);
    assert_eq!(after_delete.cross_repo_links, 1);
    assert_eq!(after_delete.embeddings, 2);
    let active_before_failure = combined_docs_path("fleet").unwrap();

    let repo_b_embeddings = repo_b.path().join(".infigraph/docs_embeddings.bin");
    let id = load_embeddings(&repo_b_embeddings).unwrap()[0].0.clone();
    save_embeddings(&repo_b_embeddings, &[(id, vec![0.2; 3])]).unwrap();
    let error = build_combined_docs(&registry, "fleet").unwrap_err();
    assert!(error.to_string().contains("dimension mismatch"));
    assert_eq!(combined_docs_path("fleet").unwrap(), active_before_failure);

    let ids_after_failed_rebuild =
        combined_doc_query("fleet", "MATCH (d:Document) RETURN d.id").unwrap();
    assert_eq!(
        ids_after_failed_rebuild.len(),
        2,
        "failed rebuild must preserve the active combined store"
    );

    std::fs::write(&repo_b_embeddings, b"corrupt").unwrap();
    let error = build_combined_docs(&registry, "fleet").unwrap_err();
    assert!(error
        .to_string()
        .contains("invalid document embeddings for repository 'server'"));
    assert_eq!(
        combined_doc_query("fleet", "MATCH (d:Document) RETURN d.id")
            .unwrap()
            .len(),
        2
    );

    std::fs::remove_file(&repo_b_embeddings).unwrap();
    let missing_embeddings = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(missing_embeddings.documents, 2);
    assert_eq!(missing_embeddings.embeddings, 1);

    let repo_b_store = repo_b.path().join(".infigraph/docs.kuzu");
    std::fs::remove_file(&repo_b_store).unwrap();
    let missing_store = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(missing_store.documents, 1);

    drop(DocStore::open(&repo_b_store).unwrap());
    let empty_store = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(empty_store.documents, 1);

    std::fs::remove_file(&repo_b_store).unwrap();
    std::fs::write(&repo_b_store, b"corrupt").unwrap();
    assert!(build_combined_docs(&registry, "fleet").is_err());
    assert_eq!(
        combined_doc_query("fleet", "MATCH (d:Document) RETURN d.id")
            .unwrap()
            .len(),
        1
    );

    if let Some(home) = old_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(threshold) = old_hnsw_threshold {
        std::env::set_var("INFIGRAPH_DOC_HNSW_THRESHOLD", threshold);
    } else {
        std::env::remove_var("INFIGRAPH_DOC_HNSW_THRESHOLD");
    }
}

fn single_repo_registry(repo: &std::path::Path) -> Registry {
    let mut repos = HashMap::new();
    repos.insert(
        "solo".to_string(),
        RepoEntry {
            name: "solo".to_string(),
            path: repo.to_path_buf(),
            languages: Vec::new(),
            symbol_count: 0,
            module_count: 0,
            last_indexed_commit: None,
        },
    );
    let mut groups = HashMap::new();
    groups.insert(
        "single".to_string(),
        Group {
            name: "single".to_string(),
            org: String::new(),
            repos: vec!["solo".to_string()],
            contracts: Vec::new(),
        },
    );
    Registry { repos, groups }
}

#[test]
fn combined_docs_single_repo_group() {
    let _guard = COMBINED_DOCS_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let repo = tempfile::tempdir().unwrap();
    index_doc(
        repo.path(),
        "README.md",
        "# Solo Repo\nSingle repo content.",
    );
    index_doc(
        repo.path(),
        "docs/guide.md",
        "# Guide\nSetup instructions for solo.",
    );

    let registry = single_repo_registry(repo.path());
    registry.save().unwrap();

    let stats = build_combined_docs(&registry, "single").unwrap();
    assert_eq!(stats.documents, 2);
    assert_eq!(stats.cross_repo_links, 0);
    assert!(has_combined_docs("single"));

    let ids = combined_doc_query("single", "MATCH (d:Document) RETURN d.id").unwrap();
    let ids: Vec<String> = ids.into_iter().map(|row| row[0].clone()).collect();
    assert!(ids.contains(&"[solo]::README.md".to_string()));
    assert!(ids.contains(&"[solo]::docs/guide.md".to_string()));

    let results = combined_doc_search("single", "setup instructions solo", 5, 0.0).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_file, "[solo]::docs/guide.md");

    if let Some(h) = old_home {
        std::env::set_var("HOME", h);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn combined_docs_conflicting_doc_ids_across_repos() {
    let _guard = COMBINED_DOCS_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let repo_a = tempfile::tempdir().unwrap();
    let repo_b = tempfile::tempdir().unwrap();

    index_doc(repo_a.path(), "README.md", "# Alpha readme unique.");
    index_doc(
        repo_a.path(),
        "docs/setup.md",
        "# Alpha setup unique guide.",
    );
    index_doc(repo_b.path(), "README.md", "# Bravo readme unique.");
    index_doc(
        repo_b.path(),
        "docs/setup.md",
        "# Bravo setup unique guide.",
    );

    let registry = registry(repo_a.path(), repo_b.path());
    registry.save().unwrap();

    let stats = build_combined_docs(&registry, "fleet").unwrap();
    assert_eq!(
        stats.documents, 4,
        "all 4 docs present despite same filenames"
    );

    let ids = combined_doc_query("fleet", "MATCH (d:Document) RETURN d.id ORDER BY d.id").unwrap();
    let ids: Vec<String> = ids.into_iter().map(|row| row[0].clone()).collect();
    assert!(ids.contains(&"[repo-a]::README.md".to_string()));
    assert!(ids.contains(&"[repo-a]::docs/setup.md".to_string()));
    assert!(ids.contains(&"[server]::README.md".to_string()));
    assert!(ids.contains(&"[server]::docs/setup.md".to_string()));

    let results = combined_doc_search("fleet", "alpha readme unique", 5, 0.0).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_file, "[repo-a]::README.md");

    let results = combined_doc_search("fleet", "bravo setup unique", 5, 0.0).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_file, "[server]::docs/setup.md");

    if let Some(h) = old_home {
        std::env::set_var("HOME", h);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn combined_docs_nonexistent_group() {
    let _guard = COMBINED_DOCS_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let repo = tempfile::tempdir().unwrap();
    let registry = single_repo_registry(repo.path());
    registry.save().unwrap();

    let err = build_combined_docs(&registry, "nonexistent").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected 'not found' error, got: {}",
        err
    );

    assert!(!has_combined_docs("nonexistent"));

    // search/query on a group that was never built — open_combined_docs errors
    let query_err = combined_doc_query("nonexistent", "MATCH (d:Document) RETURN d.id");
    assert!(query_err.is_err(), "query on unbuilt group should fail");

    if let Some(h) = old_home {
        std::env::set_var("HOME", h);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn combined_docs_auto_recovery_on_corrupt_store() {
    let _guard = COMBINED_DOCS_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    let old_hnsw_threshold = std::env::var_os("INFIGRAPH_DOC_HNSW_THRESHOLD");
    std::env::set_var("HOME", home.path());
    std::env::set_var("INFIGRAPH_DOC_HNSW_THRESHOLD", "1");

    let repo = tempfile::tempdir().unwrap();
    index_doc(repo.path(), "README.md", "# Recovery test content.");

    let registry = single_repo_registry(repo.path());
    registry.save().unwrap();

    let stats = build_combined_docs(&registry, "single").unwrap();
    assert_eq!(stats.documents, 1);
    assert!(has_combined_docs("single"));

    // Corrupt the kuzu DB by replacing it with garbage
    let db_path = combined_docs_path("single").unwrap();
    if db_path.is_dir() {
        std::fs::remove_dir_all(&db_path).unwrap();
    } else if db_path.exists() {
        std::fs::remove_file(&db_path).unwrap();
    }
    std::fs::write(&db_path, b"corrupt kuzu database").unwrap();

    // Search on corrupt store should fail but trigger auto-recovery
    let search_err = combined_doc_search("single", "recovery", 5, 0.0);
    assert!(search_err.is_err(), "search on corrupt store should error");

    // Corrupt generation should be wiped
    assert!(
        !db_path.exists(),
        "corrupt generation dir should be wiped after failed search"
    );

    // Wait for background rebuild to complete
    let mut rebuilt = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        if has_combined_docs("single") {
            rebuilt = true;
            break;
        }
    }
    assert!(
        rebuilt,
        "auto-recovery rebuild should restore combined docs"
    );

    // Verify rebuilt store is functional
    let results = combined_doc_search("single", "recovery test content", 5, 0.0).unwrap();
    assert!(!results.is_empty(), "rebuilt store should be searchable");

    if let Some(home) = old_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(threshold) = old_hnsw_threshold {
        std::env::set_var("INFIGRAPH_DOC_HNSW_THRESHOLD", threshold);
    } else {
        std::env::remove_var("INFIGRAPH_DOC_HNSW_THRESHOLD");
    }
}
