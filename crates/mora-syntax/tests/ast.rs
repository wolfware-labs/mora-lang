// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Typed views over hand-built trees (M1.2). The parser arrives in M1.3, so
//! these trees are built token by token with `TreeBuilder`.

use mora_syntax::ast::{self, AstNode};
use mora_syntax::{SyntaxKind as K, SyntaxNode, TreeBuilder, lex};

/// Builds a tree from nested `(kind, children)` and `(kind, text)` items.
enum Tree {
    Node(K, Vec<Tree>),
    Token(K, &'static str),
}

use Tree::{Node, Token};

/// The tree is leaked, so the views of a test can borrow it freely.
fn build(tree: &Tree) -> SyntaxNode<'static> {
    fn add(builder: &mut TreeBuilder, tree: &Tree) {
        match tree {
            Node(kind, children) => {
                builder.start_node(*kind);
                for child in children {
                    add(builder, child);
                }
                builder.finish_node();
            }
            Token(kind, text) => builder.token(*kind, text),
        }
    }
    let mut builder = TreeBuilder::new();
    add(&mut builder, tree);
    Box::leak(Box::new(builder.finish())).root()
}

fn name_ref(text: &'static str) -> Tree {
    Node(
        K::PathExpr,
        vec![Node(
            K::Path,
            vec![Node(
                K::PathSegment,
                vec![Node(K::NameRef, vec![Token(K::Ident, text)])],
            )],
        )],
    )
}

/// `struct Point { X: int; }`
fn point() -> Tree {
    Node(
        K::SourceFile,
        vec![Node(
            K::StructDecl,
            vec![
                Token(K::StructKw, "struct"),
                Token(K::Whitespace, " "),
                Node(K::Name, vec![Token(K::Ident, "Point")]),
                Token(K::Whitespace, " "),
                Node(
                    K::MemberList,
                    vec![
                        Token(K::LBrace, "{"),
                        Token(K::Whitespace, " "),
                        Node(
                            K::FieldDecl,
                            vec![
                                Node(
                                    K::VariableDeclarator,
                                    vec![
                                        Node(K::Name, vec![Token(K::Ident, "X")]),
                                        Token(K::Colon, ":"),
                                        Token(K::Whitespace, " "),
                                        Node(K::PredefinedType, vec![Token(K::IntKw, "int")]),
                                    ],
                                ),
                                Token(K::Semicolon, ";"),
                            ],
                        ),
                        Token(K::Whitespace, " "),
                        Token(K::RBrace, "}"),
                    ],
                ),
            ],
        )],
    )
}

#[test]
fn a_struct_declaration_navigates() {
    let root = build(&point());
    assert_eq!(root.text(), "struct Point { X: int; }");
    let file = ast::SourceFile::cast(root).unwrap();
    let items: Vec<_> = file.top_level_items().collect();
    let [ast::TopLevelItem::Item(ast::Item::StructDecl(point))] = items.as_slice() else {
        panic!("{items:?}");
    };
    assert_eq!(point.struct_token().unwrap().text(), "struct");
    assert_eq!(point.name().unwrap().ident_token().unwrap().text(), "Point");
    let members: Vec<_> = point.member_list().unwrap().members().collect();
    let [ast::Member::FieldDecl(field)] = members.as_slice() else {
        panic!("{members:?}");
    };
    let declarator = field.variable_declarator().unwrap();
    assert_eq!(declarator.name().unwrap().syntax().text(), "X");
    assert_eq!(declarator.colon_token().unwrap().text(), ":");
    let Some(ast::Type::PredefinedType(int)) = declarator.ty() else {
        panic!("{:?}", declarator.ty());
    };
    assert_eq!(int.kind().unwrap().kind(), K::IntKw);
    assert!(declarator.init().is_none());
    assert_eq!(field.semicolon_token().unwrap().text(), ";");
}

#[test]
fn the_debug_tree_shows_every_token() {
    let root = build(&point());
    let expected = r#"SourceFile@0..24
  StructDecl@0..24
    StructKw@0..6 "struct"
    Whitespace@6..7 " "
    Name@7..12
      Ident@7..12 "Point"
    Whitespace@12..13 " "
    MemberList@13..24
      LBrace@13..14 "{"
      Whitespace@14..15 " "
      FieldDecl@15..22
        VariableDeclarator@15..21
          Name@15..16
            Ident@15..16 "X"
          Colon@16..17 ":"
          Whitespace@17..18 " "
          PredefinedType@18..21
            IntKw@18..21 "int"
        Semicolon@21..22 ";"
      Whitespace@22..23 " "
      RBrace@23..24 "}"
"#;
    assert_eq!(root.debug_tree(), expected);
}

#[test]
fn casting_checks_the_kind() {
    let root = build(&point());
    let struct_node = root.children().next().unwrap();
    assert!(ast::SourceFile::cast(struct_node).is_none());
    assert!(ast::StructDecl::cast(struct_node).is_some());
    assert!(ast::Item::cast(struct_node).is_some());
    assert!(ast::Member::cast(struct_node).is_some());
    assert!(ast::Expr::cast(struct_node).is_none());
}

#[test]
fn same_type_children_are_found_by_their_tokens() {
    // `..y`: no start, so `y` is the end.
    let range = build(&Node(
        K::RangeExpr,
        vec![Token(K::DotDot, ".."), name_ref("y")],
    ));
    let range = ast::RangeExpr::cast(range).unwrap();
    assert!(range.start().is_none());
    assert_eq!(range.end().unwrap().syntax().text(), "y");

    // `c ? : e`, with the middle operand missing.
    let conditional = build(&Node(
        K::ConditionalExpr,
        vec![
            name_ref("c"),
            Token(K::Question, "?"),
            Token(K::Colon, ":"),
            name_ref("e"),
        ],
    ));
    let conditional = ast::ConditionalExpr::cast(conditional).unwrap();
    assert_eq!(conditional.condition().unwrap().syntax().text(), "c");
    assert!(conditional.then().is_none());
    assert_eq!(conditional.r#else().unwrap().syntax().text(), "e");

    // `a >> b`, with `>>` joined from two `>` by the parser.
    let shift = build(&Node(
        K::BinExpr,
        vec![name_ref("a"), Token(K::Shr, ">>"), name_ref("b")],
    ));
    let shift = ast::BinExpr::cast(shift).unwrap();
    assert_eq!(shift.op().unwrap().kind(), K::Shr);
    assert_eq!(shift.lhs().unwrap().syntax().text(), "a");
    assert_eq!(shift.rhs().unwrap().syntax().text(), "b");
}

#[test]
fn tokens_walk_the_whole_text() {
    let root = build(&point());
    let text: String = root.tokens().map(|t| t.text().to_owned()).collect();
    assert_eq!(text, root.text());
    let x = root.tokens().find(|t| t.text() == "X").unwrap();
    assert_eq!(u32::from(x.text_range().start()), 15);
}

#[test]
fn syntax_kinds() {
    for (i, &kind) in K::ALL.iter().enumerate() {
        assert_eq!(K::from_raw(u16::try_from(i).unwrap()), Some(kind));
    }
    assert_eq!(K::from_raw(u16::try_from(K::ALL.len()).unwrap()), None);
    assert_eq!(K::keyword("class"), Some(K::ClassKw));
    assert_eq!(K::keyword("get"), None);
    assert_eq!(K::contextual_keyword("get"), Some(K::GetKw));
    assert_eq!(K::contextual_keyword("_"), Some(K::Underscore));
    assert_eq!(K::Shr.text(), Some(">>"));
    assert!(K::StructDecl.is_node() && K::Error.is_node());
    assert!(!K::Ident.is_node() && !K::Underscore.is_node());
    assert!(K::DocComment.is_trivia() && !K::Ident.is_trivia());
}

#[test]
fn the_lexer_never_produces_joined_or_contextual_kinds() {
    let text = "a >> b >= c ?[ get value _ x >>>= y";
    for token in lex(text).tokens {
        assert!(!K::JOINED.contains(&token.kind), "{token:?}");
        assert!(!token.kind.is_contextual_keyword(), "{token:?}");
        assert!(!token.kind.is_node(), "{token:?}");
    }
}

#[test]
fn nodes_tokens_and_elements() {
    let root = build(&point());
    assert_eq!(root.kind(), K::SourceFile);
    assert_eq!(format!("{root:?}"), "SourceFile@0..24");
    assert_eq!(root.to_string(), "struct Point { X: int; }");

    let struct_node = root.children().next().unwrap();
    assert_ne!(struct_node, root);
    assert_eq!(root.children().next().unwrap(), struct_node);
    assert_eq!(u32::from(struct_node.text_range().len()), 24);
    assert_eq!(struct_node.first_token().unwrap().text(), "struct");
    assert_eq!(struct_node.last_token().unwrap().text(), "}");
    let kinds: Vec<_> = struct_node.descendants().map(|n| n.kind()).collect();
    assert_eq!(kinds[..3], [K::StructDecl, K::Name, K::MemberList]);
    assert_eq!(kinds.len(), 7);

    let elements: Vec<_> = struct_node.children_with_tokens().collect();
    assert_eq!(elements.len(), 5);
    assert_eq!(elements[0].kind(), K::StructKw);
    assert_eq!(elements[2].kind(), K::Name);
    assert_eq!(u32::from(elements[2].text_range().start()), 7);
    let token = elements[0].into_token().unwrap();
    assert!(elements[0].into_node().is_none());
    assert_eq!(format!("{token:?}"), "StructKw@0..6 \"struct\"");
    assert_eq!(token.to_string(), "struct");
    let name = elements[2].into_node().unwrap();
    assert!(elements[2].into_token().is_none());
    assert_eq!(name.kind(), K::Name);
}

#[test]
fn checkpoints_wrap_what_came_before() {
    // `a + b`: the operand is added before the parser knows it is the left
    // side of a binary expression, then wrapped from a checkpoint.
    let mut builder = TreeBuilder::new();
    builder.start_node(K::ExprStmt);
    let checkpoint = builder.checkpoint();
    builder.token(K::Ident, "a");
    builder.start_node_at(checkpoint, K::BinExpr);
    builder.token(K::Plus, "+");
    builder.token(K::Ident, "b");
    builder.finish_node();
    builder.token(K::Semicolon, ";");
    builder.finish_node();
    let green = builder.finish();
    assert_eq!(u32::from(green.text_len()), 4);
    let root = green.root();
    assert_eq!(root.text(), "a+b;");
    let bin = root.children().next().unwrap();
    assert_eq!(bin.kind(), K::BinExpr);
    assert_eq!(bin.text(), "a+b");
}

#[test]
fn a_token_after_a_child_node_is_found() {
    let parse = mora_syntax::parse("let x = a + b;\n");
    let bin = parse
        .syntax()
        .descendants()
        .find_map(ast::BinExpr::cast)
        .unwrap();
    assert_eq!(bin.op().unwrap().kind(), K::Plus);
    assert_eq!(bin.rhs().unwrap().syntax().text(), "b");
}

#[test]
fn an_empty_child_node_is_found() {
    // The parser recovers from the missing element with an empty node.
    let parse = mora_syntax::parse("let x = [, c];\n");
    let list = parse
        .syntax()
        .descendants()
        .find_map(ast::CollectionExpr::cast)
        .unwrap();
    let first = list.collection_elem().unwrap();
    assert!(first.syntax().text_range().is_empty());
    assert!(first.expr().is_none());
}

#[test]
fn a_function_type_tells_its_result_from_its_parameters() {
    let parse = mora_syntax::parse("let f: (int) => string = g;\n");
    let fn_type = parse
        .syntax()
        .descendants()
        .find_map(ast::FnType::cast)
        .unwrap();
    assert_eq!(fn_type.params().unwrap().syntax().text(), "(int)");
    assert_eq!(fn_type.ret().unwrap().syntax().text(), "string");
}

#[test]
fn equal_text_at_two_places_is_two_nodes() {
    // The two `a` share one green node; their offsets tell them apart.
    let parse = mora_syntax::parse("let x = a + a;\n");
    let paths: Vec<_> = parse
        .syntax()
        .descendants()
        .filter(|node| node.kind() == K::PathExpr)
        .collect();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].text(), paths[1].text());
    assert_ne!(paths[0], paths[1]);
    let again = parse
        .syntax()
        .descendants()
        .find(|node| node.kind() == K::PathExpr)
        .unwrap();
    assert_eq!(again, paths[0]);
    let set: std::collections::HashSet<_> = paths.iter().copied().chain([again]).collect();
    assert_eq!(set.len(), 2);
    let tokens: std::collections::HashSet<_> = parse
        .syntax()
        .tokens()
        .filter(|token| token.text() == "a")
        .collect();
    assert_eq!(tokens.len(), 2);
}

#[test]
fn children_read_from_the_back_have_the_same_ranges() {
    let parse = mora_syntax::parse("struct P { X: int; Y: string; }\n");
    for node in parse.syntax().descendants() {
        let forward: Vec<_> = node
            .children_with_tokens()
            .map(|c| c.text_range())
            .collect();
        let mut backward: Vec<_> = node
            .children_with_tokens()
            .rev()
            .map(|c| c.text_range())
            .collect();
        backward.reverse();
        assert_eq!(forward, backward, "{node:?}");
    }
    let root = parse.syntax();
    assert_eq!(root.last_token().unwrap().text(), "\n");
    let decl = root.children().next().unwrap();
    assert_eq!(decl.last_token().unwrap().text(), "}");
    assert_eq!(decl.text(), "struct P { X: int; Y: string; }");
}
