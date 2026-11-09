use tree_sitter::Node;

use crate::model::{Statement, StatementKind};

/// Cyclomatic complexity = 1 + number of decision points in the AST subtree.
///
/// Counts: if, else_if, for, while, do_while, loop, match/switch arms (case),
/// conditional expressions (?), logical AND (&&), logical OR (||), catch/except,
/// ternary (?:). Language-agnostic — matches node type strings from tree-sitter.
pub fn cyclomatic_complexity(node: Node) -> u32 {
    let mut count = 1u32;
    count_branches(node, &mut count);
    count
}

pub fn extract_statements<'a>(
    node: Node<'a>,
    source: &'a [u8],
    parent_symbol: &str,
    file: &str,
) -> Vec<Statement> {
    let mut stmts = Vec::new();
    let mut counter = 0u32;
    collect_statements(
        node,
        source,
        parent_symbol,
        file,
        0,
        &mut stmts,
        &mut counter,
    );
    stmts
}

fn node_condition_text<'a>(node: Node<'a>, source: &'a [u8], kind: &str) -> String {
    let cond = node
        .child_by_field_name("condition")
        .or_else(|| node.child_by_field_name("value"))
        .or_else(|| {
            if kind.starts_with("for") {
                node.child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("pattern"))
            } else {
                None
            }
        });
    match cond {
        Some(c) => {
            let text = c.utf8_text(source).unwrap_or("");
            truncate_condition(text)
        }
        None => String::new(),
    }
}

fn truncate_condition(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() > 120 {
        // Cut at a char boundary so a multi-byte character at byte 117 doesn't panic.
        let end = (0..=117)
            .rev()
            .find(|&i| cleaned.is_char_boundary(i))
            .unwrap_or(0);
        format!("{}...", &cleaned[..end])
    } else {
        cleaned
    }
}

#[allow(clippy::only_used_in_recursion)]
fn collect_statements<'a>(
    node: Node<'a>,
    source: &'a [u8],
    parent_symbol: &str,
    file: &str,
    depth: u32,
    stmts: &mut Vec<Statement>,
    counter: &mut u32,
) {
    let kind = node.kind();
    let stmt_kind = match kind {
        "if_expression" | "if_statement" => Some(StatementKind::If),
        "elif_clause" | "else_if_clause" => Some(StatementKind::ElseIf),
        "else_clause" => Some(StatementKind::Else),
        "for_statement" | "for_expression" | "for_in_statement" => Some(StatementKind::For),
        "while_statement" | "while_expression" => Some(StatementKind::While),
        "do_statement" => Some(StatementKind::DoWhile),
        "loop_expression" => Some(StatementKind::Loop),
        "match_expression" | "switch_expression" | "switch_statement" | "when_expression" => {
            Some(StatementKind::Match)
        }
        "match_arm" | "case_clause" | "switch_case" | "arm" | "when_clause" => {
            Some(StatementKind::Case)
        }
        "try_statement" | "try_expression" => Some(StatementKind::Try),
        "catch_clause" | "except_clause" | "rescue_clause" => Some(StatementKind::Catch),
        "ternary_expression" | "conditional_expression" => Some(StatementKind::Ternary),
        "return_statement" => {
            if depth == 1
                && node.start_position().row
                    < node
                        .parent()
                        .map(|p| p.end_position().row.saturating_sub(2))
                        .unwrap_or(0)
            {
                Some(StatementKind::Guard)
            } else {
                None
            }
        }
        _ => None,
    };

    let is_branch = stmt_kind.is_some();
    if let Some(sk) = stmt_kind {
        let condition = node_condition_text(node, source, kind);
        let id = format!("{}::stmt_{}", parent_symbol, counter);
        *counter += 1;
        stmts.push(Statement {
            id,
            kind: sk,
            condition,
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            depth,
            parent_symbol: parent_symbol.to_string(),
        });
    }

    let next_depth = if is_branch { depth + 1 } else { depth };
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            let ck = child.kind();
            if ck == "function_definition"
                || ck == "function_item"
                || ck == "function_declaration"
                || ck == "method_declaration"
                || ck == "method_definition"
                || ck == "closure_expression"
                || ck == "lambda_expression"
                || ck == "arrow_function"
                || ck == "anonymous_function_creation_expression"
            {
                continue;
            }
            collect_statements(
                child,
                source,
                parent_symbol,
                file,
                next_depth,
                stmts,
                counter,
            );
        }
    }
}

fn count_branches(node: Node, count: &mut u32) {
    let kind = node.kind();
    match kind {
        // Conditionals
        "if_expression" | "if_statement" | "elif_clause" | "else_if_clause" |
        "else_clause" | "when_clause" |
        // Loops
        "for_statement" | "for_expression" | "for_in_statement" |
        "while_statement" | "while_expression" |
        "do_statement" | "loop_expression" |
        // Pattern matching
        "match_arm" | "case_clause" | "switch_case" | "arm" |
        // Exception handling
        "catch_clause" | "except_clause" | "rescue_clause" |
        // Ternary / conditional expression
        "ternary_expression" | "conditional_expression" |
        // Logical short-circuit operators
        "binary_expression" => {
            // For binary_expression, only count && and ||
            if kind == "binary_expression" {
                let op = node.child_by_field_name("operator")
                    .map(|n| n.kind())
                    .unwrap_or("");
                if op == "&&" || op == "||" || op == "and" || op == "or" || op == "??" {
                    *count += 1;
                }
            } else {
                *count += 1;
            }
        }
        // Null coalescing / optional chaining count as branches
        "try_expression" | "propagation_expression" => *count += 1,
        _ => {}
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            count_branches(child, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_python(source: &str) -> Vec<Statement> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let root = tree.root_node();
        let fn_node = find_first_function(root).unwrap_or(root);
        extract_statements(fn_node, source.as_bytes(), "test::func", "test.py")
    }

    fn find_first_function(node: Node) -> Option<Node> {
        if node.kind() == "function_definition" {
            return Some(node);
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if let Some(found) = find_first_function(child) {
                    return Some(found);
                }
            }
        }
        None
    }

    #[test]
    fn test_python_if_else() {
        let source =
            "def check(x):\n    if x > 0:\n        return True\n    else:\n        return False\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"If"), "expected If, got {:?}", kinds);
        assert!(kinds.contains(&"Else"), "expected Else, got {:?}", kinds);
    }

    #[test]
    fn test_python_elif() {
        let source = "def classify(x):\n    if x > 100:\n        return 'high'\n    elif x > 50:\n        return 'medium'\n    else:\n        return 'low'\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"If"), "expected If, got {:?}", kinds);
        assert!(
            kinds.contains(&"ElseIf"),
            "expected ElseIf, got {:?}",
            kinds
        );
        assert!(kinds.contains(&"Else"), "expected Else, got {:?}", kinds);
    }

    #[test]
    fn test_python_for_while() {
        let source = "def process(items):\n    for item in items:\n        if item > 0:\n            print(item)\n    i = 0\n    while i < 10:\n        i += 1\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"For"), "expected For, got {:?}", kinds);
        assert!(kinds.contains(&"While"), "expected While, got {:?}", kinds);
        assert!(kinds.contains(&"If"), "expected If, got {:?}", kinds);
    }

    #[test]
    fn test_python_try_except() {
        let source = "def process():\n    try:\n        do_work()\n    except Exception as e:\n        handle_error(e)\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"Try"), "expected Try, got {:?}", kinds);
        assert!(kinds.contains(&"Catch"), "expected Catch, got {:?}", kinds);
    }

    #[test]
    fn test_python_ternary() {
        let source = "def pick(x):\n    return 'yes' if x > 0 else 'no'\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(
            kinds.contains(&"Ternary"),
            "expected Ternary, got {:?}",
            kinds
        );
    }

    #[test]
    fn test_depth_tracking() {
        let source = "def nested(x):\n    if x > 0:\n        if x > 10:\n            if x > 100:\n                do_thing()\n";
        let stmts = parse_python(source);
        let ifs: Vec<&Statement> = stmts
            .iter()
            .filter(|s| s.kind == StatementKind::If)
            .collect();
        assert_eq!(ifs.len(), 3, "expected 3 nested ifs, got {}", ifs.len());
        assert_eq!(ifs[0].depth, 0);
        assert_eq!(ifs[1].depth, 1);
        assert_eq!(ifs[2].depth, 2);
    }

    #[test]
    fn test_condition_text_extracted() {
        let source = "def check(x):\n    if x > 42:\n        print('big')\n";
        let stmts = parse_python(source);
        let if_stmt = stmts
            .iter()
            .find(|s| s.kind == StatementKind::If)
            .expect("expected If");
        assert!(
            if_stmt.condition.contains("x > 42"),
            "expected 'x > 42', got '{}'",
            if_stmt.condition
        );
    }

    #[test]
    fn test_no_statements_simple_function() {
        let source = "def add(a, b):\n    return a + b\n";
        let stmts = parse_python(source);
        assert!(
            stmts.is_empty(),
            "expected no statements, got {}",
            stmts.len()
        );
    }

    #[test]
    fn test_statement_ids_unique() {
        let source = "def multi(x):\n    if x > 0:\n        pass\n    if x > 1:\n        pass\n    for i in range(x):\n        pass\n";
        let stmts = parse_python(source);
        let ids: Vec<&str> = stmts.iter().map(|s| s.id.as_str()).collect();
        let unique: std::collections::HashSet<&str> = ids.iter().cloned().collect();
        assert_eq!(ids.len(), unique.len(), "IDs not unique: {:?}", ids);
    }

    #[test]
    fn test_python_guard_early_return() {
        let source = "def process(x):\n    if x is None:\n        return\n    do_work(x)\n    do_more(x)\n    return x\n";
        let stmts = parse_python(source);
        let kinds: Vec<&str> = stmts.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"If"), "expected If, got {:?}", kinds);
    }

    #[test]
    fn test_python_long_non_ascii_condition() {
        let source = format!(
            "def authorize(request):\n    if request.user == \"{}\":\n        return True\n    return False\n",
            "国".repeat(40)
        );
        let stmts = parse_python(&source);
        let cond = &stmts
            .iter()
            .find(|s| s.kind.as_str() == "If")
            .unwrap()
            .condition;
        assert!(
            cond.ends_with("..."),
            "expected a truncated condition, got {cond:?}"
        );
        assert!(cond.len() <= 120, "condition is {} bytes", cond.len());
    }

    #[test]
    fn test_truncate_condition_char_boundaries() {
        for prefix in ["", "a", "ab"] {
            for unit in ["国", "é", "😀"] {
                let text = format!("{prefix}{}", unit.repeat(70));
                let truncated = truncate_condition(&text);
                assert!(truncated.ends_with("..."));
                assert!(truncated.len() <= 120);
                assert!(text.starts_with(truncated.trim_end_matches("...")));
            }
        }
    }

    #[test]
    fn test_truncate_condition_ascii() {
        let long = "x".repeat(200);
        assert_eq!(truncate_condition(&long), format!("{}...", "x".repeat(117)));
        let short = "x".repeat(120);
        assert_eq!(truncate_condition(&short), short);
    }
}
