mod entities;
mod relations;
pub use entities::extract_entities;
pub use relations::{extract_relations, extract_relations_with_custom_edges};

use anyhow::Result;

use crate::analysis::extract_statements;
use crate::lang::{LanguagePack, ParserBackend};
use crate::model::{FileExtraction, Relation, RelationKind, Span, Statement, SymbolKind};

thread_local! {
    static TS_PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new(tree_sitter::Parser::new());
}

/// Parse a source file and extract all symbols and relationships.
pub fn extract_file(path: &str, source: &[u8], pack: &LanguagePack) -> Result<FileExtraction> {
    let (symbols, mut relations, statements) = match &pack.backend {
        ParserBackend::TreeSitter {
            grammar,
            entity_query,
            relation_query,
            inherit_decompose_query,
        } => TS_PARSER.with(|cell| -> Result<_> {
            let mut parser = cell.borrow_mut();
            parser.set_language(grammar)?;

            let tree = parser
                .parse(source, None)
                .ok_or_else(|| anyhow::anyhow!("failed to parse {}", path))?;

            let root = tree.root_node();

            let symbols = extract_entities(path, source, root, entity_query, &pack.name);
            let relations = if pack.custom_edges.is_empty() {
                extract_relations(
                    path,
                    source,
                    root,
                    relation_query,
                    inherit_decompose_query.as_deref(),
                )
            } else {
                extract_relations_with_custom_edges(
                    path,
                    source,
                    root,
                    relation_query,
                    &pack.custom_edges,
                    inherit_decompose_query.as_deref(),
                )
            };
            let stmts = extract_statements_for_symbols(root, source, &symbols);
            Ok((symbols, relations, stmts))
        })?,
        ParserBackend::Custom(extractor) => {
            let (s, r) = extractor.extract(path, source, &pack.name)?;
            (s, r, Vec::new())
        }
    };

    // Generate CALLS edges from Route symbols to their handler functions
    generate_route_handler_edges(path, &symbols, &mut relations);

    let content_hash = pack.content_fingerprint(source);

    Ok(FileExtraction {
        file: path.to_string(),
        language: pack.name.clone(),
        content_hash,
        symbols,
        relations,
        statements,
    })
}

fn extract_statements_for_symbols(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    symbols: &[crate::model::Symbol],
) -> Vec<Statement> {
    let fn_symbols: Vec<(&str, u32, u32)> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
            )
        })
        .map(|s| (s.id.as_str(), s.span.start_line, s.span.end_line))
        .collect();

    if fn_symbols.is_empty() {
        return Vec::new();
    }

    let mut all_stmts = Vec::new();
    collect_fn_nodes(root, source, &fn_symbols, &mut all_stmts);
    let mut seen = std::collections::HashSet::new();
    all_stmts.retain(|s| seen.insert(s.id.clone()));
    all_stmts
}

fn collect_fn_nodes<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
    fn_symbols: &[(&str, u32, u32)],
    stmts: &mut Vec<Statement>,
) {
    let start = node.start_position().row as u32 + 1;
    let end = node.end_position().row as u32 + 1;

    if let Some((sym_id, _, _)) = fn_symbols
        .iter()
        .find(|(_, sl, el)| start == *sl && end == *el)
    {
        let mut extracted = extract_statements(node, source, sym_id, "");
        stmts.append(&mut extracted);
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_fn_nodes(child, source, fn_symbols, stmts);
        }
    }
}

/// Create CALLS relations from Route symbols to handler functions in the same file.
/// Matches route handler names from docstrings OR route names containing function names.
fn generate_route_handler_edges(
    file: &str,
    symbols: &[crate::model::Symbol],
    relations: &mut Vec<Relation>,
) {
    // Collect function/method names for matching
    let functions: Vec<(&str, &str)> = symbols
        .iter()
        .filter(|s| {
            (s.kind == SymbolKind::Function || s.kind == SymbolKind::Method) && s.span.file == file
        })
        .map(|s| (s.name.as_str(), s.id.as_str()))
        .collect();

    for sym in symbols {
        if sym.kind != SymbolKind::Route {
            continue;
        }

        let mut target_id: Option<String> = None;

        // Method 1: explicit handler= in docstring
        if let Some(doc) = &sym.docstring {
            if let Some(handler_name) = doc.split("handler=").nth(1).map(|h| h.trim()) {
                target_id = functions
                    .iter()
                    .find(|(name, _)| *name == handler_name)
                    .map(|(_, id)| id.to_string());
            }
        }

        // Method 2: Route is on the same line range as a function — check for overlap
        if target_id.is_none() {
            target_id = symbols
                .iter()
                .find(|s| {
                    (s.kind == SymbolKind::Function || s.kind == SymbolKind::Method)
                        && s.span.file == file
                        && s.span.start_line <= sym.span.end_line
                        && s.span.end_line >= sym.span.start_line
                })
                .map(|s| s.id.clone());
        }

        if let Some(tid) = target_id {
            if tid != sym.id {
                relations.push(Relation {
                    source_id: sym.id.clone(),
                    target_id: tid,
                    kind: RelationKind::Calls,
                    span: Some(Span {
                        file: file.to_string(),
                        start_line: sym.span.start_line,
                        start_col: sym.span.start_col,
                        end_line: sym.span.end_line,
                        end_col: sym.span.end_col,
                    }),
                    receiver: None,
                });
            }
        }
    }
}
