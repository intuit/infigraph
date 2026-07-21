use std::collections::{HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use infigraph_docs::backend::DocBackend;
use infigraph_docs::chunk::{chunk_document, Chunk, ChunkStrategy};
use infigraph_docs::extract::{DocFormat, ExtractedDoc};

use crate::client::{ConfluenceClient, ConfluencePage};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SyncCursor {
    pub last_synced: String,
    pub source_id: String,
    pub space_key: String,
    pub base_url: String,
    pub page_ids: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CrawlOptions {
    pub follow_links: bool,
    pub follow_depth: usize,
    pub max_pages: usize,
    pub same_space_only: bool,
}

impl CrawlOptions {
    pub fn default_follow() -> Self {
        Self {
            follow_links: true,
            follow_depth: 1,
            max_pages: 100,
            same_space_only: true,
        }
    }

    pub fn no_follow() -> Self {
        Self {
            follow_links: false,
            follow_depth: 0,
            max_pages: 0,
            same_space_only: true,
        }
    }
}

#[derive(Debug)]
pub struct SyncResult {
    pub pages_fetched: usize,
    pub pages_indexed: usize,
    pub pages_deleted: usize,
    pub chunks_created: usize,
    pub links_created: usize,
}

pub struct ConfluenceSync {
    client: ConfluenceClient,
    space_key: String,
    source_id: String,
}

#[derive(Debug, Clone)]
struct ParsedPage {
    content: String,
    links: Vec<PageLink>,
}

#[derive(Debug, Clone)]
struct PageLink {
    page_id: Option<String>,
    url: String,
    link_type: String,
}

impl ConfluenceSync {
    pub fn new(client: ConfluenceClient, space_key: &str) -> Self {
        let source_id = format!("confluence::{}", space_key);
        Self {
            client,
            space_key: space_key.to_string(),
            source_id,
        }
    }

    pub fn sync(
        &self,
        store: &dyn DocBackend,
        root: &Path,
        page_ids: Option<&[String]>,
    ) -> Result<SyncResult> {
        self.sync_with_options(store, root, page_ids, &CrawlOptions::no_follow())
    }

    pub fn sync_with_options(
        &self,
        store: &dyn DocBackend,
        root: &Path,
        page_ids: Option<&[String]>,
        crawl: &CrawlOptions,
    ) -> Result<SyncResult> {
        let cursor_path = root.join(".infigraph").join("confluence_sync.json");
        let cursor = load_cursor(&cursor_path);

        store.upsert_source(
            &self.source_id,
            "confluence",
            self.client.base_url(),
            &self.space_key,
        )?;

        let seed_pages = self.fetch_pages(page_ids, cursor.as_ref())?;

        let (all_pages, link_map) = if crawl.follow_links {
            self.crawl_links(&seed_pages, crawl)?
        } else {
            let link_map: Vec<(String, Vec<PageLink>)> = Vec::new();
            (seed_pages, link_map)
        };

        let fetched = all_pages.len();
        let (docs, all_chunks, page_links) = self.convert_pages(&all_pages);
        let indexed = docs.len();
        let chunks_created = all_chunks.len();

        if !docs.is_empty() {
            let doc_refs: Vec<&ExtractedDoc> = docs.iter().collect();
            let chunk_refs: Vec<&Chunk> = all_chunks.iter().collect();
            store.upsert_docs(&doc_refs, &chunk_refs)?;

            for doc in &docs {
                store.link_doc_to_source(&doc.file, &self.source_id)?;
            }
        }

        if !all_chunks.is_empty() {
            let chunk_refs: Vec<&Chunk> = all_chunks.iter().collect();
            let changed_files: Vec<&str> = docs.iter().map(|d| d.file.as_str()).collect();
            infigraph_docs::embed::update_doc_embeddings(store, root, &chunk_refs, &changed_files)?;
        }

        // Create LINKS_TO edges
        let mut links_created = 0;
        let all_link_data: Vec<(String, Vec<PageLink>)> =
            page_links.into_iter().chain(link_map).collect();

        let indexed_ids: HashSet<&str> = docs.iter().map(|d| d.file.as_str()).collect();

        for (from_file_id, links) in &all_link_data {
            if !indexed_ids.contains(from_file_id.as_str()) {
                continue;
            }
            store.delete_links_from(from_file_id)?;
            for link in links {
                if let Some(ref pid) = link.page_id {
                    let to_file_id = format!("confluence://{}/{}", self.space_key, pid);
                    if indexed_ids.contains(to_file_id.as_str()) {
                        store.create_link(from_file_id, &to_file_id, &link.url, &link.link_type)?;
                        links_created += 1;
                    }
                }
            }
        }

        // Cross-link: confluence pages → local docs
        let all_doc_hashes = store.get_doc_hashes().unwrap_or_default();
        let all_doc_ids: Vec<&str> = all_doc_hashes.keys().map(|s| s.as_str()).collect();
        for (from_file_id, links) in &all_link_data {
            if !from_file_id.starts_with("confluence://") {
                continue;
            }
            for link in links {
                if link.link_type != "external" {
                    continue;
                }
                if let Some(doc_path) = extract_doc_path_from_url(&link.url) {
                    if let Some(target) = all_doc_ids.iter().find(|id| id.ends_with(&doc_path)) {
                        if *target != from_file_id.as_str() {
                            let _ = store.create_link(from_file_id, target, &link.url, "cross_ref");
                            links_created += 1;
                        }
                    }
                }
            }
        }

        let deleted = self.remove_deleted_pages(store, page_ids)?;

        let remote_ids: Vec<String> = all_pages.iter().map(|p| p.id.clone()).collect();
        save_cursor(
            &cursor_path,
            &SyncCursor {
                last_synced: chrono::Utc::now().to_rfc3339(),
                source_id: self.source_id.clone(),
                space_key: self.space_key.clone(),
                base_url: self.client.base_url().to_string(),
                page_ids: if let Some(ids) = page_ids {
                    ids.to_vec()
                } else {
                    remote_ids
                },
            },
        )?;

        Ok(SyncResult {
            pages_fetched: fetched,
            pages_indexed: indexed,
            pages_deleted: deleted,
            chunks_created,
            links_created,
        })
    }

    #[allow(clippy::type_complexity)]
    fn crawl_links(
        &self,
        seed_pages: &[ConfluencePage],
        crawl: &CrawlOptions,
    ) -> Result<(Vec<ConfluencePage>, Vec<(String, Vec<PageLink>)>)> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut all_pages: Vec<ConfluencePage> = Vec::new();
        let mut all_links: Vec<(String, Vec<PageLink>)> = Vec::new();

        for page in seed_pages {
            visited.insert(page.id.clone());
            queue.push_back((page.id.clone(), 0));
            all_pages.push(page.clone());
        }

        while let Some((page_id, depth)) = queue.pop_front() {
            if all_pages.len() >= crawl.max_pages {
                eprintln!("Crawl: hit max_pages cap ({}), stopping", crawl.max_pages);
                break;
            }

            let page = if depth == 0 {
                all_pages.iter().find(|p| p.id == page_id).cloned()
            } else {
                match self.client.get_page(&page_id) {
                    Ok(p) => {
                        all_pages.push(p.clone());
                        Some(p)
                    }
                    Err(e) => {
                        eprintln!("Crawl: failed to fetch page {}: {}", page_id, e);
                        continue;
                    }
                }
            };

            let Some(page) = page else { continue };
            let parsed = parse_confluence_html(&page);
            let file_id = format!("confluence://{}/{}", self.space_key, page.id);
            all_links.push((file_id, parsed.links.clone()));

            if depth >= crawl.follow_depth {
                continue;
            }

            for link in &parsed.links {
                if let Some(ref linked_id) = link.page_id {
                    if visited.contains(linked_id) {
                        continue;
                    }
                    if crawl.same_space_only && link.link_type == "external" {
                        continue;
                    }
                    visited.insert(linked_id.clone());
                    queue.push_back((linked_id.clone(), depth + 1));
                    eprintln!("Crawl: queued page {} (depth {})", linked_id, depth + 1);
                }
            }
        }

        Ok((all_pages, all_links))
    }

    fn fetch_pages(
        &self,
        page_ids: Option<&[String]>,
        cursor: Option<&SyncCursor>,
    ) -> Result<Vec<ConfluencePage>> {
        if let Some(ids) = page_ids {
            let mut pages = Vec::new();
            for id in ids {
                match self.client.get_page(id) {
                    Ok(page) => pages.push(page),
                    Err(e) => eprintln!("Warning: failed to fetch page {}: {}", id, e),
                }
            }
            return Ok(pages);
        }

        if let Some(c) = cursor {
            let pages =
                self.client
                    .get_pages_modified_since(&self.space_key, &c.last_synced, 1000)?;
            if !pages.is_empty() {
                return Ok(pages);
            }
        }

        self.client.get_pages_in_space(&self.space_key, 1000)
    }

    #[allow(clippy::type_complexity)]
    fn convert_pages(
        &self,
        pages: &[ConfluencePage],
    ) -> (Vec<ExtractedDoc>, Vec<Chunk>, Vec<(String, Vec<PageLink>)>) {
        let mut docs = Vec::new();
        let mut all_chunks = Vec::new();
        let mut page_links = Vec::new();

        for page in pages {
            let parsed = parse_confluence_html(page);
            if parsed.content.is_empty() {
                continue;
            }

            let file_id = format!("confluence://{}/{}", self.space_key, page.id);
            let hash = {
                let mut h = Sha256::new();
                h.update(parsed.content.as_bytes());
                format!("{:x}", h.finalize())
            };

            let doc = ExtractedDoc {
                file: file_id.clone(),
                title: Some(page.title.clone()),
                content_hash: hash.clone(),
                format: DocFormat::Markdown,
                text: parsed.content,
                page_count: Some(1),
            };

            let chunks = chunk_document(&doc, &file_id, &hash, ChunkStrategy::HeadingBounded);
            all_chunks.extend(chunks);
            page_links.push((file_id, parsed.links));
            docs.push(doc);
        }

        (docs, all_chunks, page_links)
    }

    fn remove_deleted_pages(
        &self,
        store: &dyn DocBackend,
        page_ids: Option<&[String]>,
    ) -> Result<usize> {
        if page_ids.is_some() {
            return Ok(0);
        }

        let remote_ids = self.client.get_all_page_ids_in_space(&self.space_key)?;
        let remote_set: HashSet<String> = remote_ids.into_iter().collect();

        let existing_docs = store.get_docs_by_source(&self.source_id)?;
        let mut to_delete = Vec::new();

        for doc_id in &existing_docs {
            if let Some(page_id) = doc_id.strip_prefix(&format!("confluence://{}/", self.space_key))
            {
                if !remote_set.contains(page_id) {
                    to_delete.push(doc_id.as_str());
                }
            }
        }

        let count = to_delete.len();
        if !to_delete.is_empty() {
            store.delete_docs_by_ids(&to_delete)?;
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Confluence HTML parser — preserves all content types
// ---------------------------------------------------------------------------

fn parse_confluence_html(page: &ConfluencePage) -> ParsedPage {
    let html = if let Some(body) = &page.body {
        if let Some(view) = &body.view {
            if !view.value.is_empty() {
                &view.value
            } else if let Some(storage) = &body.storage {
                &storage.value
            } else {
                return ParsedPage {
                    content: String::new(),
                    links: Vec::new(),
                };
            }
        } else if let Some(storage) = &body.storage {
            &storage.value
        } else {
            return ParsedPage {
                content: String::new(),
                links: Vec::new(),
            };
        }
    } else {
        return ParsedPage {
            content: String::new(),
            links: Vec::new(),
        };
    };

    let mut parser = HtmlParser::new(html);
    parser.parse();
    let links = std::mem::take(&mut parser.links);
    ParsedPage {
        content: parser.finish(),
        links,
    }
}

struct HtmlParser<'a> {
    input: &'a str,
    pos: usize,
    out: String,
    links: Vec<PageLink>,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_header_row: bool,
    list_depth: usize,
    ordered_list_counters: Vec<usize>,
    in_code_block: bool,
    code_language: String,
    code_content: String,
    in_pre: bool,
    macro_name: String,
    in_macro: bool,
}

impl<'a> HtmlParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            out: String::new(),
            links: Vec::new(),
            in_table: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            in_header_row: false,
            list_depth: 0,
            ordered_list_counters: Vec::new(),
            in_code_block: false,
            code_language: String::new(),
            code_content: String::new(),
            in_pre: false,
            macro_name: String::new(),
            in_macro: false,
        }
    }

    fn parse(&mut self) {
        while self.pos < self.input.len() {
            if self.input[self.pos..].starts_with('<') {
                self.parse_tag();
            } else if self.input[self.pos..].starts_with('&') {
                self.parse_entity();
            } else {
                let ch = self.input[self.pos..].chars().next().unwrap();
                if self.in_code_block || self.in_pre {
                    self.code_content.push(ch);
                } else if self.in_table {
                    self.current_cell.push(ch);
                } else {
                    self.out.push(ch);
                }
                self.pos += ch.len_utf8();
            }
        }
    }

    fn parse_entity(&mut self) {
        let rest = &self.input[self.pos..];
        let end = rest.find(';').unwrap_or(0);
        if end == 0 {
            self.push_char('&');
            self.pos += 1;
            return;
        }
        let entity = &rest[..end + 1];
        let decoded = match entity {
            "&amp;" => "&",
            "&lt;" => "<",
            "&gt;" => ">",
            "&quot;" => "\"",
            "&#39;" | "&apos;" => "'",
            "&nbsp;" => " ",
            "&ndash;" => "–",
            "&mdash;" => "—",
            "&hellip;" => "…",
            "&rarr;" => "→",
            "&larr;" => "←",
            "&times;" => "×",
            "&bull;" => "•",
            _ => {
                if entity.starts_with("&#x") {
                    let hex = &entity[3..entity.len() - 1];
                    if let Ok(n) = u32::from_str_radix(hex, 16) {
                        if let Some(ch) = char::from_u32(n) {
                            self.push_char(ch);
                            self.pos += entity.len();
                            return;
                        }
                    }
                } else if entity.starts_with("&#") {
                    let num = &entity[2..entity.len() - 1];
                    if let Ok(n) = num.parse::<u32>() {
                        if let Some(ch) = char::from_u32(n) {
                            self.push_char(ch);
                            self.pos += entity.len();
                            return;
                        }
                    }
                }
                self.push_str(entity);
                self.pos += entity.len();
                return;
            }
        };
        self.push_str(decoded);
        self.pos += entity.len();
    }

    fn push_char(&mut self, ch: char) {
        if self.in_code_block || self.in_pre {
            self.code_content.push(ch);
        } else if self.in_table {
            self.current_cell.push(ch);
        } else {
            self.out.push(ch);
        }
    }

    fn push_str(&mut self, s: &str) {
        if self.in_code_block || self.in_pre {
            self.code_content.push_str(s);
        } else if self.in_table {
            self.current_cell.push_str(s);
        } else {
            self.out.push_str(s);
        }
    }

    fn parse_tag(&mut self) {
        let rest = &self.input[self.pos..];
        let end = match rest.find('>') {
            Some(e) => e,
            None => {
                self.pos = self.input.len();
                return;
            }
        };
        let tag_content = &rest[1..end];
        self.pos += end + 1;

        let is_closing = tag_content.starts_with('/');
        let tag_str = if is_closing {
            &tag_content[1..]
        } else {
            tag_content
        };

        let (tag_name, attrs) = split_tag(tag_str);
        let tag_lower = tag_name.to_lowercase();

        if is_closing {
            self.handle_close_tag(&tag_lower);
        } else {
            self.handle_open_tag(&tag_lower, attrs);
        }
    }

    fn handle_open_tag(&mut self, tag: &str, attrs: &str) {
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.ensure_newline();
                let level: usize = tag[1..].parse().unwrap_or(1);
                for _ in 0..level {
                    self.out.push('#');
                }
                self.out.push(' ');
            }
            "p" => self.ensure_blank_line(),
            "br" => self.push_char('\n'),
            "a" => {
                if let Some(href) = extract_attr(attrs, "href") {
                    let link = classify_link(&href);
                    self.links.push(link.clone());
                    match link.link_type.as_str() {
                        "confluence_page" => {
                            self.push_char('[');
                        }
                        "jira" => {
                            self.push_str("[JIRA: ");
                        }
                        _ => {
                            self.push_char('[');
                        }
                    }
                }
            }
            "img" => {
                let alt = extract_attr(attrs, "alt").unwrap_or_default();
                let src = extract_attr(attrs, "src").unwrap_or_default();
                if !alt.is_empty() || !src.is_empty() {
                    self.push_str(&format!("![{}]({})", alt, src));
                }
            }
            "ul" => {
                self.list_depth += 1;
                self.ensure_newline();
            }
            "ol" => {
                self.list_depth += 1;
                self.ordered_list_counters.push(0);
                self.ensure_newline();
            }
            "li" => {
                self.ensure_newline();
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                if !self.ordered_list_counters.is_empty() {
                    if let Some(counter) = self.ordered_list_counters.last_mut() {
                        *counter += 1;
                        let prefix = format!("{}{}. ", indent, counter);
                        self.push_str(&prefix);
                    }
                } else {
                    let prefix = format!("{}- ", indent);
                    self.push_str(&prefix);
                }
            }
            "table" => {
                self.in_table = true;
                self.table_rows.clear();
                self.ensure_blank_line();
            }
            "thead" => {
                self.in_header_row = true;
            }
            "tbody" => {
                self.in_header_row = false;
            }
            "tr" => {
                self.current_row.clear();
                self.current_cell.clear();
            }
            "th" => {
                self.current_cell.clear();
                self.in_header_row = true;
            }
            "td" => {
                self.current_cell.clear();
            }
            "pre" => {
                self.in_pre = true;
                self.code_content.clear();
            }
            "code" => {
                if self.in_pre {
                    self.in_code_block = true;
                    self.code_language = extract_attr(attrs, "class")
                        .map(|c| c.replace("language-", "").replace("confluence-", ""))
                        .unwrap_or_default();
                    self.code_content.clear();
                } else {
                    self.push_char('`');
                }
            }
            "div" | "ac:structured-macro" => {
                if let Some(name) = extract_attr(attrs, "ac:name")
                    .or_else(|| extract_attr(attrs, "data-macro-name"))
                {
                    self.in_macro = true;
                    self.macro_name = name.to_lowercase();
                    match self.macro_name.as_str() {
                        "info" | "note" | "warning" | "tip" => {
                            self.ensure_blank_line();
                            let label = self.macro_name.to_uppercase();
                            self.out.push_str(&format!("> **{}:** ", label));
                        }
                        "code" | "noformat" => {
                            self.in_code_block = true;
                            self.code_content.clear();
                            self.code_language =
                                extract_attr(attrs, "language").unwrap_or_default();
                        }
                        "expand" => {
                            self.ensure_blank_line();
                        }
                        "status" => {
                            let color = extract_attr(attrs, "colour")
                                .or_else(|| extract_attr(attrs, "color"))
                                .unwrap_or_default();
                            let title = extract_attr(attrs, "title").unwrap_or_default();
                            if !title.is_empty() {
                                self.push_str(&format!("[STATUS: {} ({})]", title, color));
                            }
                        }
                        "jira" => {
                            if let Some(key) = extract_attr(attrs, "key") {
                                self.push_str(&format!("[JIRA: {}]", key));
                                self.links.push(PageLink {
                                    page_id: None,
                                    url: key.to_string(),
                                    link_type: "jira".to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "ac:link" => {}
            "ri:user" => {
                if let Some(name) =
                    extract_attr(attrs, "ri:username").or_else(|| extract_attr(attrs, "ri:userkey"))
                {
                    self.push_str(&format!("@{}", name));
                }
            }
            "ri:page" => {
                if let Some(title) = extract_attr(attrs, "ri:content-title") {
                    self.push_str(&format!("[Page: {}]", title));
                }
            }
            "ri:attachment" => {
                if let Some(filename) = extract_attr(attrs, "ri:filename") {
                    self.push_str(&format!("[Attachment: {}]", filename));
                }
            }
            "hr" => {
                self.ensure_newline();
                self.out.push_str("---\n");
            }
            "strong" | "b" => self.push_str("**"),
            "em" | "i" => self.push_char('*'),
            "u" => self.push_str("__"),
            "s" | "del" | "strike" => self.push_str("~~"),
            "sup" => self.push_char('^'),
            "sub" => self.push_char('~'),
            "blockquote" => {
                self.ensure_newline();
                self.push_str("> ");
            }
            "time" => {
                if let Some(dt) = extract_attr(attrs, "datetime") {
                    self.push_str(&format!("[Date: {}]", dt));
                }
            }
            "ac:emoticon" => {
                if let Some(name) = extract_attr(attrs, "ac:name") {
                    self.push_str(&format!(":{}: ", name));
                }
            }
            _ => {}
        }
    }

    fn handle_close_tag(&mut self, tag: &str) {
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.push_char('\n');
            }
            "p" => self.ensure_blank_line(),
            "a" => {
                if let Some(last_link) = self.links.last() {
                    let url = last_link.url.clone();
                    self.push_str(&format!("]({})", url));
                }
            }
            "ul" => {
                self.list_depth = self.list_depth.saturating_sub(1);
                if self.list_depth == 0 {
                    self.ensure_newline();
                }
            }
            "ol" => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordered_list_counters.pop();
                if self.list_depth == 0 {
                    self.ensure_newline();
                }
            }
            "li" => {}
            "th" | "td" => {
                let cell = self.current_cell.trim().replace('\n', " ").to_string();
                self.current_row.push(cell);
                self.current_cell.clear();
            }
            "tr" => {
                if !self.current_row.is_empty() {
                    self.table_rows.push(self.current_row.clone());
                }
                self.current_row.clear();
            }
            "thead" => {
                self.in_header_row = false;
            }
            "table" => {
                self.in_table = false;
                self.render_table();
            }
            "code" => {
                if self.in_code_block && self.in_pre {
                    // handled by /pre
                } else {
                    self.push_char('`');
                }
            }
            "pre" => {
                self.in_pre = false;
                if self.in_code_block || !self.code_content.is_empty() {
                    self.in_code_block = false;
                    self.ensure_blank_line();
                    self.out.push_str(&format!("```{}\n", self.code_language));
                    self.out.push_str(self.code_content.trim());
                    self.out.push_str("\n```\n");
                    self.code_content.clear();
                    self.code_language.clear();
                }
            }
            "div" | "ac:structured-macro" if self.in_macro => {
                if self.in_code_block {
                    self.in_code_block = false;
                    self.ensure_blank_line();
                    self.out.push_str(&format!("```{}\n", self.code_language));
                    self.out.push_str(self.code_content.trim());
                    self.out.push_str("\n```\n");
                    self.code_content.clear();
                    self.code_language.clear();
                }
                self.in_macro = false;
                self.macro_name.clear();
            }
            "strong" | "b" => self.push_str("**"),
            "em" | "i" => self.push_char('*'),
            "u" => self.push_str("__"),
            "s" | "del" | "strike" => self.push_str("~~"),
            "sup" => self.push_char('^'),
            "sub" => self.push_char('~'),
            "blockquote" => self.ensure_newline(),
            _ => {}
        }
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        let max_cols = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if max_cols == 0 {
            return;
        }

        let mut widths = vec![3usize; max_cols];
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.len());
            }
        }

        self.ensure_blank_line();

        if let Some(header) = self.table_rows.first() {
            self.out.push('|');
            for (i, cell) in header.iter().enumerate() {
                let w = widths.get(i).copied().unwrap_or(3);
                self.out.push_str(&format!(" {:width$} |", cell, width = w));
            }
            for i in header.len()..max_cols {
                let w = widths.get(i).copied().unwrap_or(3);
                self.out.push_str(&format!(" {:width$} |", "", width = w));
            }
            self.out.push('\n');

            self.out.push('|');
            for w in &widths {
                self.out.push_str(&format!(" {} |", "-".repeat(*w)));
            }
            self.out.push('\n');
        }

        for row in self.table_rows.iter().skip(1) {
            self.out.push('|');
            for (i, cell) in row.iter().enumerate() {
                let w = widths.get(i).copied().unwrap_or(3);
                self.out.push_str(&format!(" {:width$} |", cell, width = w));
            }
            for i in row.len()..max_cols {
                let w = widths.get(i).copied().unwrap_or(3);
                self.out.push_str(&format!(" {:width$} |", "", width = w));
            }
            self.out.push('\n');
        }

        self.out.push('\n');
        self.table_rows.clear();
    }

    fn ensure_newline(&mut self) {
        if !self.out.ends_with('\n') && !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    fn ensure_blank_line(&mut self) {
        self.ensure_newline();
        if !self.out.ends_with("\n\n") && !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    fn finish(self) -> String {
        let mut result = String::new();
        let mut blank_count = 0;
        for line in self.out.lines() {
            if line.trim().is_empty() {
                blank_count += 1;
                if blank_count <= 2 {
                    result.push('\n');
                }
            } else {
                blank_count = 0;
                result.push_str(line);
                result.push('\n');
            }
        }
        result.trim().to_string()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn split_tag(s: &str) -> (&str, &str) {
    let s = s.trim_end_matches('/').trim();
    match s.find(|c: char| c.is_whitespace()) {
        Some(i) => (&s[..i], s[i..].trim()),
        None => (s, ""),
    }
}

fn extract_attr(attrs: &str, name: &str) -> Option<String> {
    let patterns = [format!("{}=\"", name), format!("{}='", name)];
    for pat in &patterns {
        if let Some(start) = attrs.find(pat.as_str()) {
            let val_start = start + pat.len();
            let quote = if pat.ends_with('"') { '"' } else { '\'' };
            if let Some(end) = attrs[val_start..].find(quote) {
                return Some(attrs[val_start..val_start + end].to_string());
            }
        }
    }
    None
}

fn classify_link(href: &str) -> PageLink {
    if href.contains("/pages/") && href.contains("/wiki/spaces/") {
        let parts: Vec<&str> = href.split('/').collect();
        if let Some(idx) = parts.iter().position(|&p| p == "pages") {
            if let Some(id) = parts.get(idx + 1) {
                if id.chars().all(|c| c.is_ascii_digit()) {
                    return PageLink {
                        page_id: Some(id.to_string()),
                        url: href.to_string(),
                        link_type: "confluence_page".to_string(),
                    };
                }
            }
        }
    }

    if href.contains("pageId=") {
        if let Some(id) = href.split("pageId=").nth(1) {
            let id = id.split('&').next().unwrap_or(id);
            if id.chars().all(|c| c.is_ascii_digit()) {
                return PageLink {
                    page_id: Some(id.to_string()),
                    url: href.to_string(),
                    link_type: "confluence_page".to_string(),
                };
            }
        }
    }

    if href.contains("/browse/") || href.contains("jira") {
        let key = href.rsplit('/').next().unwrap_or("");
        if key.contains('-')
            && key
                .split('-')
                .next()
                .map(|p| p.chars().all(|c| c.is_ascii_uppercase()))
                .unwrap_or(false)
        {
            return PageLink {
                page_id: None,
                url: href.to_string(),
                link_type: "jira".to_string(),
            };
        }
    }

    PageLink {
        page_id: None,
        url: href.to_string(),
        link_type: "external".to_string(),
    }
}

fn load_cursor(path: &Path) -> Option<SyncCursor> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_cursor(path: &Path, cursor: &SyncCursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cursor)?;
    std::fs::write(path, json).context("write sync cursor")?;
    Ok(())
}

/// Extract a file path from a URL (GitHub/GitLab blob links, raw relative paths).
pub(crate) fn extract_doc_path_from_url(url: &str) -> Option<String> {
    // GitHub: https://github.com/org/repo/blob/branch/path/to/file.md
    if let Some(idx) = url.find("/blob/") {
        let after_blob = &url[idx + 6..]; // skip "/blob/"
                                          // Skip the branch name (first path segment)
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
    // GitLab: /-/blob/branch/path or /-/tree/branch/path
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_doc_path_github() {
        assert_eq!(
            extract_doc_path_from_url("https://github.com/org/repo/blob/main/docs/README.md"),
            Some("docs/README.md".to_string())
        );
    }

    #[test]
    fn test_extract_doc_path_github_with_fragment() {
        assert_eq!(
            extract_doc_path_from_url(
                "https://github.com/org/repo/blob/main/docs/guide.md#section"
            ),
            Some("docs/guide.md".to_string())
        );
    }

    #[test]
    fn test_extract_doc_path_gitlab() {
        assert_eq!(
            extract_doc_path_from_url("https://gitlab.com/org/repo/-/blob/main/docs/README.md"),
            Some("docs/README.md".to_string())
        );
    }

    #[test]
    fn test_extract_doc_path_non_repo_url() {
        assert_eq!(
            extract_doc_path_from_url("https://example.com/random/page"),
            None
        );
    }

    #[test]
    fn test_extract_doc_path_no_path_after_branch() {
        assert_eq!(
            extract_doc_path_from_url("https://github.com/org/repo/blob/main"),
            None
        );
    }
}
