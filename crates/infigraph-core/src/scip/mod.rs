use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::DataType;
use protobuf::Message;
use scip::types::{symbol_information, Index, SymbolRole};

use crate::graph::parquet_loader;
use crate::graph::store_util::{
    copy_edges_with_bad_record_retry, escape, extract_bad_copy_value, fwd_slash_path,
};
use crate::graph::GraphStore;
use crate::model::{Span, SymbolKind};

/// Import a SCIP index.scip file into the Infigraph graph store.
///
/// Matches SCIP definitions to existing tree-sitter symbols by (file, name)
/// and enriches them with compiler-grade type information. Builds cross-file
/// CALLS edges from SCIP references using an in-memory symbol map for speed.
pub fn import_scip_index(
    index_path: &Path,
    store: &GraphStore,
    project_root: Option<&Path>,
) -> Result<ImportStats> {
    let bytes = std::fs::read(index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;

    let index = Index::parse_from_bytes(&bytes)
        .with_context(|| format!("failed to parse SCIP index: {}", index_path.display()))?;

    let mut stats = ImportStats::default();
    let _lock = store.write_lock()?;
    let conn = store.connection()?;

    // Preflight disk headroom before any COPY/UNWIND write. Kuzu aborts the
    // whole process with an uncaught C++ exception on ENOSPC mid-transaction
    // rather than surfacing a Result -- this crashed sittir's SCIP import.
    if let Some(dir) = store.db_dir() {
        if let Err(shortfall) =
            crate::graph::store_util::check_disk_headroom(dir, bytes.len() as u64)
        {
            anyhow::bail!("Auto-SCIP: refusing to import -- {shortfall}");
        }
    }

    // Load learned pattern store for recording SCIP corrections
    let mut learned_store = project_root
        .map(crate::learned::LearnedStore::load)
        .unwrap_or_default();

    // Pre-load existing CALLS edges from tree-sitter resolution.
    // Used to detect when SCIP resolves differently (= a correction to learn from).
    let mut existing_calls: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    if project_root.is_some() {
        if let Ok(rows) = conn.query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id") {
            for row in rows {
                if row.len() < 2 {
                    continue;
                }
                let src = row[0].to_string().trim_matches('"').to_string();
                let tgt = row[1].to_string().trim_matches('"').to_string();
                existing_calls.entry(src).or_default().insert(tgt);
            }
        }
    }

    // Pre-load all symbols from graph into memory: (file, name) -> Vec<symbol_id>
    // and file -> sorted Vec<(start_line, end_line, symbol_id)> for containment lookup
    let mut file_name_to_ids: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut file_symbols: HashMap<String, Vec<(u32, u32, String)>> = HashMap::new();

    let q = "MATCH (s:Symbol) RETURN s.id, s.file, s.name, s.start_line, s.end_line";
    if let Ok(rows) = conn.query(q) {
        for row in rows {
            if row.len() < 5 {
                continue;
            }
            let sid = row[0].to_string().trim_matches('"').to_string();
            let sfile = row[1].to_string().trim_matches('"').to_string();
            let sname = row[2].to_string().trim_matches('"').to_string();
            let sstart: u32 = row[3].to_string().trim_matches('"').parse().unwrap_or(0);
            let send: u32 = row[4].to_string().trim_matches('"').parse().unwrap_or(0);

            file_name_to_ids
                .entry((sfile.clone(), sname))
                .or_default()
                .push(sid.clone());

            file_symbols
                .entry(sfile)
                .or_default()
                .push((sstart, send, sid));
        }
    }

    // Sort file_symbols by span size (smallest first) for containment lookup
    for syms in file_symbols.values_mut() {
        syms.sort_by_key(|(s, e, _)| *e as i64 - *s as i64);
    }

    // Build SCIP symbol -> definition file mapping (cross-file resolution)
    let mut scip_sym_to_file_name: HashMap<String, (String, String)> = HashMap::new();
    for doc in &index.documents {
        let file = &doc.relative_path;
        for occ in &doc.occurrences {
            if (occ.symbol_roles & SymbolRole::Definition as i32) == 0 {
                continue;
            }
            if occ.symbol.starts_with("local ") || occ.symbol.starts_with('<') {
                continue;
            }
            let name = scip_sym_to_name(&occ.symbol);
            scip_sym_to_file_name.insert(occ.symbol.clone(), (file.clone(), name));
        }
    }

    // Pass 1: collect enrichments and new symbols in memory
    let mut enrichments: Vec<(String, u32, u32, String)> = Vec::new();
    let mut new_symbols: Vec<(String, String, String, String, u32, u32, String)> = Vec::new();

    for doc in &index.documents {
        let file = &doc.relative_path;

        let sym_info_map: HashMap<&str, &scip::types::SymbolInformation> = doc
            .symbols
            .iter()
            .map(|si| (si.symbol.as_str(), si))
            .collect();

        for occ in &doc.occurrences {
            if (occ.symbol_roles & SymbolRole::Definition as i32) == 0 {
                continue;
            }
            let scip_sym = &occ.symbol;
            if scip_sym.starts_with("local ") || scip_sym.starts_with('<') {
                continue;
            }

            let name = scip_sym_to_name(scip_sym);
            let span = parse_range(&occ.range, file);
            let si = sym_info_map.get(scip_sym.as_str());
            let docstring = si
                .and_then(|s| s.documentation.first())
                .map(|s| s.as_str())
                .unwrap_or("");

            let key = (file.clone(), name.clone());
            if let Some(ids) = file_name_to_ids.get(&key) {
                for sid in ids {
                    enrichments.push((
                        sid.clone(),
                        span.start_line,
                        span.end_line,
                        docstring.to_string(),
                    ));
                    stats.symbols_enriched += 1;
                }
            } else {
                let kind = si
                    .map(|s| scip_kind_to_prism(&s.kind.enum_value_or_default()))
                    .unwrap_or(SymbolKind::Function);
                let sym_id = format!("{}::{}", file, name);
                new_symbols.push((
                    sym_id.clone(),
                    name.clone(),
                    kind.as_str().to_string(),
                    file.clone(),
                    span.start_line,
                    span.end_line,
                    docstring.to_string(),
                ));
                stats.symbols_added += 1;
                file_name_to_ids
                    .entry(key)
                    .or_default()
                    .push(sym_id.clone());
                file_symbols.entry(file.clone()).or_default().push((
                    span.start_line,
                    span.end_line,
                    sym_id,
                ));
            }
        }

        stats.files_processed += 1;
    }

    // Bulk insert new SCIP symbols via Parquet COPY FROM. A batch containing
    // two rows with the same id (e.g. two distinct SCIP symbols that
    // collapse to the same extracted name) is dropped down to one entry up
    // front -- an objectively-bad duplicate should never be sent to COPY at
    // all. On a COPY failure against an id that already exists in the graph,
    // drop that one record and retry rather than falling back to UNWIND for
    // the whole batch; only exhausting MAX_SYMBOL_RETRIES falls back.
    const CHUNK: usize = 2000;
    const MAX_SYMBOL_RETRIES: usize = 20;
    if !new_symbols.is_empty() {
        let tmp = std::env::temp_dir();
        let sym_pq = tmp.join("infigraph_scip_symbols.parquet");

        let mut seen_ids = std::collections::HashSet::with_capacity(new_symbols.len());
        let mut remaining: Vec<_> = new_symbols
            .into_iter()
            .filter(|(id, ..)| seen_ids.insert(id.clone()))
            .collect();

        for attempt in 0..MAX_SYMBOL_RETRIES {
            if remaining.is_empty() {
                break;
            }

            let ids: Vec<&str> = remaining.iter().map(|(id, ..)| id.as_str()).collect();
            let names: Vec<&str> = remaining
                .iter()
                .map(|(_, name, ..)| name.as_str())
                .collect();
            let kinds: Vec<&str> = remaining
                .iter()
                .map(|(_, _, kind, ..)| kind.as_str())
                .collect();
            let files: Vec<&str> = remaining
                .iter()
                .map(|(_, _, _, file, ..)| file.as_str())
                .collect();
            let start_lines: Vec<i64> = remaining
                .iter()
                .map(|(_, _, _, _, sl, ..)| *sl as i64)
                .collect();
            let end_lines: Vec<i64> = remaining.iter().map(|(.., el, _)| *el as i64).collect();
            let docs: Vec<&str> = remaining.iter().map(|(.., doc)| doc.as_str()).collect();
            let n = remaining.len();
            let empty_str: Vec<&str> = vec![""; n];
            let scip_lang: Vec<&str> = vec!["scip"; n];
            let pub_vis: Vec<&str> = vec!["public"; n];
            let zeros: Vec<i64> = vec![0; n];
            let empty_str2: Vec<&str> = vec![""; n];

            let pq_ok = parquet_loader::write_node_parquet(
                &sym_pq,
                &[
                    ("id", DataType::Utf8),
                    ("name", DataType::Utf8),
                    ("kind", DataType::Utf8),
                    ("file", DataType::Utf8),
                    ("start_line", DataType::Int64),
                    ("end_line", DataType::Int64),
                    ("signature_hash", DataType::Utf8),
                    ("language", DataType::Utf8),
                    ("visibility", DataType::Utf8),
                    ("parent", DataType::Utf8),
                    ("docstring", DataType::Utf8),
                    ("complexity", DataType::Int64),
                    ("parameters", DataType::Utf8),
                    ("return_type", DataType::Utf8),
                ],
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(names)),
                    Arc::new(StringArray::from(kinds)),
                    Arc::new(StringArray::from(files)),
                    Arc::new(Int64Array::from(start_lines)),
                    Arc::new(Int64Array::from(end_lines)),
                    Arc::new(StringArray::from(empty_str.clone())),
                    Arc::new(StringArray::from(scip_lang)),
                    Arc::new(StringArray::from(pub_vis)),
                    Arc::new(StringArray::from(empty_str)),
                    Arc::new(StringArray::from(docs)),
                    Arc::new(Int64Array::from(zeros)),
                    Arc::new(StringArray::from(empty_str2.clone())),
                    Arc::new(StringArray::from(empty_str2)),
                ],
            )
            .is_ok();

            if !pq_ok {
                eprintln!("Auto-SCIP: parquet write failed, falling back to UNWIND");
                break;
            }

            match conn.query(&format!(
                "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, parameters, return_type) FROM '{}'",
                fwd_slash_path(&sym_pq)
            )) {
                Ok(_) => {
                    remaining.clear();
                    break;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if let Some(bad) = extract_bad_copy_value(&msg) {
                        let before = remaining.len();
                        remaining.retain(|(id, ..)| id != bad);
                        if remaining.len() < before {
                            eprintln!(
                                "Auto-SCIP: COPY Symbol dropped bad-PK record (attempt {}/{MAX_SYMBOL_RETRIES}), retrying",
                                attempt + 1
                            );
                            continue;
                        }
                    }
                    eprintln!("Auto-SCIP: COPY Symbol failed ({e}), falling back to UNWIND");
                    break;
                }
            }
        }

        if !remaining.is_empty() {
            for chunk in remaining.chunks(CHUNK) {
                let rows: Vec<String> = chunk
                    .iter()
                    .map(|(id, name, kind, file, start, end, doc)| {
                        format!(
                            "{{id: '{}', name: '{}', kind: '{}', file: '{}', sl: {}, el: {}, doc: '{}'}}",
                            escape(id),
                            escape(name),
                            escape(kind),
                            escape(file),
                            start,
                            end,
                            escape(doc)
                        )
                    })
                    .collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS s CREATE (:Symbol {{id: s.id, name: s.name, kind: s.kind, file: s.file, start_line: s.sl, end_line: s.el, signature_hash: '', language: 'scip', visibility: 'public', parent: '', docstring: s.doc, complexity: 0, parameters: '', return_type: ''}})",
                    rows.join(", ")
                ));
            }
        }
        let _ = std::fs::remove_file(&sym_pq);
    }

    // Bulk write enrichments via UNWIND (updates can't use COPY FROM)
    for chunk in enrichments.chunks(CHUNK) {
        let rows: Vec<String> = chunk
            .iter()
            .map(|(id, start, end, doc)| {
                format!(
                    "{{id: '{}', sl: {}, el: {}, doc: '{}'}}",
                    escape(id),
                    start,
                    end,
                    escape(doc)
                )
            })
            .collect();
        let _ = conn.query(&format!(
            "UNWIND [{}] AS e MATCH (s:Symbol) WHERE s.id = e.id SET s.start_line = e.sl, s.end_line = e.el, s.docstring = e.doc",
            rows.join(", ")
        ));
    }

    // Pass 2: build CALLS edges from references (all in-memory)
    let mut calls_to_create: Vec<(String, String)> = Vec::new();
    let mut seen_edges: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();

    for doc in &index.documents {
        let file = &doc.relative_path;

        for occ in &doc.occurrences {
            if (occ.symbol_roles & SymbolRole::Definition as i32) != 0 {
                continue;
            }
            if occ.symbol.starts_with("local ") || occ.symbol.starts_with('<') {
                continue;
            }

            let ref_line = occ.range.first().copied().unwrap_or(0) as u32;

            let container_id = if let Some(syms) = file_symbols.get(file.as_str()) {
                syms.iter()
                    .find(|(start, end, _)| ref_line >= *start && ref_line <= *end)
                    .map(|(_, _, id)| id.clone())
            } else {
                None
            };
            let Some(container_id) = container_id else {
                continue;
            };

            let target_id = if let Some((tfile, tname)) = scip_sym_to_file_name.get(&occ.symbol) {
                file_name_to_ids
                    .get(&(tfile.clone(), tname.clone()))
                    .and_then(|ids| ids.first())
                    .cloned()
            } else {
                None
            };
            let Some(target_id) = target_id else {
                continue;
            };

            if container_id == target_id {
                continue;
            }

            // Detect SCIP correction: if tree-sitter had a CALLS edge from
            // container_id to a *different* target for the same call name,
            // SCIP is overriding it — record as a learned pattern.
            if project_root.is_some() {
                if let Some(existing_targets) = existing_calls.get(&container_id) {
                    let call_name = target_id.rsplit("::").next().unwrap_or(&target_id);
                    let target_file = target_id
                        .rsplit("::")
                        .nth(1)
                        .or_else(|| target_id.split("::").next())
                        .unwrap_or(&target_id);
                    let ts_had_different = existing_targets.iter().any(|ts_tgt| {
                        ts_tgt != &target_id
                            && ts_tgt.rsplit("::").next().unwrap_or(ts_tgt) == call_name
                    });
                    if ts_had_different {
                        let source_file = container_id.split("::").next().unwrap_or(&container_id);
                        learned_store.record_correction(
                            source_file,
                            call_name,
                            target_file,
                            &target_id,
                        );
                        stats.corrections_learned += 1;
                    }
                }
            }

            let edge = (container_id, target_id);
            if seen_edges.insert(edge.clone()) {
                calls_to_create.push(edge);
            }
        }
    }

    // Bulk write CALLS edges via Parquet COPY FROM, dropping any bad-PK
    // record and retrying rather than falling back to UNWIND for the batch.
    if !calls_to_create.is_empty() {
        let tmp = std::env::temp_dir();
        let edge_pq = tmp.join("infigraph_scip_calls.parquet");
        stats.references_added = calls_to_create.len();
        copy_edges_with_bad_record_retry(
            &conn,
            "CALLS",
            calls_to_create,
            "Symbol",
            "Symbol",
            &edge_pq,
        );
    }

    // Pass 3: build INHERITS edges from SCIP's compiler-verified is_implementation
    // relationships (class/interface/trait implementation and inheritance).
    // Mapped onto the same RelationKind::Inherits used by tree-sitter's
    // @inherit.child/@inherit.parent captures, since no language's relations.scm
    // currently distinguishes extends from implements.
    let mut inherits_to_create: Vec<(String, String)> = Vec::new();
    let mut seen_inherits: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();

    for doc in &index.documents {
        for si in &doc.symbols {
            if si.symbol.starts_with("local ") || si.symbol.starts_with('<') {
                continue;
            }
            let Some((sfile, sname)) = scip_sym_to_file_name.get(&si.symbol) else {
                continue;
            };
            let Some(source_id) = file_name_to_ids
                .get(&(sfile.clone(), sname.clone()))
                .and_then(|ids| ids.first())
                .cloned()
            else {
                continue;
            };

            for rel in &si.relationships {
                if !rel.is_implementation {
                    continue;
                }
                let Some((tfile, tname)) = scip_sym_to_file_name.get(&rel.symbol) else {
                    continue;
                };
                let Some(target_id) = file_name_to_ids
                    .get(&(tfile.clone(), tname.clone()))
                    .and_then(|ids| ids.first())
                    .cloned()
                else {
                    continue;
                };

                if source_id == target_id {
                    continue;
                }

                let edge = (source_id.clone(), target_id);
                if seen_inherits.insert(edge.clone()) {
                    inherits_to_create.push(edge);
                }
            }
        }
    }

    // Bulk write INHERITS edges via Parquet COPY FROM, dropping any bad-PK
    // record and retrying rather than falling back to UNWIND for the batch.
    if !inherits_to_create.is_empty() {
        let tmp = std::env::temp_dir();
        let edge_pq = tmp.join("infigraph_scip_inherits.parquet");
        stats.relations_added = inherits_to_create.len();
        copy_edges_with_bad_record_retry(
            &conn,
            "INHERITS",
            inherits_to_create,
            "Symbol",
            "Symbol",
            &edge_pq,
        );
    }

    // Persist learned corrections (if any were recorded)
    if let Some(root) = project_root {
        if stats.corrections_learned > 0 {
            if let Err(e) = learned_store.save(root) {
                eprintln!("warning: failed to save learned patterns: {e}");
            }
        }
    }

    Ok(stats)
}

fn parse_range(range: &[i32], file: &str) -> Span {
    let (start_line, start_col, end_line, end_col) = match range.len() {
        4 => (range[0], range[1], range[2], range[3]),
        3 => (range[0], range[1], range[0], range[2]),
        _ => (0, 0, 0, 0),
    };
    Span {
        file: file.to_string(),
        start_line: start_line as u32,
        start_col: start_col as u32,
        end_line: end_line as u32,
        end_col: end_col as u32,
    }
}

/// Extract the display name of the last descriptor in a SCIP symbol string.
///
/// SCIP symbols end with a chain of `<name><suffix>` descriptors (suffix is one
/// of `.` term, `#` type, `/` namespace, `:` macro, or `(...)`.` method with an
/// optional disambiguator) and the suffix is the literal last character, e.g.
/// `rust-analyzer cargo sittir-core 0.0.0 is_allowed_node_key().` or `.../crate/`.
/// The suffix must be stripped *before* looking for the name, otherwise it reads
/// as trailing empty text.
///
/// Non-identifier names (file paths, string-literal object keys, etc.) are
/// quoted by the indexer -- backticks for file-path descriptors, double quotes
/// for term names that aren't valid bare identifiers -- and may carry a
/// trailing disambiguator digit run the indexer appends to distinguish
/// repeated non-identifier names in the same scope (e.g. `"'"0`, `` `x`1 ``).
/// A method/term descriptor may also chain a non-empty `(...)` disambiguator
/// group after its own (usually empty) parens, e.g. `findNestedSeparator().(rule).`.
/// Both kinds of disambiguator are folded back into the returned name rather
/// than dropped: dropping either collapses distinct symbols onto one name,
/// causing a primary-key collision on insert (observed on sittir -- both a
/// duplicate-symbol-insert failure and, separately, a dangling-edge-endpoint
/// failure whose "name" had silently fallen back to the raw, unparsed symbol
/// string once the old single-`if` parens handling failed to find any
/// identifier left to extract).
fn scip_sym_to_name(scip_sym: &str) -> String {
    let mut s = scip_sym.trim_end();

    // Strip a trailing method terminator.
    if let Some(rest) = s.strip_suffix('.') {
        s = rest;
    }

    // Strip trailing `(...)` disambiguator groups, possibly chained. An
    // empty group (the common case for a plain method call) carries no
    // information and is dropped; a non-empty one is preserved by appending
    // it to the extracted name below.
    let mut suffix = String::new();
    while s.ends_with(')') {
        let Some(open) = s.rfind('(') else { break };
        let inner = &s[open + 1..s.len() - 1];
        if !inner.is_empty() {
            suffix = format!(".{inner}{suffix}");
        }
        s = s[..open].trim_end_matches(['#', '/', ':', '.']);
    }

    // Strip a single trailing suffix marker (type/namespace/macro/term).
    let mut s = s.trim_end_matches(['#', '/', ':', '.']);

    // Peel off a trailing disambiguator digit run that immediately follows a
    // closing quote, keeping it to append to the extracted name below.
    let mut disambiguator = "";
    let digit_start = s
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    if digit_start > 0 && digit_start < s.len() {
        let quote = s.as_bytes()[digit_start - 1];
        if quote == b'`' || quote == b'"' {
            disambiguator = &s[digit_start..];
            s = &s[..digit_start];
        }
    }

    // Quoted descriptor name: `Name` or "Name" (with disambiguators re-attached).
    for quote in ['`', '"'] {
        if let Some(rest) = s.strip_suffix(quote) {
            if let Some(start) = rest.rfind(quote) {
                return format!("{}{disambiguator}{suffix}", &rest[start + 1..]);
            }
        }
    }

    // Bare identifier: trailing run of alphanumeric/underscore characters.
    let ident_start = s
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    if ident_start < s.len() {
        format!("{}{disambiguator}{suffix}", &s[ident_start..])
    } else if !suffix.is_empty() {
        suffix.trim_start_matches('.').to_string()
    } else {
        scip_sym.to_string()
    }
}

fn scip_kind_to_prism(kind: &symbol_information::Kind) -> SymbolKind {
    use symbol_information::Kind::*;
    match kind {
        Function | AbstractMethod | StaticMethod | PureVirtualMethod | ProtocolMethod
        | TraitMethod | TypeClassMethod => SymbolKind::Function,
        Method | MethodAlias | MethodReceiver | MethodSpecification => SymbolKind::Method,
        Class | SingletonClass => SymbolKind::Class,
        Struct => SymbolKind::Struct,
        Interface => SymbolKind::Interface,
        Trait | TypeClass => SymbolKind::Trait,
        Enum | EnumMember => SymbolKind::Enum,
        Module | Namespace | Package => SymbolKind::Module,
        Variable | StaticVariable | Field | SelfParameter | Parameter => SymbolKind::Variable,
        Constant => SymbolKind::Constant,
        _ => SymbolKind::Function,
    }
}

#[derive(Default, Debug)]
pub struct ImportStats {
    pub files_processed: usize,
    pub symbols_added: usize,
    pub symbols_enriched: usize,
    pub symbols_skipped: usize,
    pub relations_added: usize,
    pub references_added: usize,
    pub corrections_learned: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document, Occurrence, Relationship, SymbolInformation};

    #[test]
    fn scip_sym_to_name_strips_trailing_suffix_markers() {
        // Real rust-analyzer output: suffix char is the literal last byte.
        assert_eq!(
            scip_sym_to_name("rust-analyzer cargo sittir-core 0.0.0 crate/"),
            "crate"
        );
        assert_eq!(
            scip_sym_to_name("rust-analyzer cargo sittir-core 0.0.0 K_IDENTIFIER."),
            "K_IDENTIFIER"
        );
        assert_eq!(
            scip_sym_to_name("rust-analyzer cargo sittir-core 0.0.0 is_allowed_node_key()."),
            "is_allowed_node_key"
        );
        assert_eq!(
            scip_sym_to_name("rust-analyzer cargo sittir-core 0.0.0 SomeTrait#method()."),
            "method"
        );
    }

    #[test]
    fn scip_sym_to_name_handles_chained_disambiguator_group() {
        // Real scip-typescript output that crashed sittir's SCIP import: a
        // method descriptor's empty `()` chained with a further non-empty
        // `(rule)` disambiguator. The old single-`if` parens handling
        // stripped only the last group, left a dangling `()` with nothing
        // extractable after it, and fell back to returning the entire raw,
        // unparsed symbol string as the "name" -- which then couldn't match
        // any real Symbol id, causing a dangling-edge COPY failure.
        assert_eq!(
            scip_sym_to_name(
                "scip-typescript npm @sittir/codegen 0.1.0 src/compiler/`collect-slots.ts`/findNestedSeparator().(rule)."
            ),
            "findNestedSeparator.rule"
        );
    }

    #[test]
    fn scip_sym_to_name_empty_disambiguator_group_still_strips_cleanly() {
        // A plain method call's empty `()` must still resolve to the bare
        // method name, unaffected by the loop that now also handles
        // non-empty chained groups.
        assert_eq!(
            scip_sym_to_name("rust-analyzer cargo sittir-core 0.0.0 is_allowed_node_key()."),
            "is_allowed_node_key"
        );
    }

    #[test]
    fn scip_sym_to_name_handles_backtick_quoted_descriptors() {
        // Real scip-typescript output: file-path descriptors are backtick-quoted.
        assert_eq!(
            scip_sym_to_name("scip-typescript npm test 1.0.0 `test.ts`/Animal#"),
            "Animal"
        );
        assert_eq!(
            scip_sym_to_name("scip-python python test-pkg 1.0.0 `test`/Animal#"),
            "Animal"
        );
    }

    #[test]
    fn scip_sym_to_name_handles_double_quoted_descriptors_with_disambiguator() {
        // Real scip-typescript output for a non-identifier term name (e.g. an
        // object property literally named `'`): the quoted name is followed
        // by a disambiguator digit, then the term suffix `.`.
        assert_eq!(
            scip_sym_to_name("scip-typescript npm @sittir/codegen 0.1.0 `link.ts`/\"'\"0."),
            "'0"
        );
    }

    #[test]
    fn scip_sym_to_name_disambiguator_keeps_repeated_quoted_names_distinct() {
        // Two different quoted-name symbols in the same scope that share a
        // disambiguator digit must not collapse onto the same extracted name
        // -- doing so caused a primary-key collision on sittir's graph.
        let a = scip_sym_to_name("scip-typescript npm test 1.0.0 \"'\"0.");
        let b = scip_sym_to_name("scip-typescript npm test 1.0.0 \",\"0.");
        assert_ne!(a, b);
        assert_eq!(a, "'0");
        assert_eq!(b, ",0");
    }

    fn scip_symbol(name: &str, file: &str) -> String {
        format!("scip-test npm test 1.0.0 `{file}`/{name}#")
    }

    fn make_scip_index(file: &str, child: &str, parent: &str) -> Vec<u8> {
        let child_sym = scip_symbol(child, file);
        let parent_sym = scip_symbol(parent, file);

        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![
                Occurrence {
                    range: vec![0, 0, 0, parent.len() as i32],
                    symbol: parent_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![5, 0, 5, child.len() as i32],
                    symbol: child_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: parent_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: child_sym.clone(),
                    relationships: vec![Relationship {
                        symbol: parent_sym.clone(),
                        is_implementation: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let index = Index {
            documents: vec![doc],
            ..Default::default()
        };
        index
            .write_to_bytes()
            .expect("serialize synthetic SCIP index")
    }

    struct TestEnv {
        _dir: tempfile::TempDir,
        store: GraphStore,
    }

    impl TestEnv {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let store = GraphStore::open(&dir.path().join("graph")).unwrap();
            Self { _dir: dir, store }
        }
    }

    #[test]
    fn is_implementation_relationship_creates_inherits_edge() {
        let env = TestEnv::new();
        let bytes = make_scip_index("test.ts", "Dog", "Animal");

        let index_path = env._dir.path().join("index.scip");
        std::fs::write(&index_path, bytes).unwrap();

        let stats = import_scip_index(&index_path, &env.store, None).unwrap();
        assert_eq!(stats.relations_added, 1);

        let conn = env.store.connection().unwrap();
        let rows = conn
            .query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.name, b.name")
            .unwrap();
        let pairs: Vec<(String, String)> = rows
            .into_iter()
            .map(|row| {
                (
                    row[0].to_string().trim_matches('"').to_string(),
                    row[1].to_string().trim_matches('"').to_string(),
                )
            })
            .collect();
        assert_eq!(pairs, vec![("Dog".to_string(), "Animal".to_string())]);
    }

    #[test]
    fn non_implementation_relationship_does_not_create_inherits_edge() {
        let env = TestEnv::new();
        let file = "test.ts";
        let child_sym = scip_symbol("Dog", file);
        let parent_sym = scip_symbol("Animal", file);

        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![
                Occurrence {
                    range: vec![0, 0, 0, 6],
                    symbol: parent_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![5, 0, 5, 3],
                    symbol: child_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: parent_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: child_sym.clone(),
                    // is_reference only, NOT is_implementation -- must not become INHERITS.
                    relationships: vec![Relationship {
                        symbol: parent_sym.clone(),
                        is_reference: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let index = Index {
            documents: vec![doc],
            ..Default::default()
        };
        let bytes = index.write_to_bytes().unwrap();
        let index_path = env._dir.path().join("index.scip");
        std::fs::write(&index_path, bytes).unwrap();

        let stats = import_scip_index(&index_path, &env.store, None).unwrap();
        assert_eq!(stats.relations_added, 0);

        let conn = env.store.connection().unwrap();
        let rows = conn
            .query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.name, b.name")
            .unwrap();
        assert!(rows.into_iter().next().is_none());
    }
}
