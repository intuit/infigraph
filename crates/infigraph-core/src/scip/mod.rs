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

/// (file, name) -> candidate (start_line, end_line, symbol_id) tuples --
/// see `file_name_to_ids` in `import_scip_index` for how this disambiguates
/// same-named symbols by containment.
type NameCandidates = HashMap<(String, String), Vec<(u32, u32, String)>>;

/// A newly-created (SCIP-only) symbol row, in COPY column order: id, name,
/// kind, file, start_line, end_line, docstring, scip_id.
type NewSymbolRow = (String, String, String, String, u32, u32, String, String);

/// True when `scip_sym` is a member (e.g. a parameter) of a symbol we
/// already know about, per SCIP's own descriptor grammar: strip a single
/// trailing `(...)` group and check whether what remains is a moniker
/// already present in `known` (built from every other non-local
/// definition occurrence in this same import pass, before this check
/// ever runs -- see `scip_sym_to_file_name`, populated before Pass 1).
///
/// Verified against real `scip-typescript` output: a parameter's moniker
/// is exactly its enclosing method's own moniker (always ending in `.`)
/// with `(paramName)` appended -- nothing else -- so this is an exact
/// string match against already-known data, not a re-derived heuristic.
/// Deliberately does NOT use `SymbolInformation.kind`: verified always
/// `UnspecifiedKind` in real output, so it carries no usable signal.
///
/// A normal top-level definition's own moniker always ends in `.` (Term),
/// `#` (Type), or `().` (Method) per SCIP's descriptor grammar -- never a
/// bare `)` -- so this never fires for a legitimately-new symbol; it can
/// only match nested member descriptors.
fn is_member_of_known_symbol(scip_sym: &str, known: &HashMap<String, (String, String)>) -> bool {
    let Some(without_group) = scip_sym.strip_suffix(')') else {
        return false;
    };
    let Some(open) = without_group.rfind('(') else {
        return false;
    };
    known.contains_key(&without_group[..open])
}

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

    // Pre-load all symbols from graph into memory: (file, name) -> Vec<(start_line,
    // end_line, symbol_id)> -- carries each candidate's span so Pass 1 can pick the
    // SPECIFIC same-named symbol whose span contains a given occurrence, instead of
    // enriching every same-named symbol in the file (the collision bug fixed by
    // Phase 2's ordinal disambiguation in entities.rs now means genuinely distinct
    // same-named symbols have distinct, non-overlapping spans to disambiguate by).
    // file -> sorted Vec<(start_line, end_line, symbol_id)> for containment lookup
    // (unfiltered by name -- used for Pass 2's caller-attribution only).
    let mut file_name_to_ids: NameCandidates = HashMap::new();
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
                .push((sstart, send, sid.clone()));

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

    // Snapshot of what existed in the graph BEFORE this import pass -- used
    // below to scope the `.first()` fallback to genuinely pre-existing
    // ambiguity (a span-computation edge case against one already-known
    // symbol), never against a candidate `file_name_to_ids` gains *during*
    // this same Pass 1 loop. Without this, a second brand-new same-named
    // symbol in this batch sees the first one (just inserted a few lines
    // below) as its only "candidate", fails containment (their spans don't
    // overlap), and silently falls back to merging into it as an
    // "enrichment" instead of becoming its own symbol -- confirmed via
    // `probe_two_new_same_named_symbols_one_file`: two distinct new SCIP
    // symbols collapsed into one Symbol row with zero warning.
    let preexisting_file_name_to_ids = file_name_to_ids.clone();

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
    //
    // Enrichments only ever update `docstring`. Existing symbols already
    // have a correct start_line/end_line from tree-sitter's full-body AST
    // extraction; a SCIP definition occurrence's range is the span of the
    // identifier token itself, not the enclosing declaration, so it must
    // never be written back over the tree-sitter-derived span (previously
    // it was, silently collapsing every enriched symbol's displayed source
    // to ~1 line).
    let mut enrichments: Vec<(String, String, String)> = Vec::new();
    let mut new_symbols: Vec<NewSymbolRow> = Vec::new();

    // SCIP moniker -> resolved tree-sitter Symbol.id, built as each definition
    // occurrence below is correlated to a specific symbol (by containment for
    // existing symbols, or its own fresh id for newly-created ones). A SCIP
    // moniker string is self-consistent across every occurrence of that symbol
    // within one index (definition AND every reference, disambiguator included),
    // so Pass 2/3 can look up a reference's target directly here instead of
    // re-deriving resolution through the lossy (file, name) map.
    let mut scip_sym_to_ts_id: HashMap<String, String> = HashMap::new();

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
            if is_member_of_known_symbol(scip_sym, &scip_sym_to_file_name) {
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
            // Among same-named candidates, pick the ONE whose span contains this
            // occurrence's identifier token -- not every same-named candidate.
            // Phase 2's ordinal disambiguation (entities.rs) means genuinely
            // distinct same-named symbols now have distinct, non-overlapping
            // spans, so containment reliably picks the right one. The `.first()`
            // fallback only covers a genuine span-computation edge case against
            // a symbol that already existed BEFORE this import pass -- it must
            // never fall back onto a candidate `file_name_to_ids` gained *during*
            // this same loop (that candidate is some other occurrence processed
            // moments ago, not this one), and only when pre-existing ambiguity is
            // low enough (a single candidate) to safely guess.
            let matched = file_name_to_ids
                .get(&key)
                .and_then(|candidates| {
                    candidates
                        .iter()
                        .find(|(s, e, _)| span.start_line >= *s && span.start_line <= *e)
                        .map(|(_, _, id)| id.clone())
                })
                .or_else(|| {
                    preexisting_file_name_to_ids.get(&key).and_then(|c| {
                        if c.len() == 1 {
                            c.first().map(|(_, _, id)| id.clone())
                        } else {
                            None
                        }
                    })
                });

            if let Some(sid) = matched {
                enrichments.push((sid.clone(), docstring.to_string(), scip_sym.clone()));
                scip_sym_to_ts_id.insert(scip_sym.clone(), sid);
                stats.symbols_enriched += 1;
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
                    scip_sym.clone(),
                ));
                stats.symbols_added += 1;
                scip_sym_to_ts_id.insert(scip_sym.clone(), sym_id.clone());
                file_name_to_ids.entry(key).or_default().push((
                    span.start_line,
                    span.end_line,
                    sym_id.clone(),
                ));
                file_symbols.entry(file.clone()).or_default().push((
                    span.start_line,
                    span.end_line,
                    sym_id,
                ));
            }
        }

        stats.files_processed += 1;
    }

    // Phase 2-style ordinal disambiguation (mirrors entities.rs's
    // tree-sitter path) for genuinely-distinct new SCIP symbols that share a
    // (file, name) key -- e.g. `A#foo()` and `B#foo()` both extract to
    // "foo". Without this, both would carry the identical unsuffixed id and
    // the `seen_ids` dedup below would silently keep only one, losing the
    // other with no warning (confirmed via
    // `probe_two_new_same_named_symbols_one_file`). Ordinals continue past
    // whatever count already existed for this key before this import pass,
    // so a newly-added symbol can never collide with one already in the
    // graph either.
    {
        let mut groups: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, (_, name, _, file, ..)) in new_symbols.iter().enumerate() {
            groups
                .entry((file.clone(), name.clone()))
                .or_default()
                .push(i);
        }
        for ((file, name), mut idxs) in groups {
            let existing_count = preexisting_file_name_to_ids
                .get(&(file.clone(), name.clone()))
                .map_or(0, |v| v.len());
            // Truly unambiguous: exactly one new occurrence in this batch,
            // and nothing pre-existing under this key either -- keep the
            // bare id (matches entities.rs's own single-occurrence case).
            // Otherwise (more than one new occurrence, and/or something
            // already exists for this key) every one of THIS batch's
            // occurrences needs a suffix continuing past `existing_count`,
            // including a solo one -- a lone new occurrence colliding with
            // pre-existing rows is exactly what caused a real
            // duplicated-primary-key COPY failure before this fix.
            if idxs.len() == 1 && existing_count == 0 {
                continue;
            }
            idxs.sort_by_key(|&i| (new_symbols[i].4, new_symbols[i].5));
            for (n, i) in idxs.into_iter().enumerate() {
                let new_id = format!("{file}::{name}#{}", existing_count + n + 1);
                new_symbols[i].0 = new_id.clone();
                scip_sym_to_ts_id.insert(new_symbols[i].7.clone(), new_id);
            }
        }
    }

    // Bulk insert new SCIP symbols via Parquet COPY FROM. `seen_ids` is now
    // only a defensive backstop against a literal exact duplicate (the same
    // definition occurrence observed twice) -- genuinely distinct same-named
    // symbols are disambiguated above before reaching this point. On a COPY
    // failure against an id that already exists in the graph, drop that one
    // record and retry rather than falling back to UNWIND for the whole
    // batch; only exhausting MAX_SYMBOL_RETRIES falls back.
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
            let end_lines: Vec<i64> = remaining.iter().map(|(.., el, _, _)| *el as i64).collect();
            let docs: Vec<&str> = remaining.iter().map(|(.., doc, _)| doc.as_str()).collect();
            let scip_ids: Vec<&str> = remaining
                .iter()
                .map(|(.., scip_id)| scip_id.as_str())
                .collect();
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
                    ("scip_id", DataType::Utf8),
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
                    Arc::new(StringArray::from(scip_ids)),
                ],
            )
            .is_ok();

            if !pq_ok {
                eprintln!("Auto-SCIP: parquet write failed, falling back to UNWIND");
                break;
            }

            match conn.query(&format!(
                "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, parameters, return_type, scip_id) FROM '{}'",
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
                    .map(|(id, name, kind, file, start, end, doc, scip_id)| {
                        format!(
                            "{{id: '{}', name: '{}', kind: '{}', file: '{}', sl: {}, el: {}, doc: '{}', scip: '{}'}}",
                            escape(id),
                            escape(name),
                            escape(kind),
                            escape(file),
                            start,
                            end,
                            escape(doc),
                            escape(scip_id)
                        )
                    })
                    .collect();
                let _ = conn.query(&format!(
                    "UNWIND [{}] AS s CREATE (:Symbol {{id: s.id, name: s.name, kind: s.kind, file: s.file, start_line: s.sl, end_line: s.el, signature_hash: '', language: 'scip', visibility: 'public', parent: '', docstring: s.doc, complexity: 0, parameters: '', return_type: '', scip_id: s.scip}})",
                    rows.join(", ")
                ));
            }
        }
        let _ = std::fs::remove_file(&sym_pq);
    }

    // Bulk write enrichments via UNWIND (updates can't use COPY FROM).
    // Only docstring is enriched -- see the note on `enrichments` above for
    // why start_line/end_line must never be written here.
    for chunk in enrichments.chunks(CHUNK) {
        let rows: Vec<String> = chunk
            .iter()
            .map(|(id, doc, scip_id)| {
                format!(
                    "{{id: '{}', doc: '{}', scip: '{}'}}",
                    escape(id),
                    escape(doc),
                    escape(scip_id)
                )
            })
            .collect();
        let _ = conn.query(&format!(
            "UNWIND [{}] AS e MATCH (s:Symbol) WHERE s.id = e.id SET s.docstring = e.doc, s.scip_id = e.scip",
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

            // Direct, exact lookup via the SCIP moniker itself -- built once in
            // Pass 1 as each definition was correlated to its tree-sitter
            // symbol, so this never suffers the (file, name) collision the old
            // two-step scip_sym_to_file_name -> file_name_to_ids chain did.
            let Some(target_id) = scip_sym_to_ts_id.get(&occ.symbol).cloned() else {
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
            // Same direct scip_sym_to_ts_id lookup as Pass 2 -- see its comment.
            let Some(source_id) = scip_sym_to_ts_id.get(&si.symbol).cloned() else {
                continue;
            };

            for rel in &si.relationships {
                if !rel.is_implementation {
                    continue;
                }
                let Some(target_id) = scip_sym_to_ts_id.get(&rel.symbol).cloned() else {
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

    #[test]
    fn enrichment_does_not_overwrite_existing_symbol_span() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();

        // Simulate tree-sitter's correct full-body extraction: a function
        // spanning lines 10-50.
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::widen', name: 'widen', kind: 'function', \
             file: 'test.ts', start_line: 10, end_line: 50, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', docstring: '', \
             complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();

        // SCIP's definition occurrence for the same symbol only spans the
        // identifier token itself (a single line) -- never the full body.
        let sym = scip_symbol("widen", "test.ts");
        let doc = Document {
            relative_path: "test.ts".to_string(),
            occurrences: vec![Occurrence {
                range: vec![9, 9, 9, 14], // 0-based line 9 == 1-based line 10
                symbol: sym.clone(),
                symbol_roles: SymbolRole::Definition as i32,
                ..Default::default()
            }],
            symbols: vec![SymbolInformation {
                symbol: sym,
                documentation: vec!["Widens a value.".to_string()],
                ..Default::default()
            }],
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
        assert_eq!(stats.symbols_enriched, 1, "enrichment path must have run");

        let rows = conn
            .query(
                "MATCH (s:Symbol {id: 'test.ts::widen'}) RETURN s.start_line, s.end_line, s.docstring",
            )
            .unwrap();
        let row = rows.into_iter().next().expect("symbol must still exist");
        let start: i64 = row[0].to_string().parse().unwrap();
        let end: i64 = row[1].to_string().parse().unwrap();
        let docstring = row[2].to_string().trim_matches('"').to_string();

        assert_eq!(
            start, 10,
            "SCIP enrichment must not overwrite the existing full-body start_line with the narrow definition-occurrence range"
        );
        assert_eq!(
            end, 50,
            "SCIP enrichment must not overwrite the existing full-body end_line with the narrow definition-occurrence range"
        );
        assert_eq!(
            docstring, "Widens a value.",
            "docstring enrichment should still apply"
        );
    }

    #[test]
    fn scip_parameter_descriptor_does_not_become_a_new_symbol() {
        let env = TestEnv::new();
        let file = "test.ts";
        // Real scip-typescript shape, verified against real output: a
        // parameter's moniker is exactly its enclosing method's own moniker
        // (always ending in `.`) with `(paramName)` appended.
        let method_sym = "scip-test npm test 1.0.0 `test.ts`/mintFn().".to_string();
        let param_sym = "scip-test npm test 1.0.0 `test.ts`/mintFn().(rulesBag)".to_string();

        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![
                Occurrence {
                    range: vec![0, 16, 22],
                    symbol: method_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![0, 23, 31],
                    symbol: param_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: method_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: param_sym.clone(),
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
        assert_eq!(
            stats.symbols_added, 1,
            "only the method itself should become a new symbol -- the parameter must be suppressed"
        );

        let conn = env.store.connection().unwrap();
        let rows = conn.query("MATCH (s:Symbol) RETURN s.name").unwrap();
        let names: Vec<String> = rows
            .into_iter()
            .map(|row| row[0].to_string().trim_matches('"').to_string())
            .collect();
        assert_eq!(
            names,
            vec!["mintFn".to_string()],
            "no node should exist for the parameter descriptor, and its name must not \
             leak through as a raw unparsed moniker on any node"
        );
    }

    #[test]
    fn calls_edge_still_attributes_to_enclosing_method_when_a_parameter_is_suppressed() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();

        // Seed two pre-existing tree-sitter symbols with real full-body
        // spans, the normal case: SCIP enriches an already-known function,
        // it doesn't need to add it.
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::mintFn', name: 'mintFn', kind: 'function', \
             file: 'test.ts', start_line: 1, end_line: 10, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', docstring: '', \
             complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::helper', name: 'helper', kind: 'function', \
             file: 'test.ts', start_line: 20, end_line: 25, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', docstring: '', \
             complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();

        let file = "test.ts";
        let method_sym = "scip-test npm test 1.0.0 `test.ts`/mintFn().".to_string();
        let param_sym = "scip-test npm test 1.0.0 `test.ts`/mintFn().(rulesBag)".to_string();
        let helper_sym = "scip-test npm test 1.0.0 `test.ts`/helper().".to_string();

        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![
                // mintFn's own definition occurrence (identifier token only,
                // line 0 == 1-based line 1, matching the seeded start_line).
                Occurrence {
                    range: vec![0, 16, 22],
                    symbol: method_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                // Its parameter -- must be suppressed, not create a node
                // that could steal container_id for the reference below.
                Occurrence {
                    range: vec![0, 23, 31],
                    symbol: param_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                // helper's own definition occurrence (1-based line 20).
                Occurrence {
                    range: vec![19, 9, 15],
                    symbol: helper_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                // A reference to helper() from inside mintFn's body (line 4,
                // 0-based -> 1-based line 5, within mintFn's seeded [1,10]
                // span and nowhere near the suppressed parameter's own
                // narrow single-line range).
                Occurrence {
                    range: vec![4, 2, 8],
                    symbol: helper_sym.clone(),
                    symbol_roles: 0, // reference, not definition
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: method_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: param_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: helper_sym.clone(),
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
        assert_eq!(
            stats.symbols_added, 0,
            "both mintFn and helper already exist -- only enrichment, and the \
             parameter must be suppressed, not added"
        );

        let rows = conn
            .query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.name, b.name")
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
        assert_eq!(
            pairs,
            vec![("mintFn".to_string(), "helper".to_string())],
            "the CALLS edge must attribute to the real enclosing method, not be \
             dropped or misattributed to a suppressed parameter pseudo-symbol"
        );
    }

    /// Regression test for Phase 2's Part B: the old `.first()`-off-
    /// file_name_to_ids resolution picked an arbitrary same-named symbol as
    /// a CALLS edge's target whenever more than one existed in a file --
    /// here, two distinct types (A, B) each with their own `foo` method
    /// (same extracted name, different SCIP monikers, different spans).
    /// A real call to specifically `B::foo` must resolve there, not fall
    /// back to `A::foo` just because it happened to be inserted first.
    #[test]
    fn calls_edge_resolves_to_the_specific_same_named_target_not_first_in_file() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();

        // Two pre-existing tree-sitter symbols with the SAME name but
        // different parents/ids and non-overlapping spans -- exactly what
        // Phase 2 Part A's ordinal disambiguation (or simply two distinct
        // types) produces today.
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::A::foo', name: 'foo', kind: 'method', \
             file: 'test.ts', start_line: 1, end_line: 5, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: 'test.ts::A', \
             docstring: '', complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::B::foo', name: 'foo', kind: 'method', \
             file: 'test.ts', start_line: 10, end_line: 15, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: 'test.ts::B', \
             docstring: '', complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::caller', name: 'caller', kind: 'function', \
             file: 'test.ts', start_line: 20, end_line: 25, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', docstring: '', \
             complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();

        let file = "test.ts";
        // A#foo and B#foo both extract to name "foo" via scip_sym_to_name
        // (the `A#`/`B#` type-qualifier prefix is stripped) -- a realistic
        // compiler-emitted collision, not a contrived string.
        let a_foo_sym = "scip-test npm test 1.0.0 `test.ts`/A#foo().".to_string();
        let b_foo_sym = "scip-test npm test 1.0.0 `test.ts`/B#foo().".to_string();
        let caller_sym = "scip-test npm test 1.0.0 `test.ts`/caller().".to_string();

        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![
                Occurrence {
                    range: vec![1, 4, 7], // 1-based line 2, within A::foo's [1,5]
                    symbol: a_foo_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![10, 4, 7], // 1-based line 11, within B::foo's [10,15]
                    symbol: b_foo_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![20, 9, 15], // 1-based line 21, within caller's [20,25]
                    symbol: caller_sym.clone(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                // A call to B::foo specifically, from inside caller's body.
                Occurrence {
                    range: vec![21, 2, 5],
                    symbol: b_foo_sym.clone(),
                    symbol_roles: 0, // reference, not definition
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: a_foo_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: b_foo_sym.clone(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: caller_sym.clone(),
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

        import_scip_index(&index_path, &env.store, None).unwrap();

        let rows = conn
            .query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")
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
        assert_eq!(
            pairs,
            vec![("test.ts::caller".to_string(), "test.ts::B::foo".to_string())],
            "the CALLS edge must resolve to the specific B::foo the reference \
             actually pointed at, not arbitrarily pick A::foo"
        );
    }

    /// Regression test for the sittir graph-explosion incident: a failed
    /// CALLS preload used to be silently treated as "no existing CALLS
    /// edges", corrupting the learned-pattern store's correction detection
    /// on every enrichment cycle a query happened to fail under real-world
    /// resource pressure. It must now propagate as an error instead.
    #[test]
    fn import_scip_index_fails_loudly_when_the_calls_preload_query_errors() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();
        conn.query("DROP TABLE CALLS").unwrap();

        let bytes = make_scip_index("test.ts", "Dog", "Animal");
        let index_path = env._dir.path().join("index.scip");
        std::fs::write(&index_path, bytes).unwrap();

        let result = import_scip_index(&index_path, &env.store, Some(env._dir.path()));
        assert!(
            result.is_err(),
            "a failed CALLS preload must propagate as an error, not silently \
             proceed with an empty existing_calls map"
        );
    }

    /// Regression test for the sittir graph-explosion incident's more severe
    /// half: a failed preload of existing Symbol rows used to be silently
    /// treated as "this project has no symbols yet", which made every
    /// already-indexed symbol look brand-new and re-inserted it via COPY --
    /// non-self-healing, since the next enrichment cycle repeats the same
    /// failure against the same graph state. Confirmed against sittir's own
    /// watch.log: ~27,000 "new" symbols reported on every single background
    /// enrichment cycle instead of converging toward zero.
    #[test]
    fn import_scip_index_fails_loudly_when_the_symbol_preload_query_errors() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();
        // Break the exact columns `MATCH (s:Symbol) RETURN s.id, s.file,
        // s.name, s.start_line, s.end_line` selects, without touching the
        // table's usability for the initial write_lock()/connection() calls
        // that must still succeed before this preload query ever runs.
        conn.query("ALTER TABLE Symbol DROP start_line").unwrap();

        let bytes = make_scip_index("test.ts", "Dog", "Animal");
        let index_path = env._dir.path().join("index.scip");
        std::fs::write(&index_path, bytes).unwrap();

        let result = import_scip_index(&index_path, &env.store, None);
        assert!(
            result.is_err(),
            "a failed Symbol preload must propagate as an error, not silently \
             proceed with an empty file_name_to_ids map that makes every \
             existing symbol look new"
        );
    }

    fn two_same_named_defs_doc(file: &str, a_sym: &str, b_sym: &str) -> Document {
        Document {
            relative_path: file.to_string(),
            occurrences: vec![
                Occurrence {
                    range: vec![1, 0, 1, 3],
                    symbol: a_sym.to_string(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![10, 0, 10, 3],
                    symbol: b_sym.to_string(),
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
            ],
            symbols: vec![
                SymbolInformation {
                    symbol: a_sym.to_string(),
                    ..Default::default()
                },
                SymbolInformation {
                    symbol: b_sym.to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// Regression test: two brand-new SCIP symbols in the same file that
    /// both extract to the same name via `scip_sym_to_name` (e.g. `A#foo()`
    /// and `B#foo()` -- a realistic same-named-method-on-different-types
    /// collision, no existing graph state at all) must both survive as
    /// distinct Symbol rows, ordinal-disambiguated like entities.rs's
    /// tree-sitter path -- not silently collapse into one via the
    /// `.first()` enrichment fallback (they used to: confirmed
    /// `symbols_added: 1, symbols_enriched: 1` and only one graph row before
    /// this fix) or via the `seen_ids` bulk-insert dedup (confirmed
    /// `symbols_added: 2` but still only one graph row, before the ordinal
    /// disambiguation half of this fix).
    #[test]
    fn two_new_same_named_symbols_in_one_file_both_survive_with_distinct_ordinal_ids() {
        let env = TestEnv::new();
        let file = "test.ts";
        let a_foo_sym = "scip-test npm test 1.0.0 `test.ts`/A#foo().".to_string();
        let b_foo_sym = "scip-test npm test 1.0.0 `test.ts`/B#foo().".to_string();

        let doc = two_same_named_defs_doc(file, &a_foo_sym, &b_foo_sym);
        let index = Index {
            documents: vec![doc],
            ..Default::default()
        };
        let bytes = index.write_to_bytes().unwrap();
        let index_path = env._dir.path().join("index.scip");
        std::fs::write(&index_path, bytes).unwrap();

        let stats = import_scip_index(&index_path, &env.store, None).unwrap();
        assert_eq!(
            stats.symbols_added, 2,
            "both distinct symbols must be counted as added"
        );
        assert_eq!(
            stats.symbols_enriched, 0,
            "neither is an enrichment of the other"
        );

        let conn = env.store.connection().unwrap();
        let rows = conn
            .query("MATCH (s:Symbol) RETURN s.id, s.start_line ORDER BY s.start_line")
            .unwrap();
        let ids: Vec<(String, String)> = rows
            .into_iter()
            .map(|row| {
                (
                    row[0].to_string().trim_matches('"').to_string(),
                    row[1].to_string(),
                )
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                ("test.ts::foo#1".to_string(), "1".to_string()),
                ("test.ts::foo#2".to_string(), "10".to_string()),
            ],
            "both symbols must survive as distinct, ordinal-suffixed rows sorted by span position"
        );
    }

    /// Regression test: a genuinely new same-named symbol introduced in an
    /// import pass where TWO symbols under that (file, name) key already
    /// exist in the graph (e.g. from a prior run's own ordinal
    /// disambiguation) must get an ordinal-suffixed id continuing past the
    /// existing count -- `#3`, not `#1` or `#2` (which would collide with
    /// one of the pre-existing rows and hit a real duplicated-primary-key
    /// COPY failure) and not the bare unsuffixed id either. With 2+
    /// pre-existing candidates the `.first()` fallback correctly refuses to
    /// guess (see its `c.len() == 1` guard), so this occurrence takes the
    /// "new symbol" path, which is what needs the continued ordinal.
    #[test]
    fn new_symbol_colliding_with_two_preexisting_same_named_symbols_gets_next_ordinal() {
        let env = TestEnv::new();
        let conn = env.store.connection().unwrap();
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::foo#1', name: 'foo', kind: 'method', \
             file: 'test.ts', start_line: 1, end_line: 5, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', \
             docstring: '', complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();
        conn.query(
            "CREATE (:Symbol {id: 'test.ts::foo#2', name: 'foo', kind: 'method', \
             file: 'test.ts', start_line: 10, end_line: 15, signature_hash: '', \
             language: 'typescript', visibility: 'public', parent: '', \
             docstring: '', complexity: 0, parameters: '', return_type: ''})",
        )
        .unwrap();

        let file = "test.ts";
        let c_foo_sym = "scip-test npm test 1.0.0 `test.ts`/C#foo().".to_string();
        let doc = Document {
            relative_path: file.to_string(),
            occurrences: vec![Occurrence {
                range: vec![20, 0, 20, 3],
                symbol: c_foo_sym.clone(),
                symbol_roles: SymbolRole::Definition as i32,
                ..Default::default()
            }],
            symbols: vec![SymbolInformation {
                symbol: c_foo_sym.clone(),
                ..Default::default()
            }],
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
        assert_eq!(stats.symbols_added, 1);
        assert_eq!(stats.symbols_enriched, 0);

        let rows = conn
            .query("MATCH (s:Symbol) RETURN s.id ORDER BY s.start_line")
            .unwrap();
        let ids: Vec<String> = rows
            .into_iter()
            .map(|row| row[0].to_string().trim_matches('"').to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                "test.ts::foo#1".to_string(),
                "test.ts::foo#2".to_string(),
                "test.ts::foo#3".to_string(),
            ],
            "the new symbol must continue the ordinal sequence, not collide with either \
             pre-existing row"
        );
    }
}
