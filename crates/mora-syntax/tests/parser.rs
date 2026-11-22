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
        .map(|member| kind_and_name(member.syntax()))
        .collect()
}

/// The kind of `node` and the first name in it.
fn kind_and_name(node: &mora_syntax::SyntaxNode) -> String {
    let name = node
        .descendants()
        .find(|n| n.kind() == mora_syntax::SyntaxKind::Name)
        .map(|n| n.text())
        .unwrap_or_default();
    format!("{:?} {name}", node.kind())
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
fn a_quoted_run_stops_at_the_end_of_its_line() {
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
        assert_parsed_past(text);
    }
    // After an operand, `(x)` is a call, as without the token.
    for text in [
        "let f = a \\ (x) => {\n    return x;\n};\n",
        "let f = a \\ (x) => {\n    @Inline\n    let y = x;\n    return y;\n};\n",
    ] {
        assert_parsed_past(text);
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
    assert_errors_hold_tokens(text);
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
    assert_eq!(rest_of_errors(text), parse_messages(without), "{text:?}");
}

/// [`assert_rejected_over`] with the text without each `\` and the space
/// after it.
fn assert_parsed_past(text: &str) {
    assert_rejected_over(text, &text.replace("\\ ", ""));
}

#[test]
fn rejected_text_before_the_while_of_a_do_is_parsed_past() {
    for text in [
        "do while (c); \\ while (d);\n",
        "do while (c) \\ ; \\ while (d);\n",
    ] {
        assert_parsed_past(text);
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

/// Each declaration in the tree of `text`, members and local functions
/// included, as its kind and name.
fn declarations(text: &str) -> Vec<String> {
    parse(text)
        .syntax()
        .descendants()
        .filter(|node| format!("{:?}", node.kind()).ends_with("Decl"))
        .map(|node| kind_and_name(&node))
        .collect()
}

#[test]
fn an_unpaired_quote_keeps_the_declarations_after_it() {
    for text in [
        "class A {\n    F() { \u{201c} }\n    G() { \u{201d} }\n}\n",
        "enum E { A\u{201c}, B }\nclass C { X: int\u{201d}; }\n",
        "@A \u{201d}@B\npublic class C {\n    \u{201d}P: int;\n}\n",
        "class A {\n    F(\u{201c} a: int) { }\n    G(\u{201d}) { }\n}\n",
        // The matching quote one or more declarations later.
        "@A(\u{201c}x)\nclass C { }\nclass D { X: int\u{201d}; }\n",
        "F(\u{201c}a: int) { }\nG(b: int) { }\nH(c: int\u{201d}) { }\n",
        "enum E { A\u{201c}, B }\nenum F { C }\nenum G { D\u{201d}, E }\n",
    ] {
        let without = text.replace(['\u{201c}', '\u{201d}'], "");
        assert_eq!(declarations(text), declarations(&without), "{text:?}");
        assert_rejected_over(text, &without);
    }
}

#[test]
fn rejected_text_inside_a_joined_operator_is_parsed_past() {
    let kinds = |text: &str| -> Vec<String> {
        let tree = parse(text).syntax().debug_tree();
        tree.lines()
            .map(|line| line.split('@').next().unwrap_or_default().to_owned())
            .collect()
    };
    for text in [
        "let a = items?\\[0];\n",
        "let a = items?\\.Name;\n",
        "let a = x >\\> 2;\n",
        "x >\\>= 2;\n",
        "let r = a.\\.b;\n",
    ] {
        assert_only_rejected(text);
        // The same nodes and tokens, the joined one with the run inside it.
        assert_eq!(kinds(text), kinds(&text.replace('\\', "")), "{text:?}");
    }
    // Whitespace, a line break or a comment inside the run, a quoted one
    // too, keeps the tokens around it apart, as it does without the
    // rejected characters. Where the operand after `>` is missing, the run
    // stands for it, so only the rejected characters are reported there.
    for (text, rest) in [
        ("let r = a.\\ \\.b;\n", &["expected an identifier"][..]),
        ("let r = a.\\\n\\.b;\n", &["expected an identifier"]),
        ("let r = a.\\/* c */\\.b;\n", &["expected an identifier"]),
        ("x >\\ \\= 2;\n", &[]),
        ("let a = x >\\ \\> 2;\n", &[]),
        ("let a = x >\\\n\\> 2;\n", &[]),
        // A quoted run with a space inside it.
        (
            "let r = a.\u{201c} \u{201d}.b;\n",
            &["expected an identifier"],
        ),
    ] {
        let without = text.replace(['\\', '\u{201c}', '\u{201d}'], "");
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(&without),
            "{text:?}"
        );
        assert_eq!(rest_of_errors(text), rest, "{text:?}");
    }
}

/// The messages of the errors of `text` besides `MR0017`, which it must
/// report.
fn rest_of_errors(text: &str) -> Vec<String> {
    assert_errors_hold_tokens(text);
    let errors = parse(text).errors().to_vec();
    assert!(
        errors
            .iter()
            .any(|error| error.code.to_string() == "MR0017"),
        "{text:?}"
    );
    errors
        .iter()
        .filter(|error| error.code.to_string() != "MR0017")
        .map(|error| error.message.clone())
        .collect()
}

/// The nodes and tokens of the tree of `text`, nested, leaving out trivia
/// and the `Error` nodes that hold only rejected text.
fn shape_past_rejected(text: &str) -> String {
    use mora_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
    fn rejected_only(node: &SyntaxNode) -> bool {
        node.children_with_tokens().all(|child| match child {
            SyntaxElement::Token(token) => {
                token.kind().is_trivia() || token.kind() == SyntaxKind::Unknown
            }
            SyntaxElement::Node(node) => rejected_only(&node),
        })
    }
    fn walk(node: &SyntaxNode, out: &mut String) {
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(token) if !token.kind().is_trivia() => {
                    let _ = write!(out, "{:?} ", token.kind());
                }
                SyntaxElement::Node(node)
                    if !(node.kind() == SyntaxKind::Error && rejected_only(&node)) =>
                {
                    let _ = write!(out, "{:?}( ", node.kind());
                    walk(&node, out);
                    out.push_str(") ");
                }
                _ => {}
            }
        }
    }
    assert_errors_hold_tokens(text);
    let mut out = String::new();
    walk(&parse(text).syntax(), &mut out);
    out
}

#[test]
fn rejected_text_never_makes_a_comment() {
    // `/` and `/` with rejected text between them stay two operators, and
    // the run stands for the operand missing between them.
    for (text, rest) in [
        ("let a = x /\\/ 2;\n", &[][..]),
        ("let a = x /\\/= 2;\n", &[]),
        ("let a = x /\\// 2;\n", &["expected `;`"]),
    ] {
        let spaced = text.replace('\\', " ");
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(&spaced),
            "{text:?}"
        );
        assert_eq!(rest_of_errors(text), rest, "{text:?}");
    }
}

#[test]
fn an_error_on_a_joined_token_covers_the_rejected_text_inside_it() {
    let text = "F(x: int) -\\> int { return x; }\n";
    let errors = parse(text).errors().to_vec();
    let error = errors
        .iter()
        .find(|error| error.code.to_string() == "MR0110")
        .unwrap();
    assert_eq!(error.message, "expected `=>`");
    assert_eq!(
        (u32::from(error.range.start()), u32::from(error.range.end())),
        (10, 13)
    );
    let fixed = error.fix.as_ref().unwrap().apply(text);
    assert_eq!(fixed, "F(x: int) => int { return x; }\n");
}

#[test]
fn a_blank_line_inside_rejected_text_detaches_a_doc_comment() {
    use mora_syntax::SyntaxKind;
    for (text, kind) in [
        ("/// Doc.\n\\\n\n\\\nclass A { }\n", "SourceFile"),
        ("/// Doc.\n\\\n\\\nclass A { }\n", "SourceFile"),
        ("/// Doc.\n\\ \n \\\nclass A { }\n", "SourceFile"),
        ("/// Doc.\n\\\nclass A { }\n", "SourceFile"),
        (
            "class A {\n    /// Doc.\n    \\\n    F() { }\n}\n",
            "MemberList",
        ),
        // A run on the comment's own line, the declaration on the next.
        ("\\/// Doc.\nclass A { }\n", "ClassDecl"),
        ("/// Doc.\n\\class A { }\n", "ClassDecl"),
        (
            "class A {\n    /// Doc.\n    \\F() { }\n}\n",
            "FunctionDecl",
        ),
    ] {
        let without = text.replace('\\', "");
        assert_eq!(comment_holders(text), comment_holders(&without), "{text:?}");
        assert_eq!(
            comment_holders(text),
            [("/// Doc.".to_owned(), kind.to_owned())],
            "{text:?}"
        );
    }
    // A block comment with rejected text after it on its line.
    let text = "/* Doc. */ \\\nclass A { }\n";
    let parse = parse(text);
    let class = parse
        .syntax()
        .descendants()
        .find(|node| node.kind() == SyntaxKind::ClassDecl);
    assert!(class.unwrap().text().starts_with("/* Doc. */"), "{text:?}");
}

#[test]
fn a_block_comment_over_lines_ends_the_search_for_a_closing_quote() {
    let text = "let a = \u{201c}x /*\n */ y\u{201d};\n";
    // Each quote is rejected text on its own: `x` and `y` are still read.
    let names = parse(text)
        .syntax()
        .debug_tree()
        .matches("NameRef@")
        .count();
    assert_eq!(names, 2);
    assert_rejected_over(text, "let a = x /*\n */ y;\n");
}

#[test]
fn a_quote_that_starts_a_line_looks_for_its_closing_quote_on_that_line() {
    // The first quote's scan stops at the line break, where the second
    // quote starts its own scan and finds its closing quote.
    let text = "F(\u{201c}a,\n\u{201c}b\u{201d});\n";
    assert_rejected_over(text, "F(a,\n);\n");
    // `b` is in the quoted run, not an argument.
    let names = |text: &str| {
        parse(text)
            .syntax()
            .debug_tree()
            .matches("NameRef@")
            .count()
    };
    assert_eq!(names(text), names("F(a,\n);\n"));
}

#[test]
fn a_quoted_run_never_splits_an_interpolated_string() {
    for text in [
        "Console.WriteLine\u{2018}($\"{name\u{2019}}!\");\n",
        "let s = $\"{a\u{201c}} and {b\u{201d}}\";\n",
    ] {
        assert_eq!(codes_of(text), ["MR0017", "MR0017"], "{text:?}");
    }
}

/// Each comment of `text` with the kind of the node that holds it, past the
/// `Error` nodes of rejected text around it.
fn comment_holders(text: &str) -> Vec<(String, String)> {
    use mora_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
    fn walk(node: &SyntaxNode, holder: SyntaxKind, out: &mut Vec<(String, String)>) {
        let holder = if node.kind() == SyntaxKind::Error {
            holder
        } else {
            node.kind()
        };
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(token) if token.kind().is_comment() => {
                    out.push((token.text().to_owned(), format!("{holder:?}")));
                }
                SyntaxElement::Node(child) => walk(&child, holder, out),
                SyntaxElement::Token(_) => {}
            }
        }
    }
    assert_errors_hold_tokens(text);
    let mut out = Vec::new();
    let parse = parse(text);
    let root = parse.syntax();
    walk(&root, root.kind(), &mut out);
    out
}

/// The `Error` nodes in `node`, `node` included.
fn error_nodes<'a>(
    node: &mora_syntax::SyntaxNode<'a>,
) -> impl Iterator<Item = mora_syntax::SyntaxNode<'a>> {
    node.descendants()
        .filter(|node| node.kind() == mora_syntax::SyntaxKind::Error)
}

/// The text of each `Error` node in `node`.
fn error_texts(node: &mora_syntax::SyntaxNode) -> Vec<String> {
    error_nodes(node)
        .map(|node| node.text().to_string())
        .collect()
}

/// Asserts that every `Error` node in the tree of `text` holds a token that
/// is not trivia: a comment between parts of rejected text is trivia, as
/// it would be without them, rejected text taken in place of something
/// missing is in the node that stands for it, and no `Error` node holds no
/// token at all, as one past the nesting limit that takes nothing would.
fn assert_errors_hold_tokens(text: &str) {
    for node in error_nodes(&parse(text).syntax()) {
        assert!(
            node.tokens().any(|token| !token.kind().is_trivia()),
            "{text:?}: {node:?} holds no token but trivia"
        );
    }
}

#[test]
fn comments_in_rejected_text_attach_as_they_would_without_it() {
    for (text, without) in [
        // A blank line inside a run: only the comment below it attaches.
        (
            "\\/// Doc.\n\n/* c */\\class A { }\n",
            "/// Doc.\n\n/* c */class A { }\n",
        ),
        (
            "\\/// Doc.\n\\\n/* c */\\class A { }\n",
            "/// Doc.\n\n/* c */class A { }\n",
        ),
        (
            " \n\u{201c} \u{201d}/// Doc.// c\n\n/*\nc */\u{201c}class A { }\n",
            " \n/// Doc.// c\n\n/*\nc */class A { }\n",
        ),
        // No blank line: every comment attaches, the run with them.
        (
            "/// Doc.\n\\ /* c */ \\\nclass A { }\n",
            "/// Doc.\n /* c */ \nclass A { }\n",
        ),
        ("\\ /* c */ \\\nclass A { }\n", " /* c */ \nclass A { }\n"),
    ] {
        assert_eq!(comment_holders(text), comment_holders(without), "{text:?}");
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(without),
            "{text:?}"
        );
        assert_rejected_over(text, without);
    }
    // A comment starts a part of the run: the rejected text before it, on
    // its line or the lines above, stays out of the class.
    let (text, errors) = class_of("\\ /* c */ \\\nclass A { }\n");
    assert!(text.starts_with("/* c */"), "{text:?}");
    assert_eq!(errors, ["/* c */ \\"]);
    let (text, errors) = class_of("\\\n/* c */\n\\class A { }\n");
    assert!(text.starts_with("/* c */"), "{text:?}");
    assert_eq!(errors, ["/* c */\n\\"]);
    assert_eq!(
        error_texts(&parse("\\\n/* c */\n\\class A { }\n").syntax()).len(),
        2
    );
    // Below a blank line, the part starts at its comment, past the
    // indentation, as the comment would without the run.
    let (text, errors) = class_of("\\/// Doc.\n\n  /* c */\\class A { }\n");
    assert!(text.starts_with("/* c */"), "{text:?}");
    assert_eq!(errors, ["/* c */\\"]);
}

#[test]
fn a_block_comment_on_one_line_keeps_a_quoted_run_whole() {
    let text = "let a = 1 \u{201c}x /* k */ y\u{201d};\n";
    assert_eq!(codes_of(text), ["MR0017", "MR0017"]);
    assert_rejected_over(text, "let a = 1;\n");
    // One run, in one `Error` node.
    assert_eq!(
        error_texts(&parse(text).syntax()),
        ["\u{201c}x /* k */ y\u{201d}"]
    );
    // Standing for a missing operand, the run is all that is reported.
    let text = "let a = \u{201c}x /* k */ y\u{201d};\n";
    assert_eq!(codes_of(text), ["MR0017", "MR0017"]);
}

/// Each error as its code, its message and its range.
fn full_errors(text: &str) -> Vec<(String, String, u32, u32)> {
    parse(text)
        .errors()
        .iter()
        .map(|error| {
            (
                error.code.to_string(),
                error.message.clone(),
                u32::from(error.range.start()),
                u32::from(error.range.end()),
            )
        })
        .collect()
}

#[test]
fn something_missing_is_reported_before_the_rejected_text() {
    let text = "class A \\x { }\n";
    // Right after `A`, as in the text without the `\`.
    assert_eq!(
        full_errors(text)
            .into_iter()
            .find(|(code, ..)| code == "MR0110"),
        Some(("MR0110".to_owned(), "expected `{`".to_owned(), 7, 7))
    );
    assert_rejected_over(text, "class A x { }\n");
    assert_eq!(coded_offsets("class A x { }\n")[0].1, 7);
}

#[test]
fn only_the_part_before_a_blank_line_stands_for_what_is_missing() {
    let text = "let a = \\\n\n\\\nlet b = 1;\n";
    // The `;` is missing right after the part that stands for the value.
    assert_eq!(
        coded_offsets(text),
        [
            ("MR0017".to_owned(), 8),
            ("MR0110".to_owned(), 9),
            ("MR0017".to_owned(), 11)
        ]
    );
    // The part below the blank line is an `Error` node of its own.
    assert_eq!(error_texts(&parse(text).syntax()), ["\\", "\\"]);
}

/// The text of the class declared in `text`, and of each `Error` node
/// inside it.
fn class_of(text: &str) -> (String, Vec<String>) {
    let parse = parse(text);
    let class = parse
        .syntax()
        .descendants()
        .find(|node| node.kind() == mora_syntax::SyntaxKind::ClassDecl)
        .unwrap();
    (class.text().to_string(), error_texts(&class))
}

#[test]
fn rejected_text_in_place_leaves_the_comments_below_it_to_the_declaration() {
    for (text, without, rejected_at) in [
        (
            "let a = \\\n\n/// Doc.\n\\class C { }\n",
            "let a = \n\n/// Doc.\nclass C { }\n",
            20,
        ),
        (
            "let a = \\\n/// Doc.\n\\class C { }\n",
            "let a = \n/// Doc.\nclass C { }\n",
            19,
        ),
        // On the line of the rejected text that stands for the value.
        (
            "let a = \\ /// Doc.\n\\class C { }\n",
            "let a =  /// Doc.\nclass C { }\n",
            19,
        ),
    ] {
        assert_eq!(
            comment_holders(text),
            [("/// Doc.".to_owned(), "ClassDecl".to_owned())],
            "{text:?}"
        );
        assert_eq!(comment_holders(text), comment_holders(without), "{text:?}");
        // The first `\` stands for the value; the `;` is missing after it.
        assert_eq!(
            coded_offsets(text),
            [
                ("MR0017".to_owned(), 8),
                ("MR0110".to_owned(), 9),
                ("MR0017".to_owned(), rejected_at)
            ],
            "{text:?}"
        );
    }
    // Not in place: the line of rejected text above the comment stays out
    // of the class, as a blank line would.
    let text = "x;\n\\\n/// Doc.\n\\class C { }\n";
    assert_eq!(
        comment_holders(text),
        [("/// Doc.".to_owned(), "ClassDecl".to_owned())]
    );
    let (class, errors) = class_of(text);
    assert!(class.starts_with("/// Doc."), "{class:?}");
    assert_eq!(errors, ["/// Doc.\n\\"]);
    assert_rejected_over(text, "x;\n\n/// Doc.\nclass C { }\n");
}

#[test]
fn rejected_text_after_a_comment_before_a_declaration_does_not_stand_for_what_is_missing() {
    // The comment, and the run under it, go to the declaration after them,
    // as the comment would without the run; what is missing is reported as
    // it would be.
    for (text, without) in [
        (
            "let a = /// Doc.\n\\class C { }\n",
            "let a = /// Doc.\nclass C { }\n",
        ),
        (
            "let a = /* c */ \\class C { }\n",
            "let a = /* c */ class C { }\n",
        ),
        (
            "let a = /* c */ \\\n@A G(): int => 1;\n",
            "let a = /* c */ \n@A G(): int => 1;\n",
        ),
        (
            "let a = /* c */ \\\nusing X;\n",
            "let a = /* c */ \nusing X;\n",
        ),
        // `operator` starts a declaration only in a member list.
        (
            "class K {\n    G(): int => /* c */ \\\n    operator +(a: K): K => a;\n}\n",
            "class K {\n    G(): int => /* c */\n    operator +(a: K): K => a;\n}\n",
        ),
    ] {
        assert_eq!(comment_holders(text), comment_holders(without), "{text:?}");
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(without),
            "{text:?}"
        );
        assert_rejected_over(text, without);
    }
    let (class, errors) = class_of("let a = /// Doc.\n\\class C { }\n");
    assert!(class.starts_with("/// Doc."), "{class:?}");
    assert_eq!(errors, ["\\"]);
}

#[test]
fn rejected_text_after_a_comment_stands_for_what_is_missing_where_no_declaration_follows() {
    // No declaration comes after the run to take the comment, so the run
    // stands for the missing operand, and the text reports what it would
    // without the comment. The comment stays outside the run's `Error`
    // node.
    for (text, comment) in [
        ("let s = /* note */ \u{201c}text\u{201d};\n", "/* note */"),
        ("let a = 1 + /* c */ \\;\n", "/* c */"),
        ("let a = /* c */\n\\;\n", "/* c */"),
        ("(/* c */ \\> 0\\\n\n\\)\n", "/* c */"),
        ("let u = await // c\n\\\n", "// c"),
        ("struct W { public mut(/* c */ \\) X: int; }\n", "/* c */"),
    ] {
        let blank = text.replace(comment, &" ".repeat(comment.len()));
        assert_eq!(full_errors(text), full_errors(&blank), "{text:?}");
        assert_errors_hold_tokens(text);
        // The first token after the comment starts an `Error` node that
        // the comment is not in.
        let parse = parse(text);
        let root = parse.syntax();
        let tokens: Vec<_> = root.tokens().collect();
        let at = tokens
            .iter()
            .position(|token| token.text() == comment)
            .unwrap();
        let comment = tokens[at].text_range();
        let next = tokens[at + 1..]
            .iter()
            .find(|token| !token.kind().is_trivia())
            .unwrap()
            .text_range();
        let first_of = |node: &mora_syntax::SyntaxNode| {
            node.tokens()
                .find(|token| !token.kind().is_trivia())
                .map(|token| token.text_range())
        };
        let errors: Vec<_> = error_nodes(&root).collect();
        assert!(
            errors
                .iter()
                .all(|node| !node.text_range().contains_range(comment)),
            "{text:?}"
        );
        assert!(
            errors.iter().any(|node| first_of(node) == Some(next)),
            "{text:?}"
        );
    }
}

#[test]
fn rejected_text_in_place_goes_inside_the_nodes_opened_around_it() {
    // An argument or an element opens before the `Error` node that stands
    // for its value: the rejected text goes in that `Error` node, and what
    // is missing after it is reported after it.
    for (text, node, missing) in [
        ("F(\\: int;\n", "Arg", 3),
        ("G(a, \\: b);\n", "Arg", 6),
        ("let x = [\\: 1];\n", "CollectionElem", 10),
        ("F(\u{201d})\u{201d}: int;\n", "Arg", 9),
    ] {
        assert_errors_hold_tokens(text);
        let parse = parse(text);
        let holder = parse
            .syntax()
            .descendants()
            .find(|n| format!("{:?}", n.kind()) == node && n.text().contains(['\\', '\u{201d}']))
            .unwrap();
        assert_eq!(error_texts(&holder), [holder.text()], "{text:?}");
        assert_eq!(
            coded_offsets(text)
                .into_iter()
                .find(|(code, _)| code == "MR0110"),
            Some(("MR0110".to_owned(), missing)),
            "{text:?}"
        );
    }
}

#[test]
fn rejected_text_a_node_placed_does_not_stand_for_what_is_missing_after_it() {
    // The parameter, whose name is missing, opens after the `\\`, which
    // goes before it; the body missing after the parameter list is then
    // reported, as without the `\\`.
    let text = "let f = (\\const x: int) => x;\n";
    assert_rejected_over(text, "let f = (const x: int) => x;\n");
}

/// The kind of the node that holds the `Error` node of the rejected text
/// `run` in the tree of `text`.
fn holder_of_run(text: &str, run: &str) -> String {
    let parse = parse(text);
    let holder = parse
        .syntax()
        .descendants()
        .find(|node| {
            node.children()
                .any(|child| child.kind() == mora_syntax::SyntaxKind::Error && child.text() == run)
        })
        .unwrap();
    format!("{:?}", holder.kind())
}

#[test]
fn rejected_text_after_a_token_or_a_completed_node_still_stands_for_what_is_missing() {
    // An error right after a token (`if`) or a completed node (the name
    // `X`), where no node was opened at the run, leaves the run to the
    // operand after it: only what the run does not stand for is reported.
    let error = |message: &str, at: u32| ("MR0110".to_owned(), message.to_owned(), at, at);
    let rejected = |at: u32| {
        (
            "MR0017".to_owned(),
            "`\\\\` cannot start a token".to_owned(),
            at,
            at + 1,
        )
    };
    for (text, errors, holder) in [
        (
            "if \\ { }\n",
            vec![
                error("expected `(`", 2),
                rejected(3),
                error("expected `)`", 4),
            ],
            "IfStmt",
        ),
        (
            "let p = new P { X \\ };\n",
            vec![error("expected `=`", 17), rejected(18)],
            "MemberInit",
        ),
    ] {
        assert_eq!(full_errors(text), errors, "{text:?}");
        assert_eq!(holder_of_run(text, "\\"), holder, "{text:?}");
    }
}

#[test]
fn comments_after_rejected_text_in_place_go_to_the_declaration_below() {
    // The first part stands for the argument, as an argument written there
    // would; the comment after it then goes to the enum, as it does after
    // `G(x`. Without the run it stays in the open argument list.
    let text = "G(\\/* z */\\\nenum E { A }\n";
    assert_eq!(holder_of_run(text, "\\"), "Arg");
    assert_eq!(
        comment_holders(text),
        [("/* z */".to_owned(), "EnumDecl".to_owned())]
    );
    assert_eq!(
        comment_holders("G(x/* z */\nenum E { A }\n"),
        comment_holders(text)
    );
    assert_eq!(
        comment_holders("G(/* z */\nenum E { A }\n"),
        [("/* z */".to_owned(), "ArgList".to_owned())]
    );
}

#[test]
fn comments_between_parts_of_rejected_text_are_trivia() {
    for (text, without) in [
        (
            "x;\n\\ /* a */ /* b */ \\\nclass C { }\n",
            "x;\n /* a */ /* b */ \nclass C { }\n",
        ),
        ("\\\n\n/* a */\n\n\\\n", "\n\n/* a */\n\n\n"),
    ] {
        assert_eq!(comment_holders(text), comment_holders(without), "{text:?}");
        assert_rejected_over(text, without);
    }
    // In place, the first part stands for the value; the comments after it
    // go to the class, as they would without the run.
    let text = "let a = \\ /* a */ /* b */ \\\nclass C { }\n";
    assert_eq!(
        comment_holders(text),
        comment_holders("let a =  /* a */ /* b */ \nclass C { }\n")
    );
    assert_eq!(error_texts(&parse(text).syntax()), ["\\", "/* b */ \\"]);
    let (class, errors) = class_of("x;\n\\ /* a */ /* b */ \\\nclass C { }\n");
    assert!(class.starts_with("/* a */"), "{class:?}");
    assert_eq!(errors, ["/* b */ \\"]);
    assert_eq!(
        error_texts(&parse("\\\n\n/* a */\n\n\\\n").syntax()),
        ["\\", "\\"]
    );
}

#[test]
fn a_comment_inside_a_quoted_run_is_rejected_text() {
    // Each comment's holder, but the quoted one, which the text without
    // the run does not have.
    let holders = |text: &str| {
        comment_holders(text)
            .into_iter()
            .filter(|(comment, _)| comment != "/* z */")
            .collect::<Vec<_>>()
    };
    for (text, without) in [
        // Under a line of rejected text only, the comment is not above
        // the class.
        (
            "/// Doc.\n\u{201c}/* z */\u{201d}\nclass A { }\n",
            "/// Doc.\n\nclass A { }\n",
        ),
        // On the class's own line, the run goes with the class.
        (
            "let a = 1;\n/// Doc.\n\u{201c}/* z */\u{201d} class A { }\n",
            "let a = 1;\n/// Doc.\n class A { }\n",
        ),
        (
            "/// Doc.\n\n\u{201c}/* z */\u{201d} class A { }\n",
            "/// Doc.\n\n class A { }\n",
        ),
        // A line that holds only a quoted run is blank: it splits the run.
        (
            "/// Doc.\n\\\n\u{201c}/* z */\u{201d}\n\\class A { }\n",
            "/// Doc.\n\n\nclass A { }\n",
        ),
    ] {
        assert_eq!(holders(text), holders(without), "{text:?}");
        assert_rejected_over(text, without);
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(without),
            "{text:?}"
        );
    }
    let text = "/// Doc.\n\n\u{201c}/* z */\u{201d} class A { }\n";
    assert!(class_of(text).0.starts_with("class"));
    assert_eq!(
        comment_holders(text)[1],
        ("/* z */".to_owned(), "SourceFile".to_owned())
    );
    let text = "/// Doc.\n\\\n\u{201c}/* z */\u{201d}\n\\class A { }\n";
    assert!(class_of(text).0.starts_with("class"));
    assert_eq!(
        error_texts(&parse(text).syntax()),
        ["\\\n\u{201c}/* z */\u{201d}", "\\"]
    );
}

#[test]
fn rejected_text_after_a_range_is_not_its_end() {
    // Whether the range in `text` has an end, and its text.
    let range_of = |text: &str| {
        let parse = parse(text);
        let range = parse
            .syntax()
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .unwrap();
        (range.end().is_some(), range.syntax().text().to_string())
    };
    for (text, without) in [
        (
            "let r = (a, ..\\/// f\n\n/* g */\\);\n",
            "let r = (a, ../// f\n\n/* g */);\n",
        ),
        (
            "let r = (a, ../// e\n\\\n);\n",
            "let r = (a, ../// e\n\n);\n",
        ),
        (
            "let r = (a, ../* j */\\\n\n\\/* j2 */);\n",
            "let r = (a, ../* j */\n\n/* j2 */);\n",
        ),
        ("let r = (..\\);\n", "let r = (..);\n"),
    ] {
        assert_eq!(comment_holders(text), comment_holders(without), "{text:?}");
        assert_eq!(
            shape_past_rejected(text),
            shape_past_rejected(without),
            "{text:?}"
        );
        assert_rejected_over(text, without);
        let (has_end, range_text) = range_of(text);
        assert!(!has_end, "{text:?}");
        // Neither the run nor a comment after it is in the range.
        assert!(range_text.ends_with(".."), "{text:?}: {range_text:?}");
    }
    // Rejected text before a real end leaves the end to the range.
    for text in ["let r = a..\\b;\n", "let r = ..\\b;\n"] {
        assert!(range_of(text).0, "{text:?}");
    }
}

#[test]
fn tokens_on_both_sides_of_rejected_text_keep_their_own_boundary() {
    // Joined, `=` and `=>` would lex as `==` and `>`: the parser keeps the
    // tokens it has, since a token of the tree cannot start inside one of
    // the lexer's. The cases pin the tree and the errors that follow.
    let name = "PathExpr( Path( PathSegment( NameRef( Ident ) ) ) )";
    let arrow = |expr: &str, op: &str, arrow: &str| {
        format!(
            "LocalDeclStmt( LocalDecl( LetKw VariableDeclarator( Name( Ident ) Eq {expr}( {name} \
             {op} ) ) ) ) Error( {arrow} ) ExprStmt( {name} Semicolon ) "
        )
    };
    let rejected = || {
        (
            "MR0017".to_owned(),
            "`\\\\` cannot start a token".to_owned(),
            11,
            12,
        )
    };
    let error = |message: &str, start, end| ("MR0110".to_owned(), message.to_owned(), start, end);
    let cases = [
        (
            "let a = x =\\=> y;\n",
            arrow("AssignExpr", "Eq", "FatArrow"),
            vec![
                rejected(),
                error("expected `;`", 12, 12),
                error("expected a statement", 12, 14),
            ],
        ),
        (
            "let a = x -\\-> y;\n",
            arrow("BinExpr", "Minus", "ThinArrow"),
            vec![
                rejected(),
                error("expected `;`", 12, 12),
                error("expected a statement", 12, 14),
            ],
        ),
        (
            "let a = x ?\\?? y;\n",
            format!(
                "LocalDeclStmt( LocalDecl( LetKw VariableDeclarator( Name( Ident ) Eq BinExpr( \
                 PropagateExpr( {name} Question ) QuestionQuestion {name} ) ) ) Semicolon ) "
            ),
            vec![rejected()],
        ),
        (
            "let a = x .\\.. y;\n",
            format!(
                "LocalDeclStmt( LocalDecl( LetKw VariableDeclarator( Name( Ident ) Eq RangeExpr( \
                 MemberExpr( {name} Dot NameRef( ) ) DotDot {name} ) ) ) Semicolon ) "
            ),
            vec![error("expected an identifier", 11, 11), rejected()],
        ),
    ];
    for (text, shape, errors) in cases {
        assert_eq!(shape_past_rejected(text), shape, "{text:?}");
        assert_eq!(full_errors(text), errors, "{text:?}");
    }
}
