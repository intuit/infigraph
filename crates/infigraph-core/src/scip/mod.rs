use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::DataType;
use protobuf::Message;
use scip::types::{symbol_information, Index, SymbolRole};

use crate::graph::parquet_loader;
use crate::graph::store_util::{escape, fwd_slash_path, unwind_edges_from_pairs};
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
    let conn = store.connection()?;

    // Load learned pattern store for recording SCIP corrections
    let mut learned_store = project_root
        .map(crate::learned::LearnedStore::load)
        .unwrap_or_default();

    // Pre-load existing CALLS edges from tree-sitter resolution.
    // Used to detect when SCIP resolves differently (= a correction to learn from).
    let mut existing_calls: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    if project_root.is_some() {
        if let Ok(rows) = conn.query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")
        {
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
            let sstart: u32 = row[3]
                .to_string()
                .trim_matches('"')
                .parse()
                .unwrap_or(0);
            let send: u32 = row[4]
                .to_string()
                .trim_matches('"')
                .parse()
                .unwrap_or(0);

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
                file_symbols
                    .entry(file.clone())
                    .or_default()
                    .push((span.start_line, span.end_line, sym_id));
            }
        }

        stats.files_processed += 1;
    }

    // Bulk insert new SCIP symbols via Parquet COPY FROM
    const CHUNK: usize = 2000;
    if !new_symbols.is_empty() {
        let tmp = std::env::temp_dir();
        let sym_pq = tmp.join("infigraph_scip_symbols.parquet");

        let ids: Vec<&str> = new_symbols.iter().map(|(id, ..)| id.as_str()).collect();
        let names: Vec<&str> = new_symbols
            .iter()
            .map(|(_, name, ..)| name.as_str())
            .collect();
        let kinds: Vec<&str> = new_symbols
            .iter()
            .map(|(_, _, kind, ..)| kind.as_str())
            .collect();
        let files: Vec<&str> = new_symbols
            .iter()
            .map(|(_, _, _, file, ..)| file.as_str())
            .collect();
        let start_lines: Vec<i64> = new_symbols
            .iter()
            .map(|(_, _, _, _, sl, ..)| *sl as i64)
            .collect();
        let end_lines: Vec<i64> = new_symbols.iter().map(|(.., el, _)| *el as i64).collect();
        let docs: Vec<&str> = new_symbols.iter().map(|(.., doc)| doc.as_str()).collect();
        let n = new_symbols.len();
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

        let copy_ok = if pq_ok {
            match conn.query(&format!(
                "COPY Symbol (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity, parameters, return_type) FROM '{}'",
                fwd_slash_path(&sym_pq)
            )) {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("Auto-SCIP: COPY Symbol failed ({e}), falling back to UNWIND");
                    false
                }
            }
        } else {
            eprintln!("Auto-SCIP: parquet write failed, falling back to UNWIND");
            false
        };

        if !copy_ok {
            for chunk in new_symbols.chunks(CHUNK) {
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
                        let source_file =
                            container_id.split("::").next().unwrap_or(&container_id);
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

    // Bulk write CALLS edges via Parquet COPY FROM
    if !calls_to_create.is_empty() {
        let tmp = std::env::temp_dir();
        let edge_pq = tmp.join("infigraph_scip_calls.parquet");
        let refs: Vec<(&str, &str)> = calls_to_create
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        if parquet_loader::write_edge_parquet(&edge_pq, &refs).is_ok() {
            if let Err(e) = conn.query(&format!(
                "COPY CALLS FROM '{}'",
                fwd_slash_path(&edge_pq)
            )) {
                eprintln!("Auto-SCIP: COPY CALLS failed ({e}), falling back to UNWIND");
                unwind_edges_from_pairs(&conn, &refs, "CALLS", "Symbol", "Symbol");
            }
        } else {
            unwind_edges_from_pairs(&conn, &refs, "CALLS", "Symbol", "Symbol");
        }
        stats.references_added = calls_to_create.len();
        let _ = std::fs::remove_file(&edge_pq);
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

fn scip_sym_to_name(scip_sym: &str) -> String {
    scip_sym
        .rsplit_once('`')
        .map(|(_, n)| n)
        .or_else(|| scip_sym.rsplit(['#', '.', '/']).next())
        .unwrap_or(scip_sym)
        .trim_matches(|c| c == '(' || c == ')' || c == '`')
        .to_string()
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
