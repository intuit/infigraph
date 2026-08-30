use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use super::{find_parent_class, node_text, resolve_compound_node_text};
use crate::lang::CustomEdgeDef;
use crate::model::{Relation, RelationKind, Span};

/// Extract relationships from a parsed AST using a Tree-sitter query.
///
/// The query must use these capture names:
///   @call.func / @call.site          — function calls
///   @import.module / @import.name    — imports
///   @inherit.child / @inherit.parent — inheritance
///   @{custom}.source / @{custom}.target — custom edges (from language pack custom_edges)
///
/// `decompose_query`, when present, resolves compound `@inherit.parent`/`@inherit.child`
/// captures (generics, qualified names, member expressions) down to their base identifier
/// — see `resolve_compound_node_text`.
pub fn extract_relations(
    file: &str,
    source: &[u8],
    root: Node,
    query: &Query,
    decompose_query: Option<&Query>,
) -> Vec<Relation> {
    extract_relations_with_custom_edges(file, source, root, query, &[], decompose_query)
}

/// Extract relationships including custom edge types defined by the language pack.
pub fn extract_relations_with_custom_edges(
    file: &str,
    source: &[u8],
    root: Node,
    query: &Query,
    custom_edges: &[CustomEdgeDef],
    decompose_query: Option<&Query>,
) -> Vec<Relation> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);

    let capture_names = query.capture_names();

    let mut relations = Vec::new();

    while let Some(m) = matches.next() {
        let mut rel_kind = None;
        let mut source_name = None;
        let mut target_name = None;
        let mut site_node = None;
        let mut receiver_text = None;
        // For custom edges: track source/target per capture prefix
        let mut custom_source: Option<(String, String)> = None; // (edge_name, source_text)
        let mut custom_target: Option<(String, String)> = None; // (edge_name, target_text)
        let mut custom_site_node: Option<Node> = None;
        let mut custom_edge_name: Option<String> = None;

        for capture in m.captures {
            let idx = capture.index as usize;
            let cap_name = capture_names[idx];
            let node = capture.node;
            let text = node_text(node, source);

            match cap_name {
                "call.func" => {
                    target_name = Some(text);
                    rel_kind = Some(RelationKind::Calls);
                }
                "call.site" => {
                    site_node = Some(node);
                }
                "call.caller" => {
                    source_name = Some(text);
                }
                "call.receiver" => {
                    receiver_text = Some(text);
                }
                "import.module" => {
                    target_name = Some(text);
                    rel_kind = Some(RelationKind::Imports);
                    source_name = Some(file.to_string());
                }
                "import.name" => {
                    target_name = Some(text);
                    rel_kind = Some(RelationKind::Imports);
                    source_name = Some(file.to_string());
                }
                "inherit.child" => {
                    source_name = Some(resolve_compound_node_text(node, source, decompose_query));
                    if rel_kind.is_none() {
                        rel_kind = Some(RelationKind::Inherits);
                    }
                }
                "inherit.parent" => {
                    target_name = Some(resolve_compound_node_text(node, source, decompose_query));
                    rel_kind = Some(RelationKind::Inherits);
                }
                other => {
                    // Check for custom edge captures: "{capture}.source", "{capture}.target",
                    // or "{capture}.site" (used to infer source from enclosing function)
                    if let Some((prefix, suffix)) = other.split_once('.') {
                        if let Some(edge_def) = custom_edges.iter().find(|e| e.capture == prefix) {
                            custom_edge_name = Some(edge_def.name.clone());
                            match suffix {
                                "source" => {
                                    custom_source = Some((edge_def.name.clone(), text));
                                    custom_site_node = Some(node);
                                }
                                "target" => {
                                    custom_target = Some((edge_def.name.clone(), text));
                                }
                                "site" => {
                                    custom_site_node = Some(node);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // Handle custom edge if we have a target (source can be inferred)
        if let Some((_, tgt_text)) = custom_target {
            let edge_name = if let Some((name, _)) = &custom_source {
                name.clone()
            } else {
                custom_edge_name.unwrap_or_default()
            };

            if edge_name.is_empty() {
                // No edge name resolved — skip
            } else {
                let src_text = if let Some((_, src)) = custom_source {
                    src
                } else if let Some(site) = custom_site_node {
                    // No explicit source — infer from enclosing function
                    find_enclosing_function(site, source).unwrap_or_else(|| file.to_string())
                } else {
                    file.to_string()
                };

                let span = custom_site_node.map(|n| Span {
                    file: file.to_string(),
                    start_line: n.start_position().row as u32 + 1,
                    start_col: n.start_position().column as u32,
                    end_line: n.end_position().row as u32 + 1,
                    end_col: n.end_position().column as u32,
                });

                let source_id = format!("{}::{}", file, src_text);
                let target_id = format!("{}::{}", file, tgt_text);

                relations.push(Relation {
                    source_id,
                    target_id,
                    kind: RelationKind::Custom(edge_name),
                    span,
                    receiver: None,
                });
                continue;
            }
        }

        if rel_kind == Some(RelationKind::Calls) && source_name.is_none() {
            if let Some(site) = site_node {
                source_name =
                    find_enclosing_function(site, source).or_else(|| Some(file.to_string()));
            }
        }

        // For self/this calls, resolve receiver to enclosing class name
        if rel_kind == Some(RelationKind::Calls) {
            if let Some(ref recv) = receiver_text {
                if recv == "self" || recv == "this" || recv == "@" {
                    if let Some(site) = site_node {
                        if let Some(cls) = find_parent_class(site, source) {
                            receiver_text = Some(cls);
                        }
                    }
                } else if let Some(site) = site_node {
                    // C++: `ctField->GetType()` / `ctx.GetType()` — receiver_text
                    // is the raw variable name ("ctField"), which never matches a
                    // real class name in the resolver's class_method_map lookup.
                    // If it's a member field of the enclosing class, or a parameter
                    // of the enclosing function, resolve it to its declared type
                    // so receiver-aware resolution (resolve_calls.rs Strategy 1)
                    // has a real class name to match against, instead of silently
                    // dangling.
                    if let Some(field_type) = find_field_type_in_enclosing_class(site, source, recv)
                        .or_else(|| find_param_type_in_enclosing_function(site, source, recv))
                    {
                        receiver_text = Some(field_type);
                    }
                }
            }
        }

        if let (Some(kind), Some(src), Some(tgt)) = (rel_kind, source_name, target_name) {
            let span = site_node.map(|n| Span {
                file: file.to_string(),
                start_line: n.start_position().row as u32 + 1,
                start_col: n.start_position().column as u32,
                end_line: n.end_position().row as u32 + 1,
                end_col: n.end_position().column as u32,
            });

            let source_id = if kind == RelationKind::Imports {
                src
            } else {
                format!("{}::{}", file, src)
            };
            let target_id = format!("{}::{}", file, tgt);

            relations.push(Relation {
                source_id,
                target_id,
                kind,
                span,
                receiver: receiver_text.clone(),
            });
        }
    }

    relations
}

/// Walk up the AST to find the enclosing function/method definition and return its name.
/// Walk a C/C++ declarator chain (pointer_declarator, reference_declarator,
/// etc.) down to the innermost function_declarator, if any.
fn find_cpp_function_declarator(function_definition: Node) -> Option<Node> {
    let mut current = function_definition.child_by_field_name("declarator")?;
    loop {
        if current.kind() == "function_declarator" {
            return Some(current);
        }
        current = current.child_by_field_name("declarator")?;
    }
}

/// Reduce a declarator name node to its identifier: qualified names
/// (`ClassName::method`) and destructor names (`~ClassName`) carry the
/// leaf identifier in a nested field rather than as their own text.
fn rightmost_identifier(node: Node) -> Node {
    match node.kind() {
        "qualified_identifier" => node
            .child_by_field_name("name")
            .map(rightmost_identifier)
            .unwrap_or(node),
        "destructor_name" => node.named_child(0).unwrap_or(node),
        _ => node,
    }
}

fn find_enclosing_function(node: Node, source: &[u8]) -> Option<String> {
    let func_kinds = [
        "function_definition",     // Python, JS, Lua, VB6 Function
        "function_item",           // Rust
        "function_declaration",    // Go, JS, TS, Java
        "method_declaration",      // Go, Java
        "method_definition",       // JS/TS class methods
        "constructor_declaration", // C#
        "func_literal",            // Go anonymous
        "sub_definition",          // VB6 Sub
        "property_definition",     // VB6 Property Get/Let/Set
    ];
    let sql_container_kinds = [
        "create_table", // SQL: CREATE TABLE ... AS SELECT
        "insert",       // SQL: INSERT INTO ... SELECT
    ];
    let mut current = node.parent();
    while let Some(n) = current {
        if func_kinds.contains(&n.kind()) {
            if let Some(name_node) = n.child_by_field_name("name") {
                return Some(node_text(name_node, source));
            }
        }
        // C#: a call inside a property accessor body (get/set, or a lambda
        // nested inside one — e.g. `myCmd = new RelayCommand(() => this.Foo())`
        // inside a getter) has no method_declaration/constructor_declaration
        // ancestor at all, only accessor_declaration -> property_declaration.
        // accessor_declaration itself carries no "name" field (the name lives
        // on the parent property_declaration), so without this the walk fell
        // through to the file-level fallback below, producing a CALLS edge
        // attributed to the file itself rather than any real symbol — which
        // never matches a node id and the edge silently vanished. Attribute to
        // the property's own symbol, matching entities.scm's `@var.def` id for
        // property_declaration (`file::Class::PropertyName`).
        if n.kind() == "accessor_declaration" {
            // accessor_declaration's parent is accessor_list, not
            // property_declaration directly — property_declaration is one
            // level further up (class_decl -> property_declaration ->
            // accessor_list -> accessor_declaration).
            if let Some(prop) = n.parent().and_then(|list| list.parent()) {
                if prop.kind() == "property_declaration" {
                    if let Some(name_node) = prop.child_by_field_name("name") {
                        return Some(node_text(name_node, source));
                    }
                }
            }
        }
        // C/C++: function_definition has no "name" field — the name is nested
        // inside its "declarator" field, itself possibly wrapped in
        // pointer_declarator/reference_declarator (e.g. `T* foo()`) before
        // reaching the function_declarator whose own "declarator" field holds
        // the identifier/field_identifier/qualified_identifier/destructor_name.
        if n.kind() == "function_definition" {
            if let Some(func_declarator) = find_cpp_function_declarator(n) {
                if let Some(name_field) = func_declarator.child_by_field_name("declarator") {
                    return Some(node_text(rightmost_identifier(name_field), source));
                }
            }
        }
        // Pascal: defProc → header (declProc) → name
        // name may be identifier (bare) or genericDot (TClass.Method) — use rightmost identifier
        if n.kind() == "defProc" {
            if let Some(header) = n.child_by_field_name("header") {
                if let Some(name_node) = header.child_by_field_name("name") {
                    if name_node.kind() == "genericDot" {
                        if let Some(rhs) = name_node.child_by_field_name("rhs") {
                            return Some(node_text(rhs, source));
                        }
                    }
                    return Some(node_text(name_node, source));
                }
            }
        }
        if sql_container_kinds.contains(&n.kind()) {
            if let Some(obj_ref) = n.child_by_field_name("name") {
                return Some(node_text(obj_ref, source));
            }
            // Fallback: find first object_reference child
            let mut i = 0;
            while let Some(child) = n.child(i) {
                if child.kind() == "object_reference" {
                    if let Some(id) = child.child_by_field_name("name") {
                        return Some(node_text(id, source));
                    }
                }
                i += 1;
            }
        }
        if n.kind() == "cte" {
            // CTE: first child is identifier
            if let Some(id) = n.child(0) {
                if id.kind() == "identifier" {
                    return Some(node_text(id, source));
                }
            }
        }
        current = n.parent();
    }
    None
}

/// Walk up to the enclosing class/struct body, scan its field declarations
/// for one whose declarator matches `field_name`, and return the declared
/// type's base identifier (pointer/reference wrappers and `const`/`*`/`&`
/// stripped) — e.g. for `IBase* field;`, `find_field_type_in_enclosing_class`
/// on `"field"` returns `"IBase"`. This lets a call like `field->GetType()`
/// (C++) or `field.Initialize()` (C#) resolve `field` to its real class name
/// instead of leaving the raw variable name as the receiver, which never
/// matches anything in the resolver's class-name-keyed lookup table.
///
/// Covers two grammar shapes since C++ and C# structure a field declaration
/// differently: C++'s `field_declaration` exposes `type`/`declarator` fields
/// directly; C#'s wraps them one level deeper in a `variable_declaration` ->
/// `variable_declarator` (see csharp/entities.scm's field_declaration
/// pattern, added alongside this).
fn find_field_type_in_enclosing_class(
    node: Node,
    source: &[u8],
    field_name: &str,
) -> Option<String> {
    let class_kinds = ["class_specifier", "struct_specifier", "class_declaration"];
    let mut current = node.parent();
    while let Some(n) = current {
        if class_kinds.contains(&n.kind()) {
            if let Some(body) = n.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() != "field_declaration" {
                        continue;
                    }
                    // C++ shape: type/declarator are direct fields on field_declaration.
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let mut decl_cursor = child.walk();
                        for decl in child.children_by_field_name("declarator", &mut decl_cursor) {
                            if declarator_name(decl, source).as_deref() == Some(field_name) {
                                return Some(strip_type_qualifiers(node_text(type_node, source)));
                            }
                        }
                    }
                    // C# shape: field_declaration -> variable_declaration -> (type, variable_declarator+).
                    let mut vd_cursor = child.walk();
                    for vd in child.children(&mut vd_cursor) {
                        if vd.kind() != "variable_declaration" {
                            continue;
                        }
                        let Some(type_node) = vd.child_by_field_name("type") else {
                            continue;
                        };
                        let mut declarator_cursor = vd.walk();
                        for declarator in vd.children(&mut declarator_cursor) {
                            if declarator.kind() != "variable_declarator" {
                                continue;
                            }
                            if let Some(name_node) = declarator.child_by_field_name("name") {
                                if node_text(name_node, source) == field_name {
                                    return Some(strip_type_qualifiers(node_text(
                                        type_node, source,
                                    )));
                                }
                            }
                        }
                    }
                }
            }
            return None;
        }
        current = n.parent();
    }
    None
}

/// C++ only: walk up to the enclosing `function_definition` and try to resolve
/// `name` to a declared type from either its parameter list or a local variable
/// declaration in its body — same idea as `find_field_type_in_enclosing_class`
/// but for names that aren't class members. Covers two shapes:
///
///   - parameter:    `void UseContext(const IBase& ctx) { ctx.GetType(); }`
///   - local var:    `const zctField* f = Get(...); f->GetType();`
///
/// Both resolve to `"IBase"`/`"zctField"` instead of leaving the raw
/// parameter/variable name as the receiver, which never matches anything in
/// the resolver's class-name-keyed lookup table.
fn find_param_type_in_enclosing_function(node: Node, source: &[u8], name: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "function_definition" {
            if let Some(declarator) = n.child_by_field_name("declarator") {
                if let Some(func_decl) = find_function_declarator(declarator) {
                    if let Some(params) = func_decl.child_by_field_name("parameters") {
                        let mut cursor = params.walk();
                        for param in params.children(&mut cursor) {
                            if param.kind() != "parameter_declaration" {
                                continue;
                            }
                            let Some(type_node) = param.child_by_field_name("type") else {
                                continue;
                            };
                            let Some(decl) = param.child_by_field_name("declarator") else {
                                continue;
                            };
                            if declarator_name(decl, source).as_deref() == Some(name) {
                                return Some(strip_type_qualifiers(node_text(type_node, source)));
                            }
                        }
                    }
                }
            }
            if let Some(body) = n.child_by_field_name("body") {
                if let Some(found) = find_local_var_type(body, source, name) {
                    return Some(found);
                }
            }
            return None;
        }
        current = n.parent();
    }
    None
}

/// Recursively scan a function body (or nested block) for a local `declaration`
/// statement whose declarator matches `name`, returning its declared type.
/// Does not track scoping precision (shadowing across nested blocks is not
/// disambiguated) — a best-effort match is far better than the pre-fix
/// behavior of never resolving local-variable receivers at all.
fn find_local_var_type(body: Node, source: &[u8], name: &str) -> Option<String> {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "declaration" {
            let Some(type_node) = child.child_by_field_name("type") else {
                continue;
            };
            let mut decl_cursor = child.walk();
            for decl in child.children_by_field_name("declarator", &mut decl_cursor) {
                // `const zctField* ctField = init;` wraps the identifier in an
                // init_declarator before the pointer/reference declarator.
                let inner = if decl.kind() == "init_declarator" {
                    decl.child_by_field_name("declarator").unwrap_or(decl)
                } else {
                    decl
                };
                if declarator_name(inner, source).as_deref() == Some(name) {
                    return Some(strip_type_qualifiers(node_text(type_node, source)));
                }
            }
        }
        // `else_clause` wraps an else-branch's compound_statement (or a bare
        // statement, for `else if (...)`/single-statement else) — without
        // recursing into it, every local variable declared inside an `else`
        // block is invisible to this scan. `case_statement`/`for_statement`/
        // `while_statement` cover the other common nesting shapes real C++
        // uses around a "get pointer, null-check, then call through it"
        // pattern (see Src/High/TKE/MetaData/FieldAttributesHandler.cpp).
        const RECURSABLE_KINDS: &[&str] = &[
            "compound_statement",
            "if_statement",
            "else_clause",
            "for_statement",
            "while_statement",
            "do_statement",
            "switch_statement",
            "case_statement",
        ];
        if RECURSABLE_KINDS.contains(&child.kind()) {
            if let Some(found) = find_local_var_type(child, source, name) {
                return Some(found);
            }
        }
    }
    None
}

/// Descend through declarator wrappers to find the innermost `function_declarator`
/// (the node whose `parameters` field holds the parameter list).
fn find_function_declarator(node: Node) -> Option<Node> {
    if node.kind() == "function_declarator" {
        return Some(node);
    }
    node.child_by_field_name("declarator")
        .and_then(find_function_declarator)
}

/// Unwrap a declarator (`*field`, `&field`, `field`) down to its plain identifier text.
fn declarator_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, source)),
        "pointer_declarator" | "reference_declarator" => {
            // `pointer_declarator` exposes its inner declarator via the
            // "declarator" field, but `reference_declarator`'s `&`/identifier
            // children are unnamed in this grammar — fall back to scanning
            // named children by kind so `const IBase& ctx` still resolves.
            if let Some(found) = node
                .child_by_field_name("declarator")
                .and_then(|d| declarator_name(d, source))
            {
                return Some(found);
            }
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            children
                .into_iter()
                .find_map(|c| declarator_name(c, source))
        }
        _ => None,
    }
}

/// Strip `const`/`volatile` qualifiers, trailing `*`/`&`, and a namespace
/// prefix from a raw type-node's text, leaving just the base type name
/// (e.g. `"const tke::MappingMgr&"` -> `"MappingMgr"`). Without the
/// namespace strip, a receiver declared as `const ns::Class& x` resolves to
/// `"ns::Class"`, which never matches class_method_map's bare-class-name
/// keys — same failure mode as an unresolved raw variable name, just one
/// layer further in.
fn strip_type_qualifiers(raw: String) -> String {
    let cleaned = raw
        .replace("const", "")
        .replace("volatile", "")
        .trim_matches(|c: char| c == '*' || c == '&' || c.is_whitespace())
        .to_string();
    cleaned
        .rsplit("::")
        .next()
        .unwrap_or(&cleaned)
        .trim()
        .to_string()
}
