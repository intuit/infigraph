use sha2::{Digest, Sha256};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::analysis::cyclomatic_complexity;
use crate::model::{Span, Symbol, SymbolKind};

/// Extract symbols from a parsed AST using a Tree-sitter query.
///
/// The query must use these capture names:
///   @func.def / @func.name / @func.docstring / @func.decorator
///   @func.params / @func.return_type (both optional -- only needed when a
///     grammar doesn't expose "parameters"/"return_type"/"result" as named
///     fields on the captured @func.def node; see Kotlin/Dart for examples)
///   @method.def / @method.name / @method.docstring / @method.decorator
///   @method.params / @method.return_type (same as @func.params/@func.return_type)
///   @class.def / @class.name / @class.docstring / @class.decorator
///   @module.def / @module.name
///   @test.def / @test.name / @test.docstring
///   @var.def / @var.name
///   @route.def / @route.method / @route.path / @route.handler
pub fn extract_entities(
    file: &str,
    source: &[u8],
    root: Node,
    query: &Query,
    language: &str,
) -> Vec<Symbol> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);

    let capture_names = query.capture_names();

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name_text = None;
        let mut def_node = None;
        let mut kind = None;
        let mut docstring = None;
        let mut decorator = None;
        let mut params_capture: Option<String> = None;
        let mut return_type_capture: Option<String> = None;
        let mut route_method: Option<String> = None;
        let mut route_path: Option<String> = None;
        let mut route_handler: Option<String> = None;
        let mut route_def_node: Option<Node> = None;

        for capture in m.captures {
            let idx = capture.index as usize;
            let cap_name = capture_names[idx];
            let node = capture.node;

            match cap_name {
                "func.name" => {
                    name_text = Some(node_text(node, source));
                    if kind.is_none() {
                        kind = Some(SymbolKind::Function);
                    }
                }
                "func.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Function);
                    }
                }
                "func.docstring" => {
                    docstring = Some(strip_docstring(&node_text(node, source)));
                }
                "func.decorator" => {
                    decorator = Some(node_text(node, source));
                }
                "method.name" => {
                    name_text = Some(node_text(node, source));
                    kind = Some(SymbolKind::Method);
                }
                "method.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Method);
                    }
                }
                "method.docstring" => {
                    docstring = Some(strip_docstring(&node_text(node, source)));
                }
                "method.decorator" => {
                    decorator = Some(node_text(node, source));
                }
                "func.params" | "method.params" => {
                    params_capture = Some(node_text(node, source));
                }
                "func.return_type" | "method.return_type" => {
                    return_type_capture = Some(node_text(node, source));
                }
                "class.name" => {
                    name_text = Some(node_text(node, source));
                    if kind.is_none() {
                        kind = Some(SymbolKind::Class);
                    }
                }
                "class.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Class);
                    }
                }
                "class.docstring" => {
                    docstring = Some(strip_docstring(&node_text(node, source)));
                }
                "class.decorator" => {
                    decorator = Some(node_text(node, source));
                }
                "module.name" => {
                    let raw = node_text(node, source);
                    name_text = Some(strip_string_delimiters(&raw));
                    if kind.is_none() {
                        kind = Some(SymbolKind::Module);
                    }
                }
                "module.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Module);
                    }
                }
                "test.name" => {
                    name_text = Some(node_text(node, source));
                    kind = Some(SymbolKind::Test);
                }
                "test.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Test);
                    }
                }
                "test.docstring" => {
                    docstring = Some(strip_docstring(&node_text(node, source)));
                }
                "var.name" => {
                    name_text = Some(node_text(node, source));
                    if kind.is_none() {
                        kind = Some(SymbolKind::Variable);
                    }
                }
                "var.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Variable);
                    }
                }
                "section.name" => {
                    name_text = Some(node_text(node, source));
                    if kind.is_none() {
                        kind = Some(SymbolKind::Section);
                    }
                }
                "section.def" => {
                    def_node = Some(node);
                    if kind.is_none() {
                        kind = Some(SymbolKind::Section);
                    }
                }
                "route.method" => {
                    route_method = Some(node_text(node, source));
                }
                "route.path" => {
                    route_path = Some(strip_string_delimiters(&node_text(node, source)));
                }
                "route.handler" => {
                    route_handler = Some(node_text(node, source));
                }
                "route.def" => {
                    route_def_node = Some(node);
                }
                _ => {}
            }
        }

        // Prepend decorator/attribute text to docstring for searchability
        // If no decorator from query capture, try AST-based extraction (Rust attrs, Go comments, C# attrs)
        if decorator.is_none() {
            if let Some(node) = def_node {
                decorator = find_preceding_attributes(node, source);
            }
        }
        if let Some(dec) = decorator {
            let dec_clean = dec.trim().to_string();
            docstring = Some(match docstring {
                Some(doc) => format!("{} {}", dec_clean, doc),
                None => dec_clean,
            });
        }

        if let (Some(name), Some(node), Some(sym_kind)) = (name_text, def_node, kind) {
            let span = Span {
                file: file.to_string(),
                start_line: node.start_position().row as u32 + 1,
                start_col: node.start_position().column as u32,
                end_line: node.end_position().row as u32 + 1,
                end_col: node.end_position().column as u32,
            };

            let signature_hash = match sym_kind {
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Test => {
                    hash_body(node, source)
                }
                _ => hash_node(node, source),
            };

            // Find parent class for methods by walking up the AST
            let parent_class = find_parent_class(node, source);
            let id = if let Some(ref cls) = parent_class {
                format!("{}::{}::{}", file, cls, name)
            } else {
                format!("{}::{}", file, name)
            };
            let parent = parent_class.map(|cls| format!("{}::{}", file, cls));

            let complexity = match sym_kind {
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Test => {
                    cyclomatic_complexity(node)
                }
                _ => 1,
            };

            let parameters =
                params_capture.or_else(|| extract_child_text(node, "parameters", source));
            let return_type = return_type_capture.or_else(|| {
                extract_child_text(node, "return_type", source)
                    .or_else(|| extract_child_text(node, "result", source))
            });

            let visibility =
                extract_visibility(node, source).or_else(|| default_visibility(&name, language));

            symbols.push(Symbol {
                id,
                name,
                kind: sym_kind,
                span,
                signature_hash,
                parent,
                language: language.to_string(),
                visibility,
                docstring,
                complexity,
                parameters,
                return_type,
            });
        }

        // Create Route symbol from @route.* captures
        if let Some(path) = route_path {
            let method = route_method.unwrap_or_default().to_uppercase();
            let handler = route_handler.clone().unwrap_or_default();
            let route_name = if method.is_empty() {
                format!("ROUTE {}", path)
            } else {
                format!("{} {}", method, path)
            };
            let node = route_def_node.unwrap_or(def_node.unwrap_or(root));
            let span = Span {
                file: file.to_string(),
                start_line: node.start_position().row as u32 + 1,
                start_col: node.start_position().column as u32,
                end_line: node.end_position().row as u32 + 1,
                end_col: node.end_position().column as u32,
            };
            let id = format!("{}::{}", file, route_name.replace([' ', '/'], "_"));
            let docstring = if handler.is_empty() {
                Some(format!("route {} {}", method, path))
            } else {
                Some(format!("route {} {} handler={}", method, path, handler))
            };
            symbols.push(Symbol {
                id,
                name: route_name,
                kind: SymbolKind::Route,
                span,
                signature_hash: hash_node(node, source),
                parameters: None,
                return_type: None,
                parent: None,
                language: language.to_string(),
                visibility: None,
                docstring,
                complexity: 1,
            });
        }
    }

    for sym in &mut symbols {
        if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method)
            && (is_test_by_docstring(&sym.docstring)
                || is_test_by_name_and_path(&sym.name, file, &sym.language))
        {
            sym.kind = SymbolKind::Test;
        }
    }

    // Deduplicate by ID — multiple query patterns (e.g. bare vs. annotated
    // decorator variants, or a params/return_type-capturing pattern vs. an
    // annotation-capturing one) can match the same definition node, each
    // populating a different subset of fields. Prefer the more specific kind
    // (Test > Function/Method), keep the richest docstring seen so an
    // earlier bare-pattern match doesn't blank out a later match's
    // decorator/annotation text, and fill in parameters/return_type from any
    // duplicate match that has them if the kept entry doesn't yet.
    let mut seen = std::collections::HashMap::new();
    for sym in symbols {
        seen.entry(sym.id.clone())
            .and_modify(|existing: &mut Symbol| {
                if sym.kind == SymbolKind::Test && existing.kind == SymbolKind::Function {
                    existing.kind = sym.kind.clone();
                }
                let existing_len = existing.docstring.as_deref().map_or(0, str::len);
                let new_len = sym.docstring.as_deref().map_or(0, str::len);
                if new_len > existing_len {
                    existing.docstring = sym.docstring.clone();
                }
                if existing.parameters.is_none() && sym.parameters.is_some() {
                    existing.parameters = sym.parameters.clone();
                }
                if existing.return_type.is_none() && sym.return_type.is_some() {
                    existing.return_type = sym.return_type.clone();
                }
            })
            .or_insert(sym);
    }
    seen.into_values().collect()
}

/// Walk up the AST to find the enclosing class_definition and return its name.
fn find_parent_class(node: Node, source: &[u8]) -> Option<String> {
    // Same node-kind set as relations.rs's find_enclosing_class — that
    // function already covered Java/TS/JS/C#/Kotlin/Swift/Ruby/Rust/Elixir
    // correctly, but this one (which actually builds the class-scoped
    // Symbol.id "file::Class::method") only ever checked "class_definition"
    // (Python). Every other language's methods were silently staying
    // file-scoped ("file::method") instead of class-scoped — the same
    // failure mode the C++-specific class_specifier/struct_specifier fix
    // addressed, just never generalized to the rest of these languages.
    const CLASS_KINDS: &[&str] = &[
        "class_definition",  // Python
        "class_declaration", // Java, TS, JS, C#, Kotlin, Swift
        "class",             // Ruby
        "class_specifier",   // C/C++
        "struct_specifier",  // C/C++ struct
        "impl_item",         // Rust
        "struct_item",       // Rust
    ];
    let mut current = node.parent();
    while let Some(n) = current {
        if CLASS_KINDS.contains(&n.kind()) {
            if let Some(name_node) = n.child_by_field_name("name") {
                return Some(node_text(name_node, source));
            }
        }
        if n.kind() == "function_definition" {
            if let Some(declarator) = n.child_by_field_name("declarator") {
                if let Some(qualified) = find_qualified_identifier(declarator) {
                    if let Some(scope) = qualified.child_by_field_name("scope") {
                        return Some(node_text(scope, source));
                    }
                }
            }
        }
        // Protobuf: an `rpc` lives inside a `service` node. The service name has
        // no "name" field — it's a `service_name` child. Without this, proto RPC
        // methods get an empty parent, so gRPC contract extraction (which groups
        // RPCs under their service) finds nothing (AIF3X-331 #21).
        if n.kind() == "service" {
            let mut cursor = n.walk();
            let name = n
                .children(&mut cursor)
                .find(|c| c.kind() == "service_name" || c.kind() == "message_name")
                .map(|name_node| node_text(name_node, source));
            if let Some(name) = name {
                return Some(name);
            }
        }
        current = n.parent();
    }
    None
}

/// Descend through declarator wrappers (pointer/reference return types, e.g.
/// `const char* Class::method()`) to find a `qualified_identifier` — the
/// `Class::method` name node in an out-of-line C++ method definition.
fn find_qualified_identifier(node: Node) -> Option<Node> {
    if node.kind() == "qualified_identifier" {
        return Some(node);
    }
    if node.kind() == "function_declarator" {
        return node
            .child_by_field_name("declarator")
            .and_then(find_qualified_identifier);
    }
    node.child_by_field_name("declarator")
        .and_then(find_qualified_identifier)
}

/// Look at preceding siblings for attribute/decorator nodes.
/// Handles: Rust `attribute_item` (#[get("/path")]), C# `attribute_list` ([HttpGet]),
/// Go preceding line comments (// @router /api/users [get]), and similar patterns.
fn find_preceding_attributes(node: Node, source: &[u8]) -> Option<String> {
    // Node kinds that represent decorators/attributes across languages
    const ATTR_KINDS: &[&str] = &[
        "attribute_item",    // Rust: #[get("/path")]
        "attribute_list",    // C#: [HttpGet], PHP 8: #[Route("/path")]
        "attribute",         // C# inner, PHP inner
        "annotation",        // Kotlin, Scala, Java (fallback)
        "decorator",         // TypeScript/JS (NestJS @Controller, @Get)
        "marker_annotation", // Java @Override, @GetMapping
    ];

    // Comment kinds that may contain route annotations (Go swagger, JSDoc)
    const COMMENT_KINDS: &[&str] = &["comment", "line_comment", "block_comment"];

    let mut attrs = Vec::new();

    // Collect from preceding siblings
    collect_attrs(node, source, ATTR_KINDS, COMMENT_KINDS, &mut attrs);

    // Also check parent's preceding siblings (for attributes at different nesting)
    if attrs.is_empty() {
        if let Some(parent) = node.parent() {
            collect_attrs(parent, source, ATTR_KINDS, COMMENT_KINDS, &mut attrs);
        }
    }

    if attrs.is_empty() {
        None
    } else {
        attrs.reverse();
        Some(attrs.join(" "))
    }
}

/// Collect attribute/decorator nodes from preceding siblings.
fn collect_attrs(
    node: Node,
    source: &[u8],
    attr_kinds: &[&str],
    comment_kinds: &[&str],
    attrs: &mut Vec<String>,
) {
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if attr_kinds.contains(&sib.kind()) {
            attrs.push(node_text(sib, source));
            sibling = sib.prev_sibling();
        } else if comment_kinds.contains(&sib.kind()) {
            // Only capture annotation-like comments: // @Router, /// @route, # @app.route
            let text = node_text(sib, source);
            if text.contains("@")
                || text.contains("route")
                || text.contains("endpoint")
                || text.contains("handler")
                || text.contains("API")
            {
                attrs.push(text);
            }
            sibling = sib.prev_sibling();
        } else {
            break;
        }
    }
}

fn node_text(node: Node, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

fn extract_child_text(node: Node, field_name: &str, source: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field_name)?;
    let text = child.utf8_text(source).ok()?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Extract visibility from AST node. Works across languages by checking for
/// common visibility-related child node kinds.
fn extract_visibility(node: Node, source: &[u8]) -> Option<String> {
    // Check named children for visibility-related node kinds
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            // Rust: pub, pub(crate), pub(super)
            "visibility_modifier" => {
                let text = child.utf8_text(source).ok()?.trim().to_string();
                return Some(text);
            }
            // Java/C#/Kotlin/Groovy: modifiers node containing access keywords
            "modifiers" | "modifier_list" => {
                let mut mod_cursor = child.walk();
                for modifier in child.children(&mut mod_cursor) {
                    let kind = modifier.kind();
                    if kind == "public"
                        || kind == "private"
                        || kind == "protected"
                        || kind == "internal"
                    {
                        return Some(kind.to_string());
                    }
                    // C#/Java text-based modifier check
                    if let Ok(text) = modifier.utf8_text(source) {
                        let t = text.trim();
                        if t == "public" || t == "private" || t == "protected" || t == "internal" {
                            return Some(t.to_string());
                        }
                    }
                }
            }
            // Swift: modifier with visibility_modifier child
            "modifier" => {
                if let Ok(text) = child.utf8_text(source) {
                    let t = text.trim();
                    if t == "public"
                        || t == "private"
                        || t == "internal"
                        || t == "fileprivate"
                        || t == "open"
                    {
                        return Some(t.to_string());
                    }
                }
            }
            // Direct keyword children (some grammars)
            "public" | "private" | "protected" | "internal" => {
                return Some(child.kind().to_string());
            }
            _ => {}
        }
    }
    None
}

/// Fallback visibility for languages that have no explicit access-modifier
/// keyword (Python, JavaScript, Ruby, Go, …). `extract_visibility` returns
/// `None` for these, which previously stored an empty string and made every
/// such symbol invisible to `get_api_surface` (WHERE s.visibility = 'public').
///
/// Convention: a leading underscore marks a symbol private (Python `_x`,
/// JS/TS `_x`); Go uses capitalization (lower-case first letter = unexported).
/// Everything else defaults to `public`. Languages WITH real modifiers
/// (rust/java/c#/kotlin/swift/…) are intentionally excluded — for those,
/// absence of a modifier is a deliberate signal (package-private / module-
/// private) and must not be rewritten to public.
fn default_visibility(name: &str, language: &str) -> Option<String> {
    let public = Some("public".to_string());
    let private = Some("private".to_string());
    match language {
        "python" | "javascript" | "typescript" | "ruby" | "lua" | "r" => {
            if name.starts_with('_') {
                private
            } else {
                public
            }
        }
        // Go: exported iff the first letter is upper-case.
        "go" => match name.chars().next() {
            Some(c) if c.is_uppercase() => public,
            Some(_) => private,
            None => None,
        },
        // Languages with explicit modifiers: leave as-is (None → stored empty),
        // so we don't override a meaningful "no modifier" with "public".
        _ => None,
    }
}

const TEST_ATTR_PATTERNS: &[&str] = &[
    "#[test]",
    "#[tokio::test]",
    "#[rstest]",
    "#[test_case",
    "@Test",
    "@ParameterizedTest",
    "@RepeatedTest",
    "[Test]",
    "[Fact]",
    "[Theory]",
    "[TestMethod]",
    "@pytest.mark",
    "@unittest",
];

fn is_test_by_docstring(docstring: &Option<String>) -> bool {
    match docstring {
        Some(doc) => TEST_ATTR_PATTERNS.iter().any(|pat| doc.contains(pat)),
        None => false,
    }
}

fn is_test_by_name_and_path(name: &str, file: &str, language: &str) -> bool {
    let file_lower = file.to_lowercase();
    let in_test_dir = file_lower.contains("/test/")
        || file_lower.contains("/tests/")
        || file_lower.contains("/__tests__/")
        || file_lower.contains("/spec/")
        || file_lower.starts_with("test/")
        || file_lower.starts_with("tests/")
        || file_lower.starts_with("spec/")
        || file_lower.starts_with("__tests__/");

    match language {
        // JS/TS: *.test.ts, *.spec.js, __tests__/ with describe/it/test
        "typescript" | "javascript" => {
            file_lower.ends_with(".test.ts")
                || file_lower.ends_with(".test.tsx")
                || file_lower.ends_with(".test.js")
                || file_lower.ends_with(".test.jsx")
                || file_lower.ends_with(".test.mjs")
                || file_lower.ends_with(".spec.ts")
                || file_lower.ends_with(".spec.tsx")
                || file_lower.ends_with(".spec.js")
                || file_lower.ends_with(".spec.jsx")
                || (in_test_dir && (name == "it" || name == "test" || name == "describe"))
        }
        // Go: func TestXxx in _test.go
        "go" => {
            file_lower.ends_with("_test.go")
                && (name.starts_with("Test")
                    || name.starts_with("Benchmark")
                    || name.starts_with("Fuzz"))
        }
        // Python: test_ prefix in test files/dirs, or pytest/unittest via docstring (handled separately)
        "python" => {
            name.starts_with("test_")
                && (file_lower.contains("test_") || file_lower.ends_with("_test.py") || in_test_dir)
        }
        // Ruby: RSpec it/describe in _spec.rb, Minitest test_ in _test.rb
        "ruby" => {
            (name.starts_with("test_") && in_test_dir)
                || (file_lower.ends_with("_spec.rb")
                    && (name == "it"
                        || name == "specify"
                        || name == "describe"
                        || name == "context"))
                || (file_lower.ends_with("_test.rb") && name.starts_with("test_"))
        }
        // PHP: PHPUnit test* methods in *Test.php
        "php" => name.starts_with("test") && (file_lower.ends_with("test.php") || in_test_dir),
        // Swift: XCTest func testXxx in *Tests.swift
        "swift" => {
            name.starts_with("test")
                && (file_lower.ends_with("tests.swift")
                    || file_lower.ends_with("test.swift")
                    || in_test_dir)
        }
        // Scala: ScalaTest it/test/describe in spec/test dirs or files
        "scala" => {
            (in_test_dir || file_lower.contains("src/test/"))
                && (name == "test"
                    || name == "it"
                    || name == "describe"
                    || file_lower.ends_with("spec.scala")
                    || file_lower.ends_with("test.scala"))
        }
        // Dart: *_test.dart files, test()/testWidgets()/group() in test/
        "dart" => {
            file_lower.ends_with("_test.dart")
                || (in_test_dir && (name == "test" || name == "testWidgets" || name == "group"))
        }
        // Elixir: test macro in *_test.exs
        "elixir" => {
            file_lower.ends_with("_test.exs")
                && (name == "test" || name.starts_with("test ") || name == "describe")
        }
        // Lua: busted describe/it/spec, or in spec/ or test/ dirs
        "lua" => {
            in_test_dir
                && (name == "describe" || name == "it" || name == "spec" || name == "pending")
        }
        // Perl: Test::More/Test2 — functions in .t files
        "perl" => file_lower.ends_with(".t"),
        // R: testthat test_that() in test/ dirs
        "r" => in_test_dir && (name == "test_that" || name.starts_with("test_")),
        // Julia: @test macro in test/ dir or runtests.jl
        "julia" => in_test_dir || file_lower.ends_with("runtests.jl"),
        // Haskell: HSpec describe/it, or HUnit test* in test/ or spec/
        "haskell" => {
            (in_test_dir || file_lower.contains("/spec/"))
                && (name == "describe"
                    || name == "it"
                    || name == "spec"
                    || name.starts_with("test"))
        }
        // Erlang: EUnit test_/0, Common Test *_SUITE in test/
        "erlang" => {
            (file_lower.ends_with("_test.erl")
                || file_lower.ends_with("_tests.erl")
                || file_lower.ends_with("_suite.erl"))
                || (in_test_dir && name.starts_with("test_"))
        }
        // Zig: test blocks — zig names them "test" or "test \"description\""
        "zig" => name == "test" || name.starts_with("test "),
        // F#: NUnit/xUnit [Test]/[Fact] via docstring (retag handles), Expecto testCase in test dirs
        "fsharp" => {
            in_test_dir && (name == "testCase" || name == "testList" || name.starts_with("test"))
        }
        // Groovy: Spock where/then in *Spec.groovy, JUnit via docstring retag
        "groovy" => {
            file_lower.ends_with("spec.groovy")
                || file_lower.ends_with("test.groovy")
                || (in_test_dir && name.starts_with("test"))
        }
        // Kotlin: JUnit @Test via docstring retag, kotest describe/it in test dirs
        "kotlin" => {
            in_test_dir
                && (name == "describe"
                    || name == "it"
                    || name == "test"
                    || name.starts_with("test"))
        }
        // C#: NUnit/xUnit/MSTest via docstring retag, this catches test dir heuristic
        "csharp" => {
            (file_lower.ends_with("test.cs") || file_lower.ends_with("tests.cs"))
                && name.starts_with("Test")
        }
        // Objective-C: XCTest testXxx in *Tests.m
        "objc" => {
            name.starts_with("test")
                && (file_lower.ends_with("tests.m")
                    || file_lower.ends_with("test.m")
                    || in_test_dir)
        }
        // OCaml: Alcotest/OUnit test_ in test/ dir
        "ocaml" => in_test_dir && name.starts_with("test"),
        // Fortran: pFUnit test_ in test/ dir
        "fortran" => in_test_dir && name.starts_with("test"),
        // PowerShell: Pester Describe/It/Context in *.Tests.ps1
        "powershell" => {
            file_lower.ends_with(".tests.ps1")
                && (name == "Describe" || name == "It" || name == "Context")
        }
        // Bash: bats @test in .bats files, or test_ functions in test/ dir
        "bash" => file_lower.ends_with(".bats") || (in_test_dir && name.starts_with("test_")),
        // Svelte: same as JS/TS — *.test.ts patterns
        "svelte" => {
            file_lower.ends_with(".test.ts") || file_lower.ends_with(".spec.ts") || in_test_dir
        }
        // C++: GTest TEST/TEST_F/TEST_P, Catch2 TEST_CASE/SCENARIO, Boost test*
        "cpp" => {
            (name == "TEST"
                || name == "TEST_F"
                || name == "TEST_P"
                || name == "TEST_CASE"
                || name == "SCENARIO"
                || name == "SECTION"
                || name == "BOOST_AUTO_TEST_CASE"
                || name == "BOOST_FIXTURE_TEST_CASE")
                || (in_test_dir && name.starts_with("test"))
                || file_lower.ends_with("_test.cpp")
                || file_lower.ends_with("_test.cc")
                || file_lower.ends_with("_tests.cpp")
                || file_lower.ends_with("_tests.cc")
        }
        // C: CUnit, Unity, Check — test_ prefix in test files/dirs
        "c" => {
            (in_test_dir && name.starts_with("test"))
                || file_lower.ends_with("_test.c")
                || file_lower.ends_with("_tests.c")
        }
        // CUDA: same as C++ patterns
        "cuda" => {
            (name == "TEST" || name == "TEST_F" || name == "TEST_P")
                || (in_test_dir && name.starts_with("test"))
                || file_lower.ends_with("_test.cu")
        }
        // Clojure: deftest macro — name is the test function name, file in test/ dir
        "clojure" => {
            in_test_dir || file_lower.ends_with("_test.clj") || file_lower.ends_with("_test.cljc")
        }
        // Elm: Test.describe/Test.test in tests/ dir
        "elm" => {
            in_test_dir
                || (name == "describe" || name == "test" || name == "fuzz")
                    && file_lower.contains("test")
        }
        // Common Lisp: FiveAM def-test, in test/ dirs
        "commonlisp" => in_test_dir && (name.starts_with("test") || name == "def-test"),
        // Pascal: DUnit/FPCUnit test* methods in *Test.pas
        "pascal" => {
            name.starts_with("Test")
                && (file_lower.ends_with("test.pas")
                    || file_lower.ends_with("tests.pas")
                    || in_test_dir)
        }
        // Rust/Java/C#/Kotlin: handled by docstring retag
        _ => false,
    }
}

fn hash_node(node: Node, source: &[u8]) -> String {
    let mut hasher = Sha256::new();
    let text = &source[node.byte_range()];
    hasher.update(text);
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Hash only the body of a function/method node, excluding the signature line.
/// This allows rename detection: two functions with different names but identical
/// bodies will produce the same hash.
fn hash_body(node: Node, source: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        hash_node(body, source)
    } else {
        // Fallback: try "block" field (used by some grammars like Java/C#)
        if let Some(block) = node.child_by_field_name("block") {
            hash_node(block, source)
        } else {
            hash_node(node, source)
        }
    }
}

/// Strip string delimiters (quotes) from a captured path string.
fn strip_string_delimiters(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let s = s.strip_prefix('\'').unwrap_or(s);
    let s = s.strip_suffix('\'').unwrap_or(s);
    let s = s.strip_prefix('`').unwrap_or(s);
    let s = s.strip_suffix('`').unwrap_or(s);
    s.to_string()
}

/// Strip triple-quote delimiters and leading whitespace from a docstring.
fn strip_docstring(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix("\"\"\"")
        .or_else(|| s.strip_prefix("'''"))
        .unwrap_or(s);
    let s = s
        .strip_suffix("\"\"\"")
        .or_else(|| s.strip_suffix("'''"))
        .unwrap_or(s);
    // Dedent: find minimum indentation and strip it
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 1 {
        return s.trim().to_string();
    }
    let min_indent = lines[1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut result = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            result.push_str(line.trim());
        } else if line.len() >= min_indent {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&line[min_indent..]);
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line.trim());
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    #[test]
    fn test_module_capture_produces_module_symbol() {
        // Use Python grammar: capture identifier as @module.name and enclosing
        // assignment as @module.def — two distinct nodes, proving arm independence.
        let grammar = tree_sitter_python::LANGUAGE.into();
        let src = b"MyModule = 1";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        // identifier node (name) and assignment node (def) are distinct
        let query = tree_sitter::Query::new(
            &grammar,
            r#"(assignment left: (identifier) @module.name) @module.def"#,
        )
        .unwrap();

        let symbols = extract_entities("test.bas", src, root, &query, "vb6");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, crate::model::SymbolKind::Module);
        assert_eq!(symbols[0].name, "MyModule");
    }

    #[test]
    fn test_python_function_extracts_parameters_and_return_type() {
        let grammar = tree_sitter_python::LANGUAGE.into();
        let src = b"def greet(name: str, age: int) -> str:\n    return f'hello {name}'\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_definition name: (identifier) @func.name) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("greet.py", src, root, &query, "python");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(
            symbols[0].parameters.is_some(),
            "parameters should be extracted"
        );
        assert!(
            symbols[0].parameters.as_deref().unwrap().contains("name"),
            "parameters should contain param names: {:?}",
            symbols[0].parameters
        );
        assert!(
            symbols[0].return_type.is_some(),
            "return_type should be extracted for typed Python"
        );
        assert_eq!(symbols[0].return_type.as_deref(), Some("str"));
    }

    #[test]
    fn test_python_function_no_return_type() {
        let grammar = tree_sitter_python::LANGUAGE.into();
        let src = b"def hello(x):\n    pass\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_definition name: (identifier) @func.name) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("hello.py", src, root, &query, "python");
        assert_eq!(symbols.len(), 1);
        assert!(symbols[0].parameters.is_some());
        assert!(
            symbols[0].return_type.is_none(),
            "no return type annotation"
        );
    }

    #[test]
    fn test_kotlin_function_extracts_parameters_and_return_type() {
        // tree-sitter-kotlin-ng's function_declaration has no named
        // "parameters"/"return_type" fields at all -- function_value_parameters
        // and the return type are both positional children. Verified against
        // the crate's real node-types.json before writing this test.
        let grammar = tree_sitter_kotlin_ng::LANGUAGE.into();
        let src = b"fun greet(name: String, age: Int): String {\n    return name\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_declaration\n  \
               name: (identifier) @func.name\n  \
               (function_value_parameters) @func.params\n  \
               (type)? @func.return_type) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("greet.kt", src, root, &query, "kotlin");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(
            symbols[0].parameters.is_some(),
            "parameters should be extracted via the @func.params capture"
        );
        assert!(
            symbols[0].parameters.as_deref().unwrap().contains("name"),
            "parameters should contain param names: {:?}",
            symbols[0].parameters
        );
        assert_eq!(
            symbols[0].return_type.as_deref(),
            Some("String"),
            "return_type should be extracted via the @func.return_type capture"
        );
    }

    #[test]
    fn test_kotlin_function_without_return_type_annotation() {
        // @func.return_type is an optional capture (`(type)?`) -- must not
        // panic or produce a bogus value when a function has no explicit
        // return type (Kotlin infers Unit).
        let grammar = tree_sitter_kotlin_ng::LANGUAGE.into();
        let src = b"fun sayHi(name: String) {\n    println(name)\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_declaration\n  \
               name: (identifier) @func.name\n  \
               (function_value_parameters) @func.params\n  \
               (type)? @func.return_type) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("sayHi.kt", src, root, &query, "kotlin");
        assert_eq!(symbols.len(), 1);
        assert!(symbols[0].parameters.is_some());
        assert_eq!(symbols[0].return_type, None);
    }

    #[test]
    fn test_dedup_merges_params_from_one_pattern_with_docstring_from_another() {
        // Regression test for a real interaction bug: when two separate
        // .scm patterns match the same function_declaration node -- one
        // capturing @func.params/@func.return_type (no decorator), another
        // capturing @func.decorator/@func.docstring (no params) -- the
        // dedup step used to keep only whichever match's Symbol was
        // inserted first, silently dropping the other match's fields.
        // Both parameters/return_type AND docstring must survive together.
        let grammar = tree_sitter_kotlin_ng::LANGUAGE.into();
        let src = b"@PostMapping(\"/x\")\nfun render(templateId: String): String {\n    return templateId\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_declaration\n  \
               name: (identifier) @func.name\n  \
               (function_value_parameters) @func.params\n  \
               (type)? @func.return_type) @func.def\n\
             (function_declaration\n  \
               (modifiers\n    \
                 (annotation\n      \
                   (constructor_invocation\n        \
                     (user_type\n          \
                       (identifier) @func.decorator)\n        \
                     (value_arguments\n          \
                       (value_argument\n            \
                         (string_literal\n              \
                           (string_content) @func.docstring))?))))\n  \
               name: (identifier) @func.name) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("render.kt", src, root, &query, "kotlin");
        assert_eq!(symbols.len(), 1);
        assert!(
            symbols[0].parameters.is_some(),
            "parameters from the params-capturing pattern must survive dedup"
        );
        assert_eq!(
            symbols[0].return_type.as_deref(),
            Some("String"),
            "return_type from the params-capturing pattern must survive dedup"
        );
        assert_eq!(
            symbols[0].docstring.as_deref(),
            Some("PostMapping /x"),
            "docstring from the annotation-capturing pattern must survive dedup"
        );
    }

    #[test]
    fn test_dart_function_extracts_parameters_and_return_type() {
        // tree-sitter-dart's function_signature DOES have real "parameters"/
        // "return_type" named fields -- but they're one level below the
        // captured function_declaration node (reached via its "signature"
        // field), so extract_child_text's direct child_by_field_name lookup
        // on the outer node misses them. Verified against the crate's real
        // node-types.json before writing this test.
        let grammar = tree_sitter_dart::LANGUAGE.into();
        let src = b"String greet(String name, int age) {\n  return 'hello';\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_declaration\n  \
               signature: (function_signature\n    \
                 return_type: (type)? @func.return_type\n    \
                 name: (identifier) @func.name\n    \
                 parameters: (formal_parameter_list) @func.params)) @func.def",
        )
        .unwrap();

        let symbols = extract_entities("greet.dart", src, root, &query, "dart");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(
            symbols[0].parameters.is_some(),
            "parameters should be extracted via the @func.params capture"
        );
        assert!(
            symbols[0].parameters.as_deref().unwrap().contains("name"),
            "parameters should contain param names: {:?}",
            symbols[0].parameters
        );
        assert_eq!(symbols[0].return_type.as_deref(), Some("String"));
    }

    #[test]
    fn test_dart_method_extracts_parameters_and_return_type() {
        // method_signature has ZERO named fields at all (verified against
        // node-types.json) -- structurally unreachable via child_by_field_name
        // from the captured node regardless of nesting depth, unlike the
        // top-level function case which is merely "one level too shallow".
        let grammar = tree_sitter_dart::LANGUAGE.into();
        let src =
            b"class Greeter {\n  String greet(String name, int age) {\n    return 'hi';\n  }\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(method_signature\n  \
               (function_signature\n    \
                 return_type: (type)? @method.return_type\n    \
                 name: (identifier) @method.name\n    \
                 parameters: (formal_parameter_list) @method.params)) @method.def",
        )
        .unwrap();

        let symbols = extract_entities("greeter.dart", src, root, &query, "dart");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(
            symbols[0].parameters.is_some(),
            "parameters should be extracted for Dart methods too, not just top-level functions"
        );
        assert!(
            symbols[0].parameters.as_deref().unwrap().contains("name"),
            "parameters should contain param names: {:?}",
            symbols[0].parameters
        );
        assert_eq!(symbols[0].return_type.as_deref(), Some("String"));
    }

    #[test]
    fn test_dart_constructor_still_works_unchanged() {
        // constructor_signature has direct name+parameters fields, no
        // nesting -- already worked before this plan and must keep working,
        // using the existing child_by_field_name fallback (no @method.params
        // capture needed or added for constructors).
        let grammar = tree_sitter_dart::LANGUAGE.into();
        let src = b"class Greeter {\n  Greeter(String name, int age);\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(
            &grammar,
            "(constructor_signature name: (identifier) @method.name) @method.def",
        )
        .unwrap();

        let symbols = extract_entities("greeter.dart", src, root, &query, "dart");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Greeter");
        assert!(
            symbols[0].parameters.is_some(),
            "constructor parameters must still work via the pre-existing fallback"
        );
    }

    #[test]
    fn test_is_test_by_name_and_path_go() {
        assert!(is_test_by_name_and_path(
            "TestFoo",
            "pkg/auth/auth_test.go",
            "go"
        ));
        assert!(is_test_by_name_and_path(
            "BenchmarkHash",
            "hash_test.go",
            "go"
        ));
        assert!(is_test_by_name_and_path("FuzzParse", "parse_test.go", "go"));
        assert!(!is_test_by_name_and_path(
            "TestFoo",
            "pkg/auth/auth.go",
            "go"
        ));
        assert!(!is_test_by_name_and_path("helper", "auth_test.go", "go"));
    }

    #[test]
    fn test_is_test_by_name_and_path_typescript() {
        assert!(is_test_by_name_and_path(
            "it",
            "src/auth.test.ts",
            "typescript"
        ));
        assert!(is_test_by_name_and_path(
            "describe",
            "src/auth.spec.tsx",
            "typescript"
        ));
        assert!(is_test_by_name_and_path(
            "test",
            "src/__tests__/auth.ts",
            "typescript"
        ));
        assert!(!is_test_by_name_and_path("it", "src/auth.ts", "typescript"));
    }

    // AIF3X-331 #20: keyword-less languages must default to a concrete
    // visibility so get_api_surface (WHERE s.visibility = 'public') surfaces
    // them. Previously extract_visibility returned None → stored '' → invisible.
    #[test]
    fn test_default_visibility_keywordless_langs() {
        // Underscore-convention private
        for lang in ["python", "javascript", "typescript", "ruby", "lua", "r"] {
            assert_eq!(
                default_visibility("public_fn", lang).as_deref(),
                Some("public"),
                "{lang}: bare name should default public"
            );
            assert_eq!(
                default_visibility("_private_fn", lang).as_deref(),
                Some("private"),
                "{lang}: leading underscore should be private"
            );
        }
        // Go: exported iff capitalized
        assert_eq!(
            default_visibility("Exported", "go").as_deref(),
            Some("public")
        );
        assert_eq!(
            default_visibility("unexported", "go").as_deref(),
            Some("private")
        );
        // Languages with real modifiers keep None (absence of a keyword is a
        // deliberate signal there, not "public").
        assert_eq!(default_visibility("foo", "rust"), None);
        assert_eq!(default_visibility("Foo", "java"), None);
    }

    #[test]
    fn test_is_test_by_name_and_path_javascript() {
        assert!(is_test_by_name_and_path(
            "it",
            "src/utils.test.js",
            "javascript"
        ));
        assert!(is_test_by_name_and_path(
            "test",
            "src/utils.spec.jsx",
            "javascript"
        ));
        assert!(is_test_by_name_and_path(
            "describe",
            "src/utils.test.mjs",
            "javascript"
        ));
        assert!(!is_test_by_name_and_path(
            "render",
            "src/utils.js",
            "javascript"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_python() {
        assert!(is_test_by_name_and_path(
            "test_login",
            "tests/test_auth.py",
            "python"
        ));
        assert!(is_test_by_name_and_path(
            "test_foo",
            "test/test_foo.py",
            "python"
        ));
        assert!(is_test_by_name_and_path(
            "test_bar",
            "src/bar_test.py",
            "python"
        ));
        assert!(!is_test_by_name_and_path(
            "helper",
            "tests/test_auth.py",
            "python"
        ));
        assert!(!is_test_by_name_and_path(
            "test_login",
            "src/auth.py",
            "python"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_ruby() {
        assert!(is_test_by_name_and_path(
            "it",
            "spec/models/user_spec.rb",
            "ruby"
        ));
        assert!(is_test_by_name_and_path(
            "describe",
            "spec/auth_spec.rb",
            "ruby"
        ));
        assert!(is_test_by_name_and_path(
            "test_login",
            "test/auth_test.rb",
            "ruby"
        ));
        assert!(!is_test_by_name_and_path(
            "helper",
            "spec/user_spec.rb",
            "ruby"
        ));
        assert!(!is_test_by_name_and_path(
            "it",
            "app/models/user.rb",
            "ruby"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_php() {
        assert!(is_test_by_name_and_path(
            "testLogin",
            "tests/AuthTest.php",
            "php"
        ));
        assert!(is_test_by_name_and_path(
            "testCreate",
            "test/UserTest.php",
            "php"
        ));
        assert!(!is_test_by_name_and_path(
            "helper",
            "tests/AuthTest.php",
            "php"
        ));
        assert!(!is_test_by_name_and_path(
            "testLogin",
            "src/Auth.php",
            "php"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_swift() {
        assert!(is_test_by_name_and_path(
            "testLogin",
            "AuthTests.swift",
            "swift"
        ));
        assert!(is_test_by_name_and_path(
            "testFoo",
            "Tests/FooTest.swift",
            "swift"
        ));
        assert!(!is_test_by_name_and_path(
            "helper",
            "AuthTests.swift",
            "swift"
        ));
        assert!(!is_test_by_name_and_path(
            "testLogin",
            "Sources/Auth.swift",
            "swift"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_elixir() {
        assert!(is_test_by_name_and_path(
            "test",
            "test/auth_test.exs",
            "elixir"
        ));
        assert!(is_test_by_name_and_path(
            "describe",
            "test/user_test.exs",
            "elixir"
        ));
        assert!(!is_test_by_name_and_path("test", "lib/auth.ex", "elixir"));
        assert!(!is_test_by_name_and_path(
            "helper",
            "test/auth_test.exs",
            "elixir"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_lua() {
        assert!(is_test_by_name_and_path(
            "describe",
            "spec/auth_spec.lua",
            "lua"
        ));
        assert!(is_test_by_name_and_path("it", "test/utils_test.lua", "lua"));
        assert!(!is_test_by_name_and_path("describe", "src/auth.lua", "lua"));
    }

    #[test]
    fn test_is_test_by_name_and_path_perl() {
        assert!(is_test_by_name_and_path("subtest", "t/auth.t", "perl"));
        assert!(is_test_by_name_and_path("ok", "t/01-basic.t", "perl"));
        assert!(!is_test_by_name_and_path("ok", "lib/Auth.pm", "perl"));
    }

    #[test]
    fn test_is_test_by_name_and_path_r() {
        assert!(is_test_by_name_and_path(
            "test_that",
            "tests/test-auth.R",
            "r"
        ));
        assert!(is_test_by_name_and_path(
            "test_login",
            "tests/testthat/test-login.R",
            "r"
        ));
        assert!(!is_test_by_name_and_path("test_that", "R/auth.R", "r"));
    }

    #[test]
    fn test_is_test_by_name_and_path_julia() {
        assert!(is_test_by_name_and_path("foo", "test/runtests.jl", "julia"));
        assert!(is_test_by_name_and_path(
            "bar",
            "test/auth_tests.jl",
            "julia"
        ));
        assert!(!is_test_by_name_and_path("foo", "src/Auth.jl", "julia"));
    }

    #[test]
    fn test_is_test_by_name_and_path_haskell() {
        assert!(is_test_by_name_and_path(
            "describe",
            "test/AuthSpec.hs",
            "haskell"
        ));
        assert!(is_test_by_name_and_path(
            "it",
            "spec/AuthSpec.hs",
            "haskell"
        ));
        assert!(is_test_by_name_and_path(
            "testLogin",
            "test/Auth.hs",
            "haskell"
        ));
        assert!(!is_test_by_name_and_path(
            "describe",
            "src/Auth.hs",
            "haskell"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_erlang() {
        assert!(is_test_by_name_and_path(
            "test_login",
            "test/auth_test.erl",
            "erlang"
        ));
        assert!(is_test_by_name_and_path(
            "init",
            "test/auth_SUITE.erl",
            "erlang"
        ));
        assert!(is_test_by_name_and_path(
            "test_foo",
            "test/bar_tests.erl",
            "erlang"
        ));
        assert!(!is_test_by_name_and_path(
            "handle_call",
            "src/auth.erl",
            "erlang"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_zig() {
        assert!(is_test_by_name_and_path("test", "src/auth.zig", "zig"));
        assert!(is_test_by_name_and_path(
            "test allocator",
            "src/mem.zig",
            "zig"
        ));
        assert!(!is_test_by_name_and_path("init", "src/auth.zig", "zig"));
    }

    #[test]
    fn test_is_test_by_name_and_path_fsharp() {
        assert!(is_test_by_name_and_path(
            "testCase",
            "tests/AuthTests.fs",
            "fsharp"
        ));
        assert!(is_test_by_name_and_path(
            "testLogin",
            "test/Auth.fs",
            "fsharp"
        ));
        assert!(!is_test_by_name_and_path(
            "testCase",
            "src/Auth.fs",
            "fsharp"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_groovy() {
        assert!(is_test_by_name_and_path(
            "testLogin",
            "src/test/AuthSpec.groovy",
            "groovy"
        ));
        assert!(is_test_by_name_and_path("foo", "AuthTest.groovy", "groovy"));
        assert!(!is_test_by_name_and_path(
            "handle",
            "src/Auth.groovy",
            "groovy"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_kotlin() {
        assert!(is_test_by_name_and_path(
            "testLogin",
            "src/test/AuthTest.kt",
            "kotlin"
        ));
        assert!(is_test_by_name_and_path(
            "describe",
            "test/AuthSpec.kt",
            "kotlin"
        ));
        assert!(!is_test_by_name_and_path("handle", "src/Auth.kt", "kotlin"));
    }

    #[test]
    fn test_is_test_by_name_and_path_csharp() {
        assert!(is_test_by_name_and_path(
            "TestLogin",
            "AuthTest.cs",
            "csharp"
        ));
        assert!(is_test_by_name_and_path(
            "TestCreate",
            "UserTests.cs",
            "csharp"
        ));
        assert!(!is_test_by_name_and_path("TestLogin", "Auth.cs", "csharp"));
        assert!(!is_test_by_name_and_path("Handle", "AuthTest.cs", "csharp"));
    }

    #[test]
    fn test_is_test_by_name_and_path_objc() {
        assert!(is_test_by_name_and_path("testLogin", "AuthTests.m", "objc"));
        assert!(is_test_by_name_and_path(
            "testFoo",
            "tests/FooTest.m",
            "objc"
        ));
        assert!(!is_test_by_name_and_path("viewDidLoad", "Auth.m", "objc"));
    }

    #[test]
    fn test_is_test_by_name_and_path_ocaml() {
        assert!(is_test_by_name_and_path(
            "test_login",
            "test/auth_test.ml",
            "ocaml"
        ));
        assert!(!is_test_by_name_and_path(
            "test_login",
            "lib/auth.ml",
            "ocaml"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_powershell() {
        assert!(is_test_by_name_and_path(
            "Describe",
            "Auth.Tests.ps1",
            "powershell"
        ));
        assert!(is_test_by_name_and_path(
            "It",
            "Auth.Tests.ps1",
            "powershell"
        ));
        assert!(!is_test_by_name_and_path(
            "Describe",
            "Auth.ps1",
            "powershell"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_bash() {
        assert!(is_test_by_name_and_path("run", "test/auth.bats", "bash"));
        assert!(is_test_by_name_and_path(
            "test_login",
            "test/auth_test.sh",
            "bash"
        ));
        assert!(!is_test_by_name_and_path("main", "src/auth.sh", "bash"));
    }

    #[test]
    fn test_is_test_by_name_and_path_dart() {
        assert!(is_test_by_name_and_path(
            "test",
            "test/auth_test.dart",
            "dart"
        ));
        assert!(is_test_by_name_and_path(
            "testWidgets",
            "test/widget_test.dart",
            "dart"
        ));
        assert!(!is_test_by_name_and_path("build", "lib/auth.dart", "dart"));
    }

    #[test]
    fn test_is_test_by_name_and_path_scala() {
        assert!(is_test_by_name_and_path(
            "test",
            "src/test/AuthSpec.scala",
            "scala"
        ));
        assert!(is_test_by_name_and_path(
            "it",
            "src/test/AuthTest.scala",
            "scala"
        ));
        assert!(!is_test_by_name_and_path(
            "handle",
            "src/main/Auth.scala",
            "scala"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_fortran() {
        assert!(is_test_by_name_and_path(
            "test_solve",
            "test/test_solver.f90",
            "fortran"
        ));
        assert!(!is_test_by_name_and_path(
            "solve",
            "src/solver.f90",
            "fortran"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_cpp() {
        assert!(is_test_by_name_and_path(
            "TEST",
            "test/auth_test.cpp",
            "cpp"
        ));
        assert!(is_test_by_name_and_path(
            "TEST_F",
            "test/auth_test.cc",
            "cpp"
        ));
        assert!(is_test_by_name_and_path(
            "TEST_CASE",
            "test/auth.cpp",
            "cpp"
        ));
        assert!(is_test_by_name_and_path("SCENARIO", "test/auth.cpp", "cpp"));
        assert!(is_test_by_name_and_path(
            "testAuth",
            "src/auth_test.cpp",
            "cpp"
        ));
        assert!(!is_test_by_name_and_path("main", "src/auth.cpp", "cpp"));
    }

    #[test]
    fn test_is_test_by_name_and_path_c() {
        assert!(is_test_by_name_and_path(
            "test_auth",
            "test/test_auth.c",
            "c"
        ));
        assert!(is_test_by_name_and_path(
            "test_parse",
            "src/parse_test.c",
            "c"
        ));
        assert!(!is_test_by_name_and_path("main", "src/auth.c", "c"));
    }

    #[test]
    fn test_is_test_by_name_and_path_cuda() {
        assert!(is_test_by_name_and_path(
            "TEST",
            "test/kernel_test.cu",
            "cuda"
        ));
        assert!(is_test_by_name_and_path("TEST_F", "test/kernel.cu", "cuda"));
        assert!(!is_test_by_name_and_path(
            "launch_kernel",
            "src/kernel.cu",
            "cuda"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_clojure() {
        assert!(is_test_by_name_and_path(
            "deftest",
            "test/auth_test.clj",
            "clojure"
        ));
        assert!(is_test_by_name_and_path(
            "my-test",
            "test/core_test.cljc",
            "clojure"
        ));
        assert!(!is_test_by_name_and_path(
            "handler",
            "src/auth.clj",
            "clojure"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_elm() {
        assert!(is_test_by_name_and_path(
            "describe",
            "tests/AuthTest.elm",
            "elm"
        ));
        assert!(is_test_by_name_and_path("test", "tests/Suite.elm", "elm"));
        assert!(!is_test_by_name_and_path("view", "src/Auth.elm", "elm"));
    }

    #[test]
    fn test_is_test_by_name_and_path_commonlisp() {
        assert!(is_test_by_name_and_path(
            "test-auth",
            "test/auth-test.lisp",
            "commonlisp"
        ));
        assert!(is_test_by_name_and_path(
            "def-test",
            "tests/suite.lisp",
            "commonlisp"
        ));
        assert!(!is_test_by_name_and_path(
            "handle",
            "src/auth.lisp",
            "commonlisp"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_pascal() {
        assert!(is_test_by_name_and_path(
            "TestLogin",
            "tests/AuthTest.pas",
            "pascal"
        ));
        assert!(is_test_by_name_and_path(
            "TestCreate",
            "test/UserTests.pas",
            "pascal"
        ));
        assert!(!is_test_by_name_and_path(
            "HandleClick",
            "src/Auth.pas",
            "pascal"
        ));
    }

    #[test]
    fn test_is_test_by_name_and_path_unknown_language() {
        assert!(!is_test_by_name_and_path(
            "test",
            "test/foo.xxx",
            "unknown_lang"
        ));
    }

    #[test]
    fn test_renamed_function_produces_same_sig_hash() {
        let grammar = tree_sitter_python::LANGUAGE.into();
        let src_old = b"def calculate_order_total(items: list) -> float:\n    return sum(i[\"price\"] * i[\"quantity\"] for i in items)\n";
        let src_new = b"def compute_order_sum(items: list) -> float:\n    return sum(i[\"price\"] * i[\"quantity\"] for i in items)\n";

        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_definition name: (identifier) @func.name) @func.def",
        )
        .unwrap();

        let tree_old = parser.parse(src_old, None).unwrap();
        let syms_old = extract_entities("calc.py", src_old, tree_old.root_node(), &query, "python");
        assert_eq!(syms_old.len(), 1);

        let tree_new = parser.parse(src_new, None).unwrap();
        let syms_new = extract_entities("calc.py", src_new, tree_new.root_node(), &query, "python");
        assert_eq!(syms_new.len(), 1);

        assert_eq!(
            syms_old[0].signature_hash, syms_new[0].signature_hash,
            "Renamed function with identical body should produce same sig_hash.\n  old: {} ({})\n  new: {} ({})",
            syms_old[0].name, syms_old[0].signature_hash,
            syms_new[0].name, syms_new[0].signature_hash,
        );
        assert_ne!(syms_old[0].name, syms_new[0].name);
    }

    #[test]
    fn test_different_body_produces_different_sig_hash() {
        let grammar = tree_sitter_python::LANGUAGE.into();
        let src_a = b"def foo(x):\n    return x + 1\n";
        let src_b = b"def foo(x):\n    return x * 2\n";

        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();

        let query = tree_sitter::Query::new(
            &grammar,
            "(function_definition name: (identifier) @func.name) @func.def",
        )
        .unwrap();

        let tree_a = parser.parse(src_a, None).unwrap();
        let syms_a = extract_entities("a.py", src_a, tree_a.root_node(), &query, "python");

        let tree_b = parser.parse(src_b, None).unwrap();
        let syms_b = extract_entities("a.py", src_b, tree_b.root_node(), &query, "python");

        assert_ne!(
            syms_a[0].signature_hash, syms_b[0].signature_hash,
            "Functions with different bodies should have different sig_hash"
        );
    }
}
