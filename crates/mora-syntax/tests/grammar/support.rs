//! Helpers the grammar areas share.

use mora_syntax::{SyntaxKind, SyntaxNode, parse};

use crate::common::assert_errors_hold_tokens;

/// A node as nested node kinds, without tokens: `BinExpr(PathExpr, LiteralExpr)`.
/// Paths in expressions are written `P`.
fn render(node: &SyntaxNode) -> String {
    let kind = format!("{:?}", node.kind());
    if node.kind() == SyntaxKind::PathExpr {
        return "P".into();
    }
    let children: Vec<String> = node
        .children()
        .filter(|c| c.kind() != SyntaxKind::ArgList)
        .map(|c| render(&c))
        .collect();
    if children.is_empty() {
        kind
    } else {
        format!("{kind}({})", children.join(", "))
    }
}

/// The expression of `let _ = <text>;`, which must parse without errors.
pub fn shape(text: &str) -> String {
    let source = format!("let _ = {text};");
    let parse = parse(&source);
    assert!(parse.errors().is_empty(), "{text}: {:?}", parse.errors());
    let declarator = parse
        .syntax()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::VariableDeclarator)
        .unwrap();
    render(&declarator.children().nth(1).unwrap())
}

/// [`shape`], with a path that holds generic arguments written `P<>`.
pub fn shape_marking_generics(text: &str) -> String {
    fn render(node: &SyntaxNode) -> String {
        let kind = format!("{:?}", node.kind());
        if kind == "PathExpr" {
            let generic = node
                .descendants()
                .any(|n| n.kind() == SyntaxKind::GenericArgList);
            return if generic { "P<>".into() } else { "P".into() };
        }
        let children: Vec<String> = node
            .children()
            .filter(|c| !matches!(c.kind(), SyntaxKind::ArgList))
            .map(|c| render(&c))
            .collect();
        if children.is_empty() {
            kind
        } else {
            format!("{kind}({})", children.join(", "))
        }
    }
    let source = format!("let _ = {text};");
    let parse = parse(&source);
    assert!(parse.errors().is_empty(), "{text}: {:?}", parse.errors());
    let declarator = parse
        .syntax()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::VariableDeclarator)
        .unwrap();
    let expr = declarator.children().nth(1).unwrap();
    render(&expr)
}

/// The whole file as nested node kinds, and the messages of its errors.
pub fn outline(text: &str) -> (String, Vec<String>) {
    let parse = parse(text);
    assert_eq!(parse.syntax().text(), text, "not lossless");
    let errors = parse.errors().iter().map(|e| e.message.clone()).collect();
    (render(&parse.syntax()), errors)
}

/// The messages of the errors in `text`, in the order of the text. The tree
/// must hold all of `text`, and every `Error` node a token.
pub fn messages(text: &str) -> Vec<String> {
    assert_errors_hold_tokens(text);
    let parse = parse(text);
    assert_eq!(parse.syntax().text(), text, "not lossless");
    parse
        .errors()
        .iter()
        .map(|error| error.message.clone())
        .collect()
}

/// The kind of each top-level node of the tree of `text`.
pub fn top_level_kinds(text: &str) -> Vec<String> {
    parse(text)
        .syntax()
        .children()
        .map(|n| format!("{:?}", n.kind()))
        .collect()
}

/// The number of nodes of `kind` in the tree of `text`.
pub fn count(text: &str, kind: SyntaxKind) -> usize {
    parse(text)
        .syntax()
        .descendants()
        .filter(|n| n.kind() == kind)
        .count()
}

/// Whether a node of `kind` is in the tree of `text`.
pub fn has(text: &str, kind: SyntaxKind) -> bool {
    count(text, kind) > 0
}

/// Whether a token of `kind` is in the tree.
pub fn has_token(text: &str, kind: SyntaxKind) -> bool {
    parse(text)
        .syntax()
        .debug_tree()
        .contains(&format!("{kind:?}@"))
}

/// The first node of `kind`, which must be there. The parse is leaked, so
/// the node can outlive the call.
pub fn node(text: &str, kind: SyntaxKind) -> SyntaxNode<'static> {
    Box::leak(Box::new(parse(text)))
        .syntax()
        .descendants()
        .find(|n| n.kind() == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {text:?}"))
}

/// Parses without errors and holds a node of `kind`.
pub fn assert_clean_with(text: &str, kind: SyntaxKind) {
    assert_eq!(messages(text), Vec::<String>::new(), "{text:?}");
    assert!(has(text, kind), "no {kind:?} in {text:?}");
}
