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
        let parsed = parse(text);
        assert!(parsed.errors().is_empty(), "{text}: {:?}", parsed.errors());
        top_level_kinds(text)
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

fn codes_of(text: &str) -> Vec<String> {
    parse(text)
        .errors()
        .iter()
        .map(|error| error.code.to_string())
        .collect()
}

#[test]
fn a_curly_quoted_argument_reports_only_the_rejected_characters() {
    assert_eq!(
        codes_of("Console.WriteLine(\u{201c}Hello\u{201d});\n"),
        ["MR0017", "MR0017"]
    );
}

#[test]
fn an_unclosed_string_reports_no_missing_semicolon() {
    assert_eq!(
        codes_of("let greeting = \"Hello;\nConsole.WriteLine(greeting);\n"),
        ["MR0012"]
    );
}

#[test]
fn a_stray_character_at_the_start_of_a_statement_reports_once() {
    assert_eq!(codes_of("\\\nlet a = 1;\n"), ["MR0017"]);
}

#[test]
fn a_missing_semicolon_without_a_lexer_error_still_reports() {
    assert_eq!(codes_of("let a = 1\nlet b = 2;\n"), ["MR0110"]);
}

#[test]
fn a_usable_token_with_a_lexer_error_still_gets_its_missing_semicolon() {
    assert_eq!(
        codes_of("let a = 1e400\nlet b = 2;\n"),
        ["MR0010", "MR0110"]
    );
    assert_eq!(codes_of("let a = 0xZ\nlet b = 2;\n"), ["MR0014", "MR0110"]);
    assert_eq!(
        codes_of("let a = \"a\\qb\"\nlet b = 2;\n"),
        ["MR0013", "MR0110"]
    );
}

#[test]
fn a_quoted_run_takes_its_brackets_whole() {
    assert_eq!(
        codes_of("F(\u{201c}G(a, b)\u{201d});\n"),
        ["MR0017", "MR0017"]
    );
    // A stray `)` is not part of the run and is reported.
    assert_eq!(
        codes_of("F(\u{201c}a\u{201d}));\n"),
        ["MR0017", "MR0017", "MR0110", "MR0110"]
    );
}

#[test]
fn a_rejected_run_stops_before_a_statement_keyword() {
    // The call is not closed, which is a mistake of its own, and the next
    // statement is parsed.
    assert_eq!(
        codes_of("F(\u{201c}x\nlet y = 1;\n"),
        ["MR0017", "MR0110", "MR0110"]
    );
}

#[test]
fn a_rejected_token_starts_an_error_expression_anywhere() {
    for text in [
        "let a = \"x\" + \u{201c}y\u{201d};\n",
        "if (\u{201c}a\u{201d}) { }\n",
        "F(\u{201c}a, b\u{201d});\n",
    ] {
        assert_eq!(codes_of(text), ["MR0017", "MR0017"], "{text:?}");
    }
    for text in [
        "let a = [\u{201c}a, b];\n",
        "let s = $\"{\u{201c}x}\";\n",
        "let a = \u{201c};\n",
    ] {
        assert_eq!(codes_of(text), ["MR0017"], "{text:?}");
    }
    // A missing `;` after the run is still reported.
    assert_eq!(
        codes_of("let a = \"x\" + \u{201c}y\u{201d}\nlet b = 2;\n"),
        ["MR0017", "MR0017", "MR0110"]
    );
    assert_eq!(
        codes_of("let a = \u{201c}\nlet b = 1;\n"),
        ["MR0017", "MR0110"]
    );
}

#[test]
fn a_rejected_token_after_an_operand_is_parsed_past() {
    // The parser goes on after the token as if it were not there: `2` after
    // `(1` needs the `)` that the call of a parenthesized expression
    // expects, and what follows is the rest of the statement.
    assert_rejected_over("let a = (1\u{201c}2);\n", "let a = (1 2);\n");
    // A missing `;` after it is still reported.
    assert_rejected_over("let a = 1 \\\nlet b = 2;\n", "let a = 1\nlet b = 2;\n");
}

/// Unclosed character literals, each with the same text with the literal
/// closed. The literal takes nothing after its first character, so every
/// other error is the one the closed text reports, and what follows the
/// literal is parsed as code.
const UNCLOSED_CHARACTERS: &[(&str, &str)] = &[
    ("let a = 'a;", "let a = 'a';"),
    ("Console.WriteLine('a);", "Console.WriteLine('a');"),
    ("let a = ('a);", "let a = ('a');"),
    ("let a = ['a, 2];", "let a = ['a', 2];"),
    ("let a = 1 + 'a, 2;", "let a = 1 + 'a', 2;"),
    ("let a = 'a}", "let a = 'a'}"),
    ("let a = 'a,", "let a = 'a',"),
    ("let a = ['a', 'b;", "let a = ['a', 'b';"),
    ("F('a;", "F('a';"),
    ("F('ab);", "F('a'b);"),
    ("let c = 'ab\nlet d = 1;\n", "let c = 'a'b\nlet d = 1;\n"),
    ("F('a$`, b);\n", "F('a'$`, b);\n"),
];

#[test]
fn an_unclosed_character_literal_reports_only_what_a_closed_one_would_more() {
    for (text, closed) in UNCLOSED_CHARACTERS {
        assert_eq!(parse(text).syntax().text(), *text, "{text:?}: not lossless");
        let expected: Vec<String> = std::iter::once("MR0012".to_string())
            .chain(codes_of(closed))
            .collect();
        assert_eq!(codes_of(text), expected, "{text:?}");
        assert_eq!(
            parse_messages(text)[1..],
            parse_messages(closed),
            "{text:?}"
        );
    }
    // As with a missing closer after any other operand.
    assert_eq!(codes_of("[1, 2;"), ["MR0110"]);
}

fn parse_messages(text: &str) -> Vec<String> {
    parse(text)
        .errors()
        .iter()
        .map(|error| error.message.clone())
        .collect()
}

#[test]
fn every_unclosed_character_literal_parses_in_a_bounded_number_of_steps() {
    // The parser fails once it emits more than a fixed number of events per
    // token, so a loop that stops taking tokens fails here instead of
    // hanging.
    for (text, _) in UNCLOSED_CHARACTERS {
        assert!(std::panic::catch_unwind(|| parse(text)).is_ok(), "{text:?}");
    }
}

#[test]
fn a_rejected_run_after_an_operand_keeps_the_operand() {
    for text in [
        "let a = nope \u{201c}y\u{201d};\n",
        "let a = nope(1, 2) \u{201c}y\u{201d};\n",
    ] {
        let tree = parse(text).syntax().debug_tree();
        assert!(tree.contains("NameRef"), "{text:?}:\n{tree}");
        assert_eq!(codes_of(text), ["MR0017", "MR0017"], "{text:?}");
    }
}

#[test]
fn a_rejected_token_ends_at_a_statement_on_the_next_line() {
    // The `;` is missing after the token; the statement on the next line is
    // one of its own.
    for text in [
        "let a = \u{201c}x\nnope(1);\nlet b = 2;\n",
        "let a = 1 \\\nb = 2;\n",
        "let a = 1 \\\n\nb = 2;\n",
    ] {
        let tree = parse(text).syntax().debug_tree();
        assert!(tree.contains("ExprStmt"), "{text:?}:\n{tree}");
        let codes = codes_of(text);
        assert_eq!(
            codes.iter().filter(|code| *code == "MR0110").count(),
            1,
            "{text:?}"
        );
    }
}

#[test]
fn a_quoted_run_ends_at_its_closing_quote() {
    // What follows the closing quote is parsed as if the quoted text were
    // not there: `+ + 1` is a unary `+` of a unary `+ 1`.
    let tree = parse("let a = \u{201c}x\u{201d} + + 1;\n")
        .syntax()
        .debug_tree();
    assert!(
        !tree.contains("BinExpr") && tree.matches("PrefixExpr").count() == 2,
        "{tree}"
    );
    assert_eq!(
        codes_of("let a = \u{201c}x\u{201d} + * 1;\n"),
        ["MR0017", "MR0017", "MR0110"]
    );
    // Brackets inside the quotes are text.
    for text in [
        "let a = \u{201c}see (note\u{201d};\nnope();\n",
        "F() {\n    let a = \u{201c}see (note\u{201d};\n    nope();\n}\nG() { nope2(); }\n",
        "let a = \u{201c}[x\u{201d};\n",
        "Console.WriteLine(\u{201c}Done :)\u{201d});\n",
        // So are line breaks, before the statement ends.
        "let a = \u{201c}hello\nworld\u{201d};\n",
        "F(\u{201c}hello\nworld\u{201d});\n",
        // Other quotes open a run too.
        "F(\u{201d}a, b\u{201d}, c);\n",
        "F(\u{201e}a, b\u{201c}, c);\n",
        "F(\u{201a}a, b\u{2018}, c);\n",
        "F(\u{2019}a, b\u{2019}, c);\n",
    ] {
        assert_eq!(codes_of(text), ["MR0017", "MR0017"], "{text:?}");
    }
    // An operand after a closing quote is the value, as if the quoted text
    // were not there: `let a = nope(1);`.
    assert_eq!(
        codes_of("let a = \u{201c}x\u{201d}\nnope(1);\nlet b = 2;\n"),
        ["MR0017", "MR0017"]
    );
    // Without its closing quote before the `;`, a quote is one token, and
    // what follows it is parsed.
    assert_eq!(
        codes_of("let a = \u{201c}x y\nnope(1);\n"),
        ["MR0017", "MR0110", "MR0110"]
    );
    assert_eq!(
        codes_of("let a = \u{201c}x\nlet b = \u{201c}y\u{201d};\n"),
        ["MR0017", "MR0110", "MR0017", "MR0017"]
    );
    // A `,` inside the quotes is part of the run.
    assert_eq!(
        codes_of("F(\u{201c}a, b\u{201d}, c);\n"),
        ["MR0017", "MR0017"]
    );
}

/// Each error as its code and where it starts.
fn coded_offsets(text: &str) -> Vec<(String, u32)> {
    parse(text)
        .errors()
        .iter()
        .map(|error| (error.code.to_string(), u32::from(error.range.start())))
        .collect()
}

/// The kind of each top-level node of the tree of `text`.
fn top_level_kinds(text: &str) -> Vec<String> {
    parse(text)
        .syntax()
        .children()
        .map(|n| format!("{:?}", n.kind()))
        .collect()
}

#[test]
fn a_rejected_token_keeps_the_declarations_after_it() {
    for text in [
        "let a = 1 \u{2018} (;\nMain(): int { return 1 }\nG(): int { let = ; }\n",
        "G(): int { let a = 1 \\ (; return 1; }\nMain(): int { return G(); }\n",
        "G(): int { let a = 1 \\ ( return 1; }\nMain(): int { return G(); }\n",
        "G(): int {\n    let a = 1 \\ [ x\n    return 1;\n}\nMain(): int { return G(); }\n",
        "A(n: int): int {\n    if \\ (n < 0) {\n        return -1;\n    }\n}\n\n\
         B(): int {\n    return 1;\n}\n\nC(): int {\n    return 2;\n}\n",
        "A(n: int): int {\n    while \\ (ready) {\n        return -1;\n    }\n}\n\n\
         B(): int {\n    return 1;\n}\n",
    ] {
        let kinds = top_level_kinds(text);
        let functions = kinds.iter().filter(|kind| *kind == "FunctionDecl").count();
        assert_eq!(
            functions,
            text.matches("): int").count(),
            "{text:?}: {kinds:?}"
        );
        assert!(codes_of(text).contains(&"MR0017".to_owned()), "{text:?}");
    }
    // Main's missing `;` and G's two errors are still reported.
    let codes =
        coded_offsets("let a = 1 \u{2018} (;\nMain(): int { return 1 }\nG(): int { let = ; }\n");
    for expected in [("MR0110", 39), ("MR0110", 56), ("MR0110", 58)] {
        assert!(
            codes.contains(&(expected.0.to_string(), expected.1)),
            "{expected:?} in {codes:?}"
        );
    }
    // A `{` after the token opens a block, which the `}`s that follow close:
    // left open, it holds the function after it, as a local function, up to
    // the line that starts with an access modifier.
    let text = "let a = \\ {\n    return 1;\nB(): int {\n    return 1;\n}\npublic struct S { }\n";
    assert_eq!(
        top_level_kinds(text),
        ["LocalDeclStmt", "Block", "StructDecl"]
    );
}

#[test]
fn a_rejected_token_is_parsed_past_as_if_it_were_not_there() {
    // Each reports the token, then what the text without it reports.
    for text in [
        "let x = a \\ F(() => { return 1; });\n",
        "let x = a \\ F(b,\n    c);\n",
        "let f = G(\\ x => {\n    let y = 1;\n    return y;\n});\n",
        "F(a \\ G(\n    b,\n    c\n));\n",
        "let x = a \\ new P {\n    A = 1,\n    B = 2\n};\n",
        "let a = 1 \\ + 2;\n",
    ] {
        assert_rejected_over(text, &text.replace("\\ ", ""));
    }
    // After an operand, `(x)` is a call, as without the token.
    for text in [
        "let f = a \\ (x) => {\n    return x;\n};\n",
        "let f = a \\ (x) => {\n    @Inline\n    let y = x;\n    return y;\n};\n",
    ] {
        assert_rejected_over(text, &text.replace("\\ ", ""));
    }
    // A rejected token between an extension's parameters and its body.
    assert_eq!(
        codes_of("extension(p: Point) \\ {\n    public X(): int => 1;\n}\n"),
        ["MR0017"]
    );
}

#[test]
fn a_missing_body_leaves_the_closing_brace() {
    for keyword in [
        "if (n < 0)",
        "while (n < 0)",
        "for (;;)",
        "foreach (let x in xs)",
        "do",
    ] {
        let text = format!("A(n: int): int {{\n    {keyword}\n}}\nB(): int {{ return 1; }}\n");
        assert_eq!(
            top_level_kinds(&text),
            ["FunctionDecl", "FunctionDecl"],
            "{text:?}"
        );
        assert_eq!(codes_of(&text), ["MR0110"], "{text:?}");
    }
    let text = "A(n: int): int {\n    if (n < 0) { } else\n}\nB(): int { return 1; }\n";
    assert_eq!(top_level_kinds(text), ["FunctionDecl", "FunctionDecl"]);
    assert_eq!(codes_of("if (true)"), ["MR0110"]);
    assert_eq!(codes_of("{ do }\nA();\n"), ["MR0110"]);
}

#[test]
fn rejected_tokens_in_a_row_are_one_error() {
    for text in [
        "let a = \u{a0}\u{a0}b + 1;\n",
        "let c = F(\u{a0}\u{a0}b);\n",
        "let a = \\\\x;\n",
        // A `..` after them is the range operator after an operand.
        "let r = (a.., \u{a0}\u{a0}..b);\n",
        "let r = (a.., \\..b);\n",
    ] {
        let rejected = text.matches(['\u{a0}', '\\']).count();
        assert_eq!(codes_of(text), vec!["MR0017"; rejected], "{text:?}");
    }
    let tree = parse("let c = F(\u{a0}\u{a0}b);\n").syntax().debug_tree();
    assert!(tree.contains("RParen"), "{tree}");
}

#[test]
fn a_rejected_token_where_a_body_starts_is_not_the_body() {
    for text in [
        "if (c) \\ { A(); } else { B(); }\n",
        "do \\ { A(); } while (c);\n",
    ] {
        assert_eq!(codes_of(text), ["MR0017"], "{text:?}");
        // The block, and the `else` or the `while`, are part of the statement.
        assert_eq!(top_level_kinds(text).len(), 1, "{text:?}");
    }
    // The block is the body of the `if`, and the `else` is its own.
    let tree = parse("if (c) \\ { A(); } else { B(); }\n")
        .syntax()
        .debug_tree();
    assert!(tree.contains("ElseClause"), "{tree}");
    // A missing body after the token is reported as it is without the
    // token; at the top level, the `}` is a stray one of its own.
    assert_rejected_over("if (c) A(); else \\ }\n", "if (c) A(); else }\n");
    let text = "F() {\n    if (c) A(); else \\ }\nG() { }\n";
    assert_eq!(
        coded_offsets(text),
        [("MR0110".to_owned(), 26), ("MR0017".to_owned(), 27)]
    );
    assert_eq!(top_level_kinds(text), ["FunctionDecl", "FunctionDecl"]);
}

#[test]
fn a_rejected_token_where_a_type_a_value_or_a_semicolon_starts_is_parsed_past() {
    for text in [
        "let a: \\ int = 1;\n",
        "F(a: \\ int) { }\n",
        "F(): int {\n    return \\ x;\n}\n",
        "let p = new \\ P();\n",
        "do { } while (false)\\;\n",
        "F() {\n    break \\;\n}\n",
        // Among modifiers and where a declaration or a member starts.
        "static \\ F() {}\n",
        "public \\ struct S {}\n",
        "struct S {\n    public \\ X: int;\n}\n",
        "F() {\n    static \\ G() {}\n}\n",
        "using \\ Std;\n",
        "enum E { \\ A, B }\n",
        "struct S {\n    public mut(private\\) Step: int = 1;\n}\n",
        "struct S {\n    public mut(\\private) Step: int = 1;\n}\n",
        "Max<T>(a: T): T where \\T : IComparable<T> => a;\n",
        "Max<T>(a: T): T where T \\: IComparable<T> => a;\n",
    ] {
        assert_only_rejected(text);
    }
}

/// Asserts that `text`, with its one `\`, reports only that token, and
/// that a run of rejected tokens in its place reports only them.
fn assert_only_rejected(text: &str) {
    assert_eq!(codes_of(text), ["MR0017"], "{text:?}");
    for run in ["\\\\", "\\\\\\", "\u{a0}\u{a0}"] {
        let longer = text.replacen('\\', run, 1);
        let rejected = run.chars().count();
        assert_eq!(codes_of(&longer), vec!["MR0017"; rejected], "{longer:?}");
    }
}

#[test]
fn a_do_without_a_body_reports_only_the_body() {
    assert_eq!(codes_of("do while (c);\n"), ["MR0110"]);
    let tree = parse("do while (c);\n").syntax().debug_tree();
    assert!(
        !tree
            .lines()
            .any(|line| line.trim_start().starts_with("WhileStmt@")),
        "{tree}"
    );
    // A `while` loop with an empty body is the body when the `do` has its
    // own `while` after it.
    assert!(codes_of("do while (c); while (d);\n").is_empty());
    assert!(codes_of("do while (c) A(); while (d);\n").is_empty());
    // A body without the `while` after it is not a missing body.
    assert_eq!(
        parse_messages("F() {\n    do A();\n}\n")
            .first()
            .map(String::as_str),
        Some("expected `while`")
    );
}

#[test]
fn a_write_scope_is_one_word_with_rejected_tokens_around_it() {
    let scope = |text: &str| parse(text).syntax().debug_tree().contains("WriteScope@");
    assert!(scope("struct S {\n    public mut(private\\) X: int;\n}\n"));
    assert!(!scope(
        "struct S {\n    public mut(private internal) X: int;\n}\n"
    ));
    // A run of rejected tokens in the scope is one error.
    let tree = parse("struct S {\n    public mut(\\\\private) X: int;\n}\n")
        .syntax()
        .debug_tree();
    assert_eq!(tree.matches("Error@").count(), 1, "{tree}");
}

#[test]
fn a_write_scope_without_its_word_reports_once() {
    // Rejected text stands for the word.
    assert_only_rejected("struct S {\n    public mut(\\) X: int;\n}\n");
    assert_eq!(
        parse_messages("struct S {\n    public mut() X: int;\n}\n"),
        ["expected `private`, `ns` or `internal`"]
    );
}

#[test]
fn a_using_without_its_name_leaves_the_declaration_on_the_next_line() {
    for text in [
        "using\nF() {}\n",
        "using \\\nF() {}\n",
        "using static\npublic struct S {}\n",
    ] {
        let kinds = top_level_kinds(text);
        assert_eq!(
            kinds.iter().filter(|kind| *kind != "Error").count(),
            2,
            "{text:?}"
        );
        let messages = parse_messages(text);
        assert!(
            messages.contains(&"expected an identifier".to_owned()),
            "{text:?}"
        );
        assert_eq!(messages.len(), 1 + text.matches('\\').count(), "{text:?}");
    }
    // A name on the next line that starts no declaration is the name.
    assert!(codes_of("using\n    Std.Text;\nF() {}\n").is_empty());
}

#[test]
fn rejected_text_around_a_do_without_a_body_reports_only_what_it_adds() {
    // Each reports the token and the missing body, as `do while (c);`
    // reports the body.
    for text in ["do \\ while (c);\n", "do while (c) \\;\n"] {
        let mut codes = codes_of(text);
        codes.sort();
        assert_eq!(codes, ["MR0017", "MR0110"], "{text:?}");
        assert!(
            parse_messages(text).contains(&"expected a statement".to_owned()),
            "{text:?}"
        );
        assert_eq!(top_level_kinds(text), ["DoWhileStmt"], "{text:?}");
    }
}

#[test]
fn rejected_text_in_an_enum_is_parsed_past() {
    for text in [
        "enum E { A, \\ }\n",
        "enum E { \\ }\n",
        "enum E { A \\, B }\n",
        "enum E { \\ @X A }\n",
        "enum E { A, B \\ }\n",
        "enum E { A = 1 \\, B }\n",
    ] {
        assert_only_rejected(text);
        assert_eq!(top_level_kinds(text), ["EnumDecl"], "{text:?}");
    }
}

#[test]
fn the_rest_of_an_unclosed_character_literal_is_parsed_as_code() {
    // The literal ends after `'"`, so the string after it runs to the end
    // of the line and takes the `}`; the function after it is still one.
    let text = "F(): Token {\n    return new Token { Text = '\"\" };\n}\n\n\
                file Check(token: Token): bool {\n    return true;\n}\n";
    assert_eq!(top_level_kinds(text), ["FunctionDecl", "FunctionDecl"]);
    // `Pair` is not part of the literal: the `;` is missing after it.
    assert_eq!(
        codes_of("x + 'Pair<int, string,>.Default;\n"),
        ["MR0012", "MR0110"]
    );
}

#[test]
fn a_cast_after_rejected_text_is_a_cast() {
    assert_only_rejected("let s = new int[n] + \\(any IShape)shape;\n");
    assert_only_rejected("let s = \\(int)x;\n");
}

#[test]
fn rejected_text_in_a_lambda_parameter_is_parsed_past() {
    for text in [
        "let f = (\\x: int) => x;\n",
        "let f = (a: int, \\b: int) => a;\n",
        "let f = (ref \\x: int) => x;\n",
    ] {
        assert_only_rejected(text);
    }
}

/// Asserts that `text` reports its rejected tokens and then what `without`,
/// the same text without its rejected text, reports.
fn assert_rejected_over(text: &str, without: &str) {
    let errors = parse(text).errors().to_vec();
    let rejected = errors
        .iter()
        .filter(|error| error.code.to_string() == "MR0017")
        .count();
    assert!(rejected > 0, "{text:?}");
    let rest: Vec<String> = errors
        .iter()
        .filter(|error| error.code.to_string() != "MR0017")
        .map(|error| error.message.clone())
        .collect();
    assert_eq!(rest, parse_messages(without), "{text:?}");
}

#[test]
fn rejected_text_before_the_while_of_a_do_is_parsed_past() {
    for text in [
        "do while (c); \\ while (d);\n",
        "do while (c) \\ ; \\ while (d);\n",
    ] {
        assert_rejected_over(text, &text.replace("\\ ", ""));
        assert!(
            parse_messages(text)
                .iter()
                .all(|message| message.contains("cannot start a token")),
            "{text:?}"
        );
    }
}

#[test]
fn a_lookahead_skips_a_quoted_run_as_the_parser_takes_it() {
    // The quoted text is one run, so the `do` has no `while (c);` after it.
    assert_rejected_over("do \u{201c} while (c) \u{201d};\n", "do ;\n");
    assert_rejected_over(
        "struct S {\n    public mut(\u{201c}a b\u{201d}) X: int;\n}\n",
        "struct S {\n    public mut(private) X: int;\n}\n",
    );
    assert_rejected_over(
        "F() {\n    static \u{201c}a b\u{201d} G() {}\n}\n",
        "F() {\n    static G() {}\n}\n",
    );
}

#[test]
fn rejected_text_in_every_slot_of_an_enum_member_and_a_lambda_parameter() {
    for text in [
        "enum E { A \\ = 1 }\n",
        "let f = (x \\: int) => x;\n",
        "let f = (x: int \\, y: int) => x;\n",
        "let f = (x: int = 1 \\) => x;\n",
        "let f = (x: int = 1 \\, y: int) => x;\n",
        "let f = (x: int, \\) => x;\n",
    ] {
        assert_only_rejected(text);
    }
    assert!(codes_of("let f = (x: int,) => x;\n").is_empty());
}

/// Small valid examples of each construct, for
/// `a_rejected_token_anywhere_in_valid_code_reports_only_itself`.
const VALID_EXAMPLES: &[&str] = &[
    "let a: List<int> = new List<int>();\n",
    "F(a: int, b: (int, string)): int? => a;\n",
    "let t: Dictionary<string, List<int>>? = null;\n",
    "F(): int {\n    return 1 + 2;\n}\n",
    "F() {\n    return;\n}\n",
    "let p = new P(1, 2);\n",
    "let q = new P { X = 1, Y = 2 };\n",
    "let xs = new int[4];\n",
    "F() {\n    let a = 1;\n    a = a + 1;\n    G(a);\n    break;\n}\n",
    "public static F(): int => 1;\n",
    "public struct S {\n    public static X: int = 1;\n    private Y: int;\n}\n",
    "internal async F() {}\n",
    concat!(
        "using Std.Json;\n",
        "using static Std.Math;\n",
        "using Json = Std.Json.JsonValue;\n",
        "global using Std.Text;\n",
        "F() {}\n",
    ),
    "enum E { A, B = 2, @X C }\n",
    "enum Color : byte { Red, Green = 1 }\n",
    "public Max<T>(a: T, b: T): T where T : IComparable<T> => a;\n",
    "public struct Pair<A, B> where A : IEquatable<A>, IDefault where B : allows ref struct {}\n",
    "struct S {\n    public mut(private) Step: int = 1;\n    public mut(ns) Version: int = 0;\n}\n",
    "F() {\n    do { G(); } while (c);\n}\n",
    "F() {\n    do G(); while (c);\n    while (c) G();\n}\n",
    "let f = (x: int, ref y: int, z: int = 1) => x;\n",
    "let g = (x) => x;\n",
    "let h = x => x + 1;\n",
    "let s = new int[n] + (any IShape)shape;\n",
    "let c = (int)x + (long)(y);\n",
    "F() {\n    static G() {}\n    async H() {}\n}\n",
    concat!(
        "F(xs: List<int>): int {\n",
        "    foreach (let x in xs) {\n",
        "        if (x > 0) { return x; } else { continue; }\n",
        "    }\n",
        "    for (var i = 0; i < 10; i++) { G(i); }\n",
        "    return match (xs.Count) { 0 => 1, _ => 2 };\n",
        "}\n",
    ),
    "let r = a ? b : c;\n",
    "let t = (1, \"a\");\n",
    "let ys = [1, 2, 3];\n",
    "let z = F<int>(x)[0].Name;\n",
    concat!(
        "@Json\n",
        "public class Point : IShape {\n",
        "    public X: int;\n",
        "    public Point(x: int) { X = x; }\n",
        "}\n",
    ),
    "union Shape { Circle(r: double), Square }\n",
    "interface IShape {\n    Area(): double;\n}\n",
];

#[test]
fn a_rejected_token_anywhere_in_valid_code_reports_only_itself() {
    for example in VALID_EXAMPLES {
        assert!(codes_of(example).is_empty(), "{example:?}");
        let ends = mora_syntax::lex(example)
            .tokens
            .iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| u32::from(token.range.start()) as usize)
            .chain([example.trim_end().len()])
            .collect::<Vec<_>>();
        for at in ends {
            let text = format!("{}\\{}", &example[..at], &example[at..]);
            assert_eq!(codes_of(&text), ["MR0017"], "{text:?}");
        }
    }
}
