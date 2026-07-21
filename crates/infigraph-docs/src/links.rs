use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::backend::DocBackend;
use crate::extract::ExtractedDoc;

#[derive(Debug)]
pub struct DocLink {
    pub url: String,
    pub link_type: String,
    pub target_doc_id: Option<String>,
}

pub fn extract_and_link_doc(
    store: &dyn DocBackend,
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

pub fn extract_links(text: &str, doc_file: &str) -> Vec<DocLink> {
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

    // GitHub/GitLab blob URLs pointing to docs — resolve to target doc path
    if (url.contains("/blob/") || url.contains("/-/blob/"))
        && (url.starts_with("http://") || url.starts_with("https://"))
    {
        if let Some(doc_path) = extract_doc_path_from_url(url) {
            return DocLink {
                url: url.to_string(),
                link_type: "github".to_string(),
                target_doc_id: Some(doc_path),
            };
        }
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
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Resolve a link URL to an absolute path. Returns the resolved (non-canonicalized)
/// path if the file exists. Caller should canonicalize if needed for comparison.
pub(crate) fn resolve_link_to_abs_path(url: &str, doc_abs_path: &Path) -> Option<PathBuf> {
    let path = url.split('#').next().unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    if path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("//")
        || path.contains("confluence")
        || path.contains("atlassian")
        || path.contains("jira")
    {
        return None;
    }
    let parent = doc_abs_path.parent()?;
    let resolved = parent.join(path);
    if resolved.exists() {
        Some(resolved)
    } else {
        None
    }
}

fn extract_confluence_page_id(url: &str) -> Option<String> {
    // /wiki/spaces/SPACE/pages/PAGEID/...
    if url.contains("/pages/") {
        let parts: Vec<&str> = url.split('/').collect();
        if let Some(pages_idx) = parts.iter().position(|&p| p == "pages") {
            if let Some(id) = parts.get(pages_idx + 1) {
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                    if let Some(spaces_idx) = parts.iter().position(|&p| p == "spaces") {
                        if let Some(space) = parts.get(spaces_idx + 1) {
                            if !space.is_empty() && *space != "pages" {
                                return Some(format!("confluence://{}/{}", space, id));
                            }
                        }
                    }
                }
            }
        }
    }
    // ?pageId=12345 — no space info available
    None
}

/// Extract a file path from a GitHub/GitLab blob URL.
pub fn extract_doc_path_from_url(url: &str) -> Option<String> {
    // GitHub: https://github.com/org/repo/blob/branch/path/to/file.md
    if let Some(idx) = url.find("/blob/") {
        let after_blob = &url[idx + 6..];
        if let Some(slash) = after_blob.find('/') {
            let path = &after_blob[slash + 1..];
            if !path.is_empty() {
                return Some(
                    path.split('?')
                        .next()
                        .unwrap_or(path)
                        .split('#')
                        .next()
                        .unwrap_or(path)
                        .to_string(),
                );
            }
        }
    }
    // GitLab: /-/blob/branch/path
    if let Some(idx) = url.find("/-/blob/") {
        let after = &url[idx + 8..];
        if let Some(slash) = after.find('/') {
            let path = &after[slash + 1..];
            if !path.is_empty() {
                return Some(
                    path.split('?')
                        .next()
                        .unwrap_or(path)
                        .split('#')
                        .next()
                        .unwrap_or(path)
                        .to_string(),
                );
            }
        }
    }
    None
}

/// Extract the repository name from a GitHub/GitLab URL.
/// e.g. "https://github.com/org/my-repo/blob/main/docs/foo.md" → Some("my-repo")
pub fn extract_repo_from_url(url: &str) -> Option<String> {
    let without_protocol = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    for marker in ["/-/blob/", "/blob/"] {
        if let Some(idx) = without_protocol.find(marker) {
            let repo_path = &without_protocol[..idx];
            return repo_path
                .rsplit('/')
                .next()
                .filter(|repo| !repo.is_empty())
                .map(str::to_string);
        }
    }

    // Strip protocol
    let rest = without_protocol;
    // Skip host: github.com/org/repo/... or github.intuit.com/org/repo/...
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    // parts: [host, org, repo, "blob"|"tree"|..., rest]
    if parts.len() >= 3 {
        let repo = parts[2];
        if !repo.is_empty() {
            return Some(repo.to_string());
        }
    }
    None
}

pub fn resolve_doc_id(doc_path: &str, doc_ids: &HashSet<String>) -> Option<String> {
    if doc_ids.contains(doc_path) {
        return Some(doc_path.to_string());
    }
    let mut matches: Vec<_> = doc_ids
        .iter()
        .filter(|id| {
            id.strip_suffix(doc_path)
                .is_some_and(|prefix| prefix.ends_with('/'))
                || doc_path
                    .strip_suffix(id.as_str())
                    .is_some_and(|prefix| prefix.ends_with('/'))
        })
        .collect();
    matches.sort_by_key(|id| std::cmp::Reverse(id.len()));
    if matches.len() > 1 && matches[0].len() == matches[1].len() {
        return None;
    }
    matches.first().map(|id| (*id).clone())
}

/// Create LINKS_TO edges from manifest files to indexed docs based on doc_urls.
/// Creates a Document node for the manifest file if one doesn't already exist.
pub fn link_manifest_doc_urls(
    store: &dyn DocBackend,
    manifest_file: &str,
    doc_urls: &[String],
    all_doc_ids: &HashSet<String>,
) {
    if doc_urls.is_empty() {
        return;
    }
    // Ensure manifest has a Document node so LINKS_TO edges can be created
    let _ = store.ensure_document_node(manifest_file);

    for url in doc_urls {
        // Try confluence page ID match
        if let Some(conf_id) = extract_confluence_page_id(url) {
            if all_doc_ids.contains(&conf_id) {
                let _ = store.create_link(manifest_file, &conf_id, url, "manifest_ref");
            }
            continue;
        }
        // Try GitHub/GitLab file path extraction
        if let Some(doc_path) = extract_doc_path_from_url(url) {
            if let Some(doc_id) = resolve_doc_id(&doc_path, all_doc_ids) {
                let _ = store.create_link(manifest_file, &doc_id, url, "manifest_ref");
            }
        }
    }
}
