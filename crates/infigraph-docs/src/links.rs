use std::collections::HashSet;

use regex::Regex;

use crate::extract::ExtractedDoc;
use crate::store::DocStore;

#[derive(Debug)]
struct DocLink {
    url: String,
    link_type: String,
    target_doc_id: Option<String>,
}

pub fn extract_and_link_doc(
    store: &DocStore,
    doc: &ExtractedDoc,
    all_doc_ids: &HashSet<String>,
) {
    let links = extract_links(&doc.text, &doc.file);
    if links.is_empty() {
        return;
    }

    let _ = store.delete_links_from(&doc.file);

    for link in &links {
        if let Some(ref target) = link.target_doc_id {
            if all_doc_ids.contains(target) && target != &doc.file {
                let _ = store.create_link(&doc.file, target, &link.url, &link.link_type);
            }
        }
    }
}

fn extract_links(text: &str, doc_file: &str) -> Vec<DocLink> {
    let mut links = Vec::new();

    // Markdown links: [text](url)
    let md_link_re = Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap();
    for cap in md_link_re.captures_iter(text) {
        let url = cap[2].trim();
        if url.starts_with('#') {
            continue; // anchor-only
        }
        let classified = classify_doc_link(url, doc_file);
        links.push(classified);
    }

    // HTML links: <a href="...">
    let html_link_re = Regex::new(r#"<a\s[^>]*href=["']([^"']+)["']"#).unwrap();
    for cap in html_link_re.captures_iter(text) {
        let url = cap[1].trim();
        if url.starts_with('#') {
            continue;
        }
        let classified = classify_doc_link(url, doc_file);
        links.push(classified);
    }

    links
}

fn classify_doc_link(url: &str, doc_file: &str) -> DocLink {
    // Confluence page links
    if url.contains("/wiki/") || url.contains("confluence") || url.contains("atlassian") {
        let page_id = extract_confluence_page_id(url);
        return DocLink {
            url: url.to_string(),
            link_type: "confluence".to_string(),
            target_doc_id: page_id,
        };
    }

    // JIRA links
    if url.contains("/browse/") || url.contains("jira") {
        return DocLink {
            url: url.to_string(),
            link_type: "jira".to_string(),
            target_doc_id: None,
        };
    }

    // External URLs
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//") {
        return DocLink {
            url: url.to_string(),
            link_type: "external".to_string(),
            target_doc_id: None,
        };
    }

    // Relative path — resolve against current doc's directory
    let target = resolve_relative_path(url, doc_file);
    DocLink {
        url: url.to_string(),
        link_type: "local".to_string(),
        target_doc_id: Some(target),
    }
}

fn resolve_relative_path(url: &str, doc_file: &str) -> String {
    // Strip fragment
    let path = url.split('#').next().unwrap_or(url);
    // Strip query string
    let path = path.split('?').next().unwrap_or(path);

    if path.is_empty() {
        return doc_file.to_string();
    }

    // Get directory of current doc
    let dir = if let Some(idx) = doc_file.rfind('/') {
        &doc_file[..idx]
    } else {
        ""
    };

    // Resolve relative path components
    let full = if path.starts_with('/') || dir.is_empty() {
        path.to_string()
    } else {
        format!("{}/{}", dir, path)
    };

    // Normalize: collapse .. and .
    let mut parts: Vec<&str> = Vec::new();
    for component in full.split('/') {
        match component {
            "" | "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn extract_confluence_page_id(url: &str) -> Option<String> {
    // /wiki/spaces/SPACE/pages/PAGEID/...
    if url.contains("/pages/") {
        let parts: Vec<&str> = url.split('/').collect();
        if let Some(idx) = parts.iter().position(|&p| p == "pages") {
            if let Some(id) = parts.get(idx + 1) {
                if id.chars().all(|c| c.is_ascii_digit()) {
                    // Can't resolve to doc_id without knowing space — return None
                    // Confluence docs use confluence://SPACE/ID format
                    return None;
                }
            }
        }
    }
    if url.contains("pageId=") {
        if let Some(id) = url.split("pageId=").nth(1) {
            let id = id.split('&').next().unwrap_or(id);
            if id.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
        }
    }
    None
}
