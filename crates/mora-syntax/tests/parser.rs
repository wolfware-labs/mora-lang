// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Parser snapshot tests (M1.3). Each `tests/parser/ok/*.mr` and
//! `tests/parser/err/*.mr` file sits next to a `.rast` file holding its tree
//! and its errors. `ok` files must parse without errors. To update the
//! snapshots after a deliberate change, run
//! `MORA_BLESS=1 cargo test -p mora-syntax --test parser` and review the diff.

use std::fmt::Write;
use std::path::Path;

use mora_syntax::ast::{self, AstNode};
use mora_syntax::parse;

fn render(text: &str) -> String {
    let parse = parse(text);
    let mut out = parse.syntax().debug_tree();
    for error in parse.errors() {
        // Writing to a `String` cannot fail.
        let _ = writeln!(
            out,
            "error {} {:?}: {}",
            error.code, error.range, error.message
        );
    }
    out
}

#[test]
fn snapshots() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parser");
    let bless = std::env::var_os("MORA_BLESS").is_some();
    let mut failures = Vec::new();
    let mut count = 0;
    for group in ["ok", "err"] {
        let mut files: Vec<_> = std::fs::read_dir(dir.join(group))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "mr"))
            .collect();
        files.sort();
        for path in files {
            count += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let parse = parse(&text);
            assert_eq!(
                parse.syntax().text(),
                text,
                "{}: not lossless",
                path.display()
            );
            let has_errors = !parse.errors().is_empty();
            if (group == "ok") == has_errors {
                failures.push(format!(
                    "{}: {}",
                    path.display(),
                    if has_errors {
                        "unexpected errors"
                    } else {
                        "expected errors"
                    }
                ));
            }
            let rendered = render(&text);
            let snapshot = path.with_extension("rast");
            if bless {
                std::fs::write(&snapshot, &rendered).unwrap();
            } else if std::fs::read_to_string(&snapshot).ok().as_deref() != Some(rendered.as_str())
            {
                failures.push(format!(
                    "{}: snapshot differs; run MORA_BLESS=1 cargo test -p mora-syntax --test parser\n{rendered}",
                    path.display()
                ));
            }
        }
    }
    assert!(count >= 10, "found only {count} parser tests");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn class_members(text: &str) -> Vec<String> {
    let parsed = parse(text);
    let file = parsed.tree();
    let Some(ast::TopLevelItem::Item(ast::Item::ClassDecl(class))) = file.top_level_items().next()
    else {
        panic!("no class in {text}");
    };
    class
        .member_list()
        .unwrap()
        .members()
        .map(|member| {
            let syntax = member.syntax();
            let name = syntax
                .descendants()
                .find(|n| n.kind() == mora_syntax::SyntaxKind::Name)
                .map(|n| n.text())
                .unwrap_or_default();
            format!("{:?} {name}", syntax.kind())
        })
        .collect()
}

#[test]
fn a_missing_semicolon_keeps_the_next_members() {
    let text = "class A { X: int Y: int; M() { } }";
    assert_eq!(
        class_members(text),
        ["FieldDecl X", "FieldDecl Y", "FunctionDecl M"]
    );
}

#[test]
fn a_missing_brace_keeps_the_next_members() {
    let text = "class B {\n public First() {\n Work();\n public Second() { }\n}";
    assert_eq!(
        class_members(text),
        ["FunctionDecl First", "FunctionDecl Second"]
    );
}

#[test]
fn garbage_between_members_is_skipped() {
    let text = "class C { X: int; ) ) ; Y: int; }";
    assert_eq!(class_members(text), ["FieldDecl X", "FieldDecl Y"]);
}

#[test]
fn every_spec_example_parses_without_panicking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let spec = std::fs::read_to_string(root.join("docs/design/mora.md")).unwrap();
    let mut block: Option<String> = None;
    let mut count = 0;
    for line in spec.lines() {
        match &mut block {
            None if line.trim_start() == "```mora" => block = Some(String::new()),
            Some(text) if line.trim_start() == "```" => {
                assert_eq!(parse(text).syntax().text(), *text);
                count += 1;
                block = None;
            }
            Some(text) => {
                text.push_str(line);
                text.push('\n');
            }
            None => {}
        }
    }
    assert!(count >= 20, "{count}");
}

/// The expression of `let _ = <text>;` as nested node kinds, without
/// tokens: `BinExpr(PathExpr, LiteralExpr)`. Paths are written `P`.
fn shape(text: &str) -> String {
    fn render(node: &mora_syntax::SyntaxNode) -> String {
        let kind = format!("{:?}", node.kind());
        if kind == "PathExpr" {
            let generic = node
                .descendants()
                .any(|n| n.kind() == mora_syntax::SyntaxKind::GenericArgList);
            return if generic { "P<>".into() } else { "P".into() };
        }
        let children: Vec<String> = node
            .children()
            .filter(|c| !matches!(c.kind(), mora_syntax::SyntaxKind::ArgList))
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
        .find(|n| n.kind() == mora_syntax::SyntaxKind::VariableDeclarator)
        .unwrap();
    let expr = declarator.children().nth(1).unwrap();
    render(&expr)
}

#[test]
fn question_marks_follow_section_7_4() {
    assert_eq!(shape("a ? b : c"), "ConditionalExpr(P, P, P)");
    assert_eq!(shape("x?"), "PropagateExpr(P)");
    assert_eq!(shape("x?.y"), "MemberExpr(P, NameRef)");
    assert_eq!(shape("await f()?"), "PropagateExpr(AwaitExpr(CallExpr(P)))");
    assert_eq!(shape("await f()"), "AwaitExpr(CallExpr(P))");
    assert_eq!(shape("await -x"), "AwaitExpr(PrefixExpr(P))");
    assert_eq!(shape("await !ready"), "AwaitExpr(PrefixExpr(P))");
    assert_eq!(
        shape("await (int)value"),
        "AwaitExpr(CastExpr(PredefinedType, P))"
    );
    assert_eq!(
        shape("await -f()?"),
        "PropagateExpr(AwaitExpr(PrefixExpr(CallExpr(P))))"
    );
    assert_eq!(
        shape("stackalloc byte[256]"),
        "StackallocExpr(PredefinedType, LiteralExpr)"
    );
    assert_eq!(
        shape("(r?).Name"),
        "MemberExpr(ParenExpr(PropagateExpr(P)), NameRef)"
    );
    assert_eq!(
        shape("a ? b : c ? d : e"),
        "ConditionalExpr(P, P, ConditionalExpr(P, P, P))"
    );
    assert_eq!(
        shape("ok ? x = 1 : y = 2"),
        "ConditionalExpr(P, AssignExpr(P, LiteralExpr), AssignExpr(P, LiteralExpr))"
    );
    assert_eq!(
        shape("x = ok ? 1 : 2"),
        "AssignExpr(P, ConditionalExpr(P, LiteralExpr, LiteralExpr))"
    );
    assert_eq!(shape("x? ?? y"), "BinExpr(PropagateExpr(P), P)");
    assert_eq!(
        shape("c ? a?[0] : b"),
        "ConditionalExpr(P, IndexExpr(P, Arg(LiteralExpr)), P)"
    );
    assert_eq!(
        shape("c ? r? : d"),
        "ConditionalExpr(P, PropagateExpr(P), P)"
    );
    assert_eq!(shape("f(x?, y)"), "CallExpr(P)");
    assert_eq!(
        shape("flag ? [1] : []"),
        "ConditionalExpr(P, CollectionExpr(CollectionElem(LiteralExpr)), CollectionExpr)"
    );
}

#[test]
fn generic_arguments_follow_section_14_4() {
    assert_eq!(shape("F<A, B>(c)"), "CallExpr(P<>)");
    assert_eq!(
        shape("(F < A, B > c)"),
        "TupleExpr(TupleElem(BinExpr(P, P)), TupleElem(BinExpr(P, P)))"
    );
    assert_eq!(shape("a < b"), "BinExpr(P, P)");
    assert_eq!(shape("List<int>.Empty"), "MemberExpr(P<>, NameRef)");
}

#[test]
fn casts_need_an_operand_after_the_parentheses() {
    assert_eq!(
        shape("(int)x"),
        "CastExpr(PredefinedType(), P)".replace("()", "")
    );
    assert_eq!(shape("(int)-x"), "CastExpr(PredefinedType, PrefixExpr(P))");
    assert_eq!(
        shape("(T)x"),
        "CastExpr(PathType(Path(PathSegment(NameRef))), P)"
    );
    assert_eq!(shape("(a) - b"), "BinExpr(ParenExpr(P), P)");
    assert_eq!(shape("(a)"), "ParenExpr(P)");
}

#[test]
fn precedence_follows_section_7_1() {
    assert_eq!(shape("a + b * c"), "BinExpr(P, BinExpr(P, P))");
    assert_eq!(shape("a * b + c"), "BinExpr(BinExpr(P, P), P)");
    assert_eq!(shape("-a * b"), "BinExpr(PrefixExpr(P), P)");
    assert_eq!(shape("a..b"), "RangeExpr(P, P)");
    assert_eq!(shape("a + b..c"), "BinExpr(P, RangeExpr(P, P))");
    assert_eq!(
        shape("a * match (b) { _ => 0 }"),
        "BinExpr(P, MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, LiteralExpr))))"
    );
    assert_eq!(
        shape("a == b is int"),
        "BinExpr(P, IsExpr(P, TypePattern(PredefinedType)))"
    );
    assert_eq!(
        shape("a & b ^ c | d"),
        "BinExpr(BinExpr(BinExpr(P, P), P), P)"
    );
    assert_eq!(shape("a || b && c"), "BinExpr(P, BinExpr(P, P))");
    assert_eq!(shape("a ?? b ?? c"), "BinExpr(P, BinExpr(P, P))");
    assert_eq!(
        shape("a ?? b ? c : d"),
        "ConditionalExpr(BinExpr(P, P), P, P)"
    );
    assert_eq!(shape("x = y = z"), "AssignExpr(P, AssignExpr(P, P))");
    assert_eq!(shape("x >>= 1"), "AssignExpr(P, LiteralExpr)");
    assert_eq!(shape("a >> b > c"), "BinExpr(BinExpr(P, P), P)");
}

#[test]
fn patterns_follow_section_7_8() {
    let pattern = |text: &str| {
        let s = shape(&format!("x is {text}"));
        s.strip_prefix("IsExpr(P, ")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap()
            .to_owned()
    };
    assert_eq!(pattern("0"), "ConstantPattern(LiteralExpr)");
    assert_eq!(
        pattern("Color.Red"),
        "ConstantPattern(MemberExpr(P, NameRef))"
    );
    assert_eq!(
        pattern("not none"),
        "NotPattern(ConstantPattern(LiteralExpr))"
    );
    assert_eq!(
        pattern("User u"),
        "DeclarationPattern(PathType(Path(PathSegment(NameRef))), NameDesignation(Name))"
    );
    assert_eq!(pattern("let v"), "BindingPattern(NameDesignation(Name))");
    assert_eq!(pattern("_"), "DiscardPattern");
    assert_eq!(pattern("< 0"), "RelationalPattern(LiteralExpr)");
    assert_eq!(
        pattern("> 0 and < 10 or 20"),
        "BinPattern(BinPattern(RelationalPattern(LiteralExpr), RelationalPattern(LiteralExpr)), ConstantPattern(LiteralExpr))"
    );
    assert_eq!(
        pattern("Circle(let r)"),
        "CasePattern(Path(PathSegment(NameRef)), PositionalPattern(PatternField(BindingPattern(NameDesignation(Name)))))"
    );
    assert_eq!(
        pattern("(0, _)"),
        "PositionalPattern(PatternField(ConstantPattern(LiteralExpr)), PatternField(DiscardPattern))"
    );
    assert_eq!(
        pattern("{ Age: > 18 }"),
        "PropertyPattern(PatternField(NameRef, RelationalPattern(LiteralExpr)))"
    );
    assert_eq!(
        pattern("[1, .. let rest]"),
        "ListPattern(ConstantPattern(LiteralExpr), SlicePattern(BindingPattern(NameDesignation(Name))))"
    );
    assert_eq!(
        pattern("(> 0)"),
        "ParenPattern(RelationalPattern(LiteralExpr))"
    );
    assert_eq!(pattern("int[]"), "TypePattern(ArrayType(PredefinedType))");
    // Parentheses hold a positional pattern, here of type patterns.
    assert_eq!(
        pattern("(int, string)"),
        "PositionalPattern(PatternField(TypePattern(PredefinedType)), PatternField(TypePattern(PredefinedType)))"
    );
}

#[test]
fn match_arm_names_are_not_lambdas() {
    assert_eq!(
        shape("match (s) { None => 0, ok => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(ConstantPattern(P), LiteralExpr), MatchArm(ConstantPattern(P), LiteralExpr)))"
    );
    assert_eq!(
        shape("match (n) { _ when ok => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, WhenClause(P), LiteralExpr)))"
    );
    assert_eq!(
        shape("match (n) { _ when xs.Any(x => x > 0) => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, WhenClause(CallExpr(MemberExpr(P, NameRef))), LiteralExpr)))"
    );
}

/// Every program the spec and the error pages run (`// expect:` first)
/// parses without errors: the examples are the language as documented.
#[test]
fn every_runnable_example_parses_cleanly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut docs = vec![root.join("docs/design/mora.md")];
    for entry in std::fs::read_dir(root.join("docs/errors")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name != "README.md") {
            docs.push(path);
        }
    }
    let mut checked = 0;
    let mut failures = Vec::new();
    for doc in docs {
        let text = std::fs::read_to_string(&doc).unwrap();
        let mut block: Option<String> = None;
        for line in text.lines() {
            match &mut block {
                None if line.trim_start() == "```mora" => block = Some(String::new()),
                Some(example) if line.trim_start() == "```" => {
                    if example.starts_with("// expect:") {
                        checked += 1;
                        let errors: Vec<_> = parse(example)
                            .errors()
                            .iter()
                            .map(|e| e.message.clone())
                            .collect();
                        if !errors.is_empty() {
                            failures.push(format!("{}: {errors:?}\n{example}", doc.display()));
                        }
                    }
                    block = None;
                }
                Some(example) => {
                    example.push_str(line);
                    example.push('\n');
                }
                None => {}
            }
        }
    }
    assert!(checked >= 10, "{checked}");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn nothing_chains_after_propagation() {
    assert_eq!(
        shape("items?[flag ? 0 : 1]"),
        "IndexExpr(P, Arg(ConditionalExpr(P, LiteralExpr, LiteralExpr)))"
    );
    assert_eq!(
        shape("(T)stackalloc byte[1]"),
        "CastExpr(PathType(Path(PathSegment(NameRef))), StackallocExpr(PredefinedType, LiteralExpr))"
    );
    let errors: Vec<_> = parse("let _ = r? .Name;")
        .errors()
        .iter()
        .map(|e| e.message.clone())
        .collect();
    // `r?` ends the expression; `.Name` is then left over.
    assert_eq!(errors.first().map(String::as_str), Some("expected `;`"));
    // A leading `[` is always a collection; `@` starts an attribute.
    let stmts = |text: &str| -> Vec<String> {
        let parse = parse(text);
        assert!(parse.errors().is_empty(), "{text}: {:?}", parse.errors());
        parse
            .syntax()
            .children()
            .map(|n| format!("{:?}", n.kind()))
            .collect()
    };
    assert_eq!(stmts("[x][0];"), ["ExprStmt"]);
    assert_eq!(stmts("[x](arg);"), ["ExprStmt"]);
    assert_eq!(stmts("[a] is [_];"), ["ExprStmt"]);
    assert_eq!(stmts("[a] < b;"), ["ExprStmt"]);
    assert_eq!(stmts("[a] with { };"), ["ExprStmt"]);
    assert_eq!(stmts("[Inline];"), ["ExprStmt"]);
    assert_eq!(stmts("@Cfg(Debug = true)\nLog(1);"), ["AttributedStmt"]);
    assert_eq!(stmts("@Inline { }"), ["AttributedStmt"]);
    assert_eq!(
        stmts("@Cfg(Debug = true)\nlet x: int = 1;"),
        ["AttributedStmt"]
    );
    assert_eq!(
        stmts("@Inline\nTwice(x: int): int => x * 2;"),
        ["FunctionDecl"]
    );
    assert_eq!(stmts("@A\n@B\nstatic X: int;"), ["FieldDecl"]);
}
