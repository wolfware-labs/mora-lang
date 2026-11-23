// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The nesting limit (§3.1, §14.4): every construct that nests is accepted
//! up to the limit, reported once just past it, and parsed without running
//! out of stack far past it.

use mora_diag::{Code, codes};
use mora_syntax::{SyntaxKind as K, parse};

mod common;

use common::assert_errors_hold_tokens;

/// A construct nested `n` times, and the largest `n` the limit accepts. The
/// largest `n` is below 256 where the statement around the construct, or a
/// node inside it, is a level too.
struct Shape {
    name: &'static str,
    text: fn(usize) -> String,
    limit: usize,
}

const SHAPES: &[Shape] = &[
    Shape {
        name: "parentheses",
        text: |n| format!("let x = {}1{};", "(".repeat(n), ")".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "prefix operators",
        text: |n| format!("let x = {}1;", "- ".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "a sum",
        text: |n| format!("let x = 1{};", " + 1".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "assignments",
        text: |n| format!("x{} = 1;", " = x".repeat(n)),
        limit: 253,
    },
    Shape {
        name: "member accesses",
        text: |n| format!("let x = a{};", ".b".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "calls",
        text: |n| format!("let x = f{};", "()".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "indexing",
        text: |n| format!("let x = f{};", "[0]".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "collections",
        text: |n| format!("let x = {}1{};", "[".repeat(n), "]".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "lambdas",
        text: |n| format!("let x = {}1;", "y => ".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "interpolated strings",
        text: |n| format!("let x = {}1{};", "$\"{".repeat(n), "}\"".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "blocks",
        text: |n| format!("{}{}", "{ ".repeat(n), "}".repeat(n)),
        limit: 256,
    },
    Shape {
        name: "if statements",
        text: |n| format!("{}x();", "if (a) ".repeat(n)),
        limit: 253,
    },
    Shape {
        name: "generic arguments",
        text: |n| format!("let x: {}int{} = 1;", "List<".repeat(n), ">".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "array types",
        text: |n| format!("let x: int{} = 1;", "[]".repeat(n)),
        limit: 254,
    },
    Shape {
        name: "tuple types",
        text: |n| {
            format!(
                "let x: {}int, int{} = 1;",
                "(int, ".repeat(n),
                ")".repeat(n)
            )
        },
        limit: 254,
    },
    Shape {
        name: "`not` patterns",
        text: |n| format!("let x = y is {}1;", "not ".repeat(n)),
        limit: 252,
    },
    Shape {
        name: "case patterns",
        text: |n| format!("let x = y is {}_{};", "Some(".repeat(n), ")".repeat(n)),
        limit: 253,
    },
    Shape {
        name: "positional patterns",
        text: |n| format!("let x = y is {}_{};", "(_, ".repeat(n), ")".repeat(n)),
        limit: 253,
    },
    Shape {
        name: "`or` patterns",
        text: |n| format!("let x = y is 1{};", " or 1".repeat(n)),
        limit: 252,
    },
    Shape {
        name: "nested types",
        text: |n| format!("{}{}", "class A { ".repeat(n), "}".repeat(n)),
        limit: 256,
    },
];

/// How many `MR0018` errors `text` gets; it must get no other error, and
/// its tree must hold its text.
fn too_deep(text: &str) -> usize {
    let parse = parse(text);
    assert_eq!(parse.syntax().text(), text);
    assert_errors_hold_tokens(text);
    let others: Vec<_> = parse
        .errors()
        .iter()
        .filter(|error| error.code != codes::MR0018)
        .collect();
    assert!(others.is_empty(), "{others:?}");
    parse.errors().len()
}

/// Runs `test` on a thread with the default stack of a spawned thread, as
/// the compiler's worker threads have.
fn on_small_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(test)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn every_construct_nests_up_to_the_limit() {
    on_small_stack(|| {
        for shape in SHAPES {
            let text = (shape.text)(shape.limit);
            assert_eq!(too_deep(&text), 0, "{}", shape.name);
        }
    });
}

#[test]
fn one_level_past_the_limit_is_one_error() {
    on_small_stack(|| {
        for shape in SHAPES {
            let text = (shape.text)(shape.limit + 1);
            assert_eq!(too_deep(&text), 1, "{}", shape.name);
        }
    });
}

#[test]
fn far_past_the_limit_is_still_one_error() {
    on_small_stack(|| {
        for shape in SHAPES {
            let text = (shape.text)(20_000);
            assert_eq!(too_deep(&text), 1, "{}", shape.name);
        }
    });
}

#[test]
fn the_error_points_at_the_first_level_past_the_limit() {
    // The statement is the first level, so the 256th `(` is the 257th.
    let text = format!("let x = {}1{};", "(".repeat(300), ")".repeat(300));
    let parsed = parse(&text);
    let [error] = parsed.errors() else {
        panic!("{:?}", parsed.errors());
    };
    assert_eq!(error.code, codes::MR0018);
    assert_eq!(error.message, "nesting is too deep");
    assert_eq!(usize::from(error.range.start()), "let x = ".len() + 255);
    assert_eq!(usize::from(error.range.len()), 1);
    // A chain is too deep at its start: its first operand is the deepest.
    let text = format!("let x = 1{};", " + 1".repeat(300));
    let parsed = parse(&text);
    let [error] = parsed.errors() else {
        panic!("{:?}", parsed.errors());
    };
    assert_eq!(usize::from(error.range.start()), "let x = ".len());
}

#[test]
fn each_statement_and_member_gets_its_own_error() {
    let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    let text = format!(
        "let a = {deep};\nlet b = {deep};\nclass C {{\n  F(): int => {deep};\n  G(): int => {deep};\n}}\n"
    );
    assert_eq!(too_deep(&text), 4);
}

#[test]
fn each_statement_of_a_block_body_gets_its_own_error() {
    let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    let text =
        format!("class C {{\n  F(): () {{\n    let a = {deep};\n    let b = {deep};\n  }}\n}}\n");
    assert_eq!(too_deep(&text), 2);
}

#[test]
fn a_member_with_mr0018_has_no_other_error() {
    // The `;` after an expression body is part of the member: missing, it
    // is no error of its own. The next member's errors are still reported.
    let parens = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    let chain = format!("1{}", " + 1".repeat(300));
    for deep in [parens, chain] {
        assert_eq!(
            too_deep(&format!("class C {{\n  F(): int => {deep}\n}}\n")),
            1
        );
        let text = format!("class C {{\n  F(): int => {deep}\n  G(): int => 1\n}}\n");
        let codes: Vec<_> = parse(&text).errors().iter().map(|e| e.code).collect();
        assert_eq!(codes, [codes::MR0018, codes::MR0110]);
    }
}

#[test]
fn each_member_case_and_enum_member_gets_its_own_error() {
    let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    let generic = format!("{}int{}", "List<".repeat(300), ">".repeat(300));
    for text in [
        format!("union U {{ A; F(): int => {deep}; G(): int => {deep}; }}"),
        format!("union U {{ A(x: {generic}), B(y: {generic}) }}"),
        format!("enum E {{ A = {deep}, B = {deep} }}"),
    ] {
        assert_eq!(too_deep(&text), 2, "{}", &text[..20]);
    }
    let text = format!("union U {{ A; F(): int => {deep}; G(): int => 1 }}");
    let codes: Vec<_> = parse(&text).errors().iter().map(|e| e.code).collect();
    assert_eq!(codes, [codes::MR0018, codes::MR0110]);
}

#[test]
fn each_statement_of_a_lambda_in_a_long_chain_gets_its_own_error() {
    // The parser takes a chain in a loop, so only the tree builder sees
    // that the lambda's statements, and only they, are past the limit.
    let text = format!("let x = (y => {{ a(); b(); }}){};", " + 1".repeat(252));
    assert_eq!(too_deep(&text), 2);
}

#[test]
fn a_statement_past_the_limit_is_skipped_to_its_semicolon() {
    // The statement is the 257th level: what follows its first bracketed
    // group is still part of it, and the blocks around it end where
    // they should: the call after them parses.
    for stmt in ["(x) = 1;", "[1, 2].Each(f);", "(a)(b);", "{ a(); } "] {
        let text = format!("{}{stmt} {} b();", "{ ".repeat(256), "}".repeat(256));
        assert_eq!(too_deep(&text), 1, "{stmt}");
        assert_eq!(count(&text, K::CallExpr), 1, "{stmt}");
    }
    // A `;` inside the statement's brackets does not end it; its own `;`
    // does, and the next statement, as deep, gets its own error.
    let text = format!(
        "{}(x) = f(y => {{ a(); }}); b(); {} c();",
        "{ ".repeat(256),
        "}".repeat(256)
    );
    assert_eq!(too_deep(&text), 2);
    assert_eq!(count(&text, K::CallExpr), 1);
}

#[test]
fn a_statement_past_the_limit_that_ends_in_a_block_ends_there() {
    // The 257th level: the statement ends at its block's `}`, or after
    // its `else`, and the next one is parsed, and reported, on its own.
    for stmt in [
        "if (a) { b(); }",
        "if (a) { b(); } else { c(); }",
        "if (a) { b(); } else c();",
        "while (a) { b(); }",
        "{ b(); }",
        "F(): () { b(); }",
    ] {
        let text = format!("{}{stmt} x(); {}", "{ ".repeat(256), "}".repeat(256));
        assert_eq!(too_deep(&text), 2, "{stmt}");
    }
    // A statement that does not end in a block goes on to its `;`, and
    // what an `if`, a `do` or an attribute holds is part of it.
    for stmt in [
        "do { b(); } while (a);",
        "do b(); while (a);",
        "let f = () => { b(); };",
        "if (a) b(); else c();",
        "if (a) b(); else { c(); }",
        "if (a) {} else if (b) c(); else d();",
        "while (a) if (b) c(); else d();",
        "@A if (a) { b(); }",
        "@A.B(1) { b(); }",
        "@A { b(); }",
        "@A F(): () { b(); }",
        "@A G<T>(): () { b(); }",
        "@A<List<int>> if (a) { b(); }",
        "@A<int if (a) { b(); }",
        "@A<int> if (a) { b(); } else c();",
        "unsafe { b(); }",
        "unsafe b();",
        "F(): (int) => int { b(); };",
        "F(): int => new P { X = 1 }.X;",
        "match (a) { _ => 1 }",
        ")",
    ] {
        let text = format!("{}{stmt} x(); {}", "{ ".repeat(256), "}".repeat(256));
        assert_eq!(too_deep(&text), 2, "{stmt}");
    }
}

#[test]
fn an_unclosed_attribute_type_list_leaves_the_block_its_brace() {
    let text = format!("{}x(); @A<(int) {} y();", "{ ".repeat(256), "}".repeat(256));
    assert_eq!(too_deep(&text), 2);
}

#[test]
fn a_declaration_too_deep_before_its_body_still_checks_the_body() {
    // The base type is past the limit; the members and the statements of
    // a body are statements of their own.
    let generic = format!("{}int{}", "List<".repeat(300), ">".repeat(300));
    let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    let text = format!("class C : {generic} {{ F(): int => {deep}; G(): int => 1 }}");
    let codes: Vec<_> = parse(&text).errors().iter().map(|e| e.code).collect();
    assert_eq!(codes, [codes::MR0018, codes::MR0018, codes::MR0110]);
    let text = format!("F(x: {generic}): () {{ let a = {deep}; b() }}");
    let codes: Vec<_> = parse(&text).errors().iter().map(|e| e.code).collect();
    assert_eq!(codes, [codes::MR0018, codes::MR0018, codes::MR0110]);
    let text = format!("union U : {generic} {{ A; F(): int => {deep}; G(): int => 1 }}");
    let codes: Vec<_> = parse(&text).errors().iter().map(|e| e.code).collect();
    assert_eq!(codes, [codes::MR0018, codes::MR0018, codes::MR0110]);
    let text = format!("enum E : {generic} {{ A = {deep}, B = {deep} }}");
    assert_eq!(too_deep(&text), 3);
    // The braces around them are the declaration's own: a missing one is
    // no error of its own.
    for text in [
        format!("class C : {generic}"),
        format!("class C : {generic} {{ G(): int => 1;"),
        format!("F(x: {generic}): () {{ let a = 1;"),
    ] {
        assert_eq!(too_deep(&text), 1, "{}", &text[..8]);
    }
}

#[test]
fn a_statement_gets_one_error_however_many_parts_are_too_deep() {
    // Between the condition and the body only the `if` itself is open.
    let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    assert_eq!(too_deep(&format!("if ({deep}) x = {deep};")), 1);
}

#[test]
fn what_follows_a_construct_that_is_too_deep_parses() {
    // The error node ends at the bracket that closes it, so the enclosing
    // levels and the next statement keep their trees.
    let text = format!(
        "let x = f({}1{}, 2);\nlet y = 3;",
        "(".repeat(300),
        ")".repeat(300)
    );
    assert_eq!(too_deep(&text), 1);
    // In an expression `<` and `>` are operators, not brackets.
    for inner in ["a < b", "(a > b) + 1"] {
        let text = format!(
            "let x = f({}{inner}{}, 2);\nlet y = 3;",
            "(".repeat(300),
            ")".repeat(300)
        );
        assert_eq!(too_deep(&text), 1, "{inner}");
    }
    // What the parser skipped leaves out separators and bodies the
    // levels around it expect, which would be errors of their own.
    let deep = [
        format!("let x = {}1;", "a ? b : ".repeat(300)),
        format!(
            "let x = {}1{};",
            "match (x) { _ => ".repeat(300),
            " }".repeat(300)
        ),
        format!("{}{}", "F(): () { ".repeat(300), "}".repeat(300)),
        format!("F(): () {{ {}{} x(); }}", "{ ".repeat(300), "}".repeat(300)),
        format!(
            "class C {{ {}{} G(): () {{}} }}",
            "class A { ".repeat(300),
            "}".repeat(300)
        ),
        format!(
            "let x = f<{}int{}>(1);",
            "List<".repeat(300),
            ">".repeat(300)
        ),
        format!(
            "let x = y is {}int{};",
            "List<".repeat(20_000),
            ">".repeat(20_000)
        ),
    ];
    for text in deep {
        assert_eq!(too_deep(&text), 1, "{}", &text[..40]);
    }
    let types = format!(
        "let x: Map<{}int{}, int> = m;",
        "List<".repeat(300),
        ">".repeat(300)
    );
    assert_eq!(too_deep(&types), 1);
}

#[test]
fn a_chain_inside_other_levels_adds_its_own() {
    // 200 positional patterns and an `or` chain of 100 inside them: the
    // parser is never more than about 200 levels in, but the tree is 300.
    let text = format!(
        "let x = y is {}1{}{};",
        "(_, ".repeat(200),
        " or 1".repeat(100),
        ")".repeat(200)
    );
    assert_eq!(too_deep(&text), 1);
}

/// How many nodes of `kind` the tree of `text` has. What an `Error` node
/// skipped holds only tokens, so this counts what was parsed around it.
fn count(text: &str, kind: K) -> usize {
    parse(text)
        .syntax()
        .descendants()
        .filter(|node| node.kind() == kind)
        .count()
}

#[test]
fn the_parser_skips_only_what_is_too_deep() {
    let parens = |inner: &str| format!("{}{inner}{}", "(".repeat(300), ")".repeat(300));
    // A bracketed group ends the skip: beside a member list that is too
    // deep, the next member, which is not a level, parses. Its return type
    // is past the limit too, an error of its own. So does the next
    // statement after `<` and `>` used as operators.
    let text = format!(
        "{}class B {{ }} G(): () {{ }} {}",
        "class A { ".repeat(256),
        "}".repeat(256)
    );
    assert_eq!((too_deep(&text), count(&text, K::FunctionDecl)), (2, 1));
    for inner in ["a < b", "(a > b) + 1"] {
        let text = format!("let x = f({}, 2);\nlet y = 3;", parens(inner));
        assert_eq!(count(&text, K::LocalDeclStmt), 2, "{inner}");
        assert_eq!(count(&text, K::Arg), 2, "{inner}");
    }
    // Without brackets, a `,` ends it: `f` keeps its second argument.
    let text = format!("let x = f({}1, 2);", "- ".repeat(300));
    assert_eq!((too_deep(&text), count(&text, K::Arg)), (1, 2));
    // In a type, `<` and `>` are brackets: the initializer after the type
    // parses.
    let generic = |inner: &str| format!("{}{inner}{}", "List<".repeat(300), ">".repeat(300));
    let text = format!("let x: Map<{}, int> = m;", generic("Map<int, int>"));
    assert_eq!((too_deep(&text), count(&text, K::PathExpr)), (1, 1));
}

#[test]
fn a_type_past_the_limit_is_still_a_type() {
    // The lookahead skips the rest of the type, whatever it is made of, so
    // `f<...>(1)` is a call and the pattern a type pattern.
    let generic = |inner: &str| format!("{}{inner}{}", "List<".repeat(300), ">".repeat(300));
    let inner = "Std.Map<(x: int, string), weak T?[]>, (int) => bool uses none, $t, $(f(1, 2))";
    let text = format!("let x = f<{}>(1);", generic(inner));
    assert_eq!((too_deep(&text), count(&text, K::CallExpr)), (1, 1));
    let text = format!("let x = y is {};", generic(inner));
    assert_eq!((too_deep(&text), count(&text, K::TypePattern)), (1, 1));
    // An element that is missing is not a type, even past the limit: the
    // elements of the 256th list are the first that the lookahead skips.
    let text = format!(
        "let x = f<{}int, , int{}>(1);",
        "List<".repeat(256),
        ">".repeat(256)
    );
    assert_eq!(count(&text, K::CallExpr), 0);
    // Nor is an operator: these `<` and `>` compare.
    let text = format!(
        "let x = f<{}int + 1{}>(1);",
        "List<".repeat(256),
        ">".repeat(256)
    );
    assert_eq!(count(&text, K::CallExpr), 0);
    // A short type ends where it ends: `u` is a binding.
    let text = "let ok = s is any IShape u;";
    assert_eq!((too_deep(text), count(text, K::DeclarationPattern)), (0, 1));
}

#[test]
fn a_range_with_no_end_is_as_deep_as_its_text() {
    // The statement is the first level and each `(` one more, so the range
    // inside 254 of them is the 256th level, and its start, `a`, the 257th.
    // A range with no end has nothing inside it to go past the limit.
    let range = |depth: usize, start: &str| {
        let text = format!(
            "let r = {}{start}..{};",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        coded_ranges(&text)
    };
    let at = |depth: usize| "let r = ".len() + depth;
    assert_eq!(range(254, ""), []);
    assert_eq!(range(254, "a"), [(codes::MR0018, at(254), at(254) + 1)]);
    // One level more: the range itself is past the limit, from its first
    // token.
    assert_eq!(range(255, ""), [(codes::MR0018, at(255), at(255) + 2)]);
    assert_eq!(range(255, "a"), [(codes::MR0018, at(255), at(255) + 1)]);
}

/// A place the parser stops at the limit: its name, the text up to where
/// the node past it would start, what ends the text there, the kind of node
/// it would have been in, and each error's code and range.
type Site = (
    &'static str,
    String,
    &'static str,
    &'static str,
    Vec<(Code, usize, usize)>,
);

#[test]
fn a_node_past_the_limit_that_takes_no_token_is_left_out() {
    // Each place the parser stops at the limit, with the text ending where
    // the node past it would start: `MR0018` where it is reported, and no
    // `Error` node for the nothing it would hold. The node it would have been
    // in ends at the last token instead, as it does when an operand is
    // missing.
    let repeat = |s: &str, n: usize| s.repeat(n);
    let sites: Vec<Site> = vec![
        (
            "prefix operand",
            format!("let x = {}", repeat("- ", 255)),
            ";",
            "PrefixExpr",
            vec![(codes::MR0018, 517, 517)],
        ),
        (
            "awaited value",
            format!("let x = {}", repeat("await ", 255)),
            ";",
            "AwaitExpr",
            vec![(codes::MR0018, 1537, 1537)],
        ),
        (
            "lambda body",
            format!("let x = {}", repeat("() => ", 255)),
            ";",
            "LambdaExpr",
            vec![(codes::MR0018, 1537, 1537)],
        ),
        (
            "parenthesized",
            format!("let x = {}", repeat("(", 255)),
            ";",
            "ParenExpr",
            vec![(codes::MR0018, 263, 263)],
        ),
        (
            "tuple element",
            format!("let x = {}", repeat("(", 255)),
            ",",
            "TupleElem",
            vec![(codes::MR0018, 263, 263)],
        ),
        (
            "index",
            format!("let x = a{}", repeat("[", 255)),
            ";",
            "CollectionElem",
            vec![(codes::MR0018, 264, 264)],
        ),
        (
            "conditional branch",
            format!("let x = {}", repeat("a ? b : ", 255)),
            ";",
            "ConditionalExpr",
            vec![(codes::MR0018, 2040, 2041)],
        ),
        (
            "match arm",
            format!("let x = {}", repeat("match (a) { _ => ", 255)),
            ";",
            "MatchArm",
            vec![(codes::MR0018, 4333, 4334)],
        ),
        (
            "generic argument",
            format!("let x: {}", repeat("List<", 255)),
            ";",
            "GenericArgList",
            vec![(codes::MR0018, 1282, 1282)],
        ),
        (
            "tuple type element",
            format!("let x: {}", repeat("(int, ", 255)),
            ";",
            "TupleTypeElem",
            vec![(codes::MR0018, 1532, 1535)],
        ),
        (
            "function type result",
            format!("let x: {}", repeat("(int) => ", 255)),
            ";",
            "FnType",
            vec![(codes::MR0018, 2285, 2288)],
        ),
        (
            "constant pattern",
            format!("let b = x is {}", repeat("not ", 253)),
            ";",
            "ConstantPattern",
            vec![(codes::MR0018, 1024, 1024)],
        ),
        (
            "negated pattern",
            format!("let b = x is {}", repeat("not ", 254)),
            ";",
            "NotPattern",
            vec![(codes::MR0018, 1028, 1028)],
        ),
        (
            "pattern field",
            format!("let b = x is {}", repeat("(", 254)),
            ";",
            "PatternField",
            vec![(codes::MR0018, 267, 267)],
        ),
        (
            "interpolation hole",
            format!("let x = {}", repeat("$\"{", 255)),
            "",
            "InterpHole",
            vec![(codes::MR0012, 8, 10), (codes::MR0018, 773, 773)],
        ),
    ];
    for (name, body, stop, holder, errors) in sites {
        let text = format!("{body}{stop}\n");
        assert_eq!(coded_ranges(&text), errors, "{name}");
        let parse = parse(&text);
        assert_errors_hold_tokens(&text);
        // The node the empty one was in ends before the stop, with no
        // `Error` node last in it; a tuple element that held only that node
        // is empty.
        let at = body.trim_end().len();
        let node = parse
            .syntax()
            .descendants()
            .find(|node| {
                format!("{:?}", node.kind()) == holder
                    && (at..=body.len()).contains(&usize::from(node.text_range().end()))
            })
            .unwrap_or_else(|| panic!("{name}: no {holder} ending at {at}"));
        let last = node
            .children_with_tokens()
            .rfind(|child| !child.kind().is_trivia());
        assert_ne!(last.map(|child| child.kind()), Some(K::Error), "{name}");
    }
}

/// Each error of `text` as its code and range.
fn coded_ranges(text: &str) -> Vec<(Code, usize, usize)> {
    parse(text)
        .errors()
        .iter()
        .map(|error| {
            (
                error.code,
                usize::from(error.range.start()),
                usize::from(error.range.end()),
            )
        })
        .collect()
}

#[test]
fn a_statement_past_the_limit_at_the_end_of_the_text_reports_once() {
    // The statement after the attributes is past the limit and the text
    // ends there: one `MR0018`, at the end, and no `Error` node.
    for text in [
        format!("{}@A", "{ ".repeat(255)),
        format!("{}@A \u{201c}", "{ ".repeat(255)),
    ] {
        let errors = coded_ranges(&text);
        let too_deep: Vec<_> = errors
            .iter()
            .filter(|error| error.0 == codes::MR0018)
            .collect();
        assert_eq!(too_deep, [&(codes::MR0018, 512, 512)], "{text:?}");
        assert_errors_hold_tokens(&text);
    }
}

#[test]
fn the_statement_after_one_past_the_limit_reports_its_own_errors() {
    // Muting ends with the statement that went past the limit, though its
    // node was left out.
    let text = format!("let x = {};\nlet z = ;\n", "- ".repeat(255));
    assert_eq!(
        coded_ranges(&text),
        [(codes::MR0018, 517, 517), (codes::MR0110, 527, 527)]
    );
}

/// The kind of the node that holds the first token of `text` that is `what`.
fn holder_of(text: &str, what: &str) -> K {
    let parse = parse(text);
    parse
        .syntax()
        .descendants()
        .find(|node| {
            node.children_with_tokens()
                .any(|child| child.into_token().is_some_and(|token| token.text() == what))
                || node
                    .children()
                    .any(|child| child.kind() == K::Error && child.text() == what)
        })
        .map(|node| node.kind())
        .unwrap()
}

#[test]
fn what_comes_before_a_node_left_out_goes_where_it_would_without_it() {
    // Past the limit the node left out is the operand, so nothing is missing
    // for rejected text there to stand for: it is parsed as if absent and
    // goes where its whitespace does, in the statement, as the comment does.
    // Without the limit it stands for the missing operand, in the
    // parentheses.
    let deep = |inner: &str| format!("let x = {} {inner} ;\nlet z = ;\n", "(".repeat(255));
    let bare = deep("");
    assert_eq!(coded_ranges(&bare)[0], (codes::MR0018, 263, 263));
    let run = deep("\u{201c}");
    assert_eq!(holder_of(&run, "\u{201c}"), K::LocalDeclStmt);
    assert_errors_hold_tokens(&run);
    let comment = deep("/* c */");
    assert_eq!(holder_of(&comment, "/* c */"), K::LocalDeclStmt);
    assert_eq!(
        holder_of("let x = ( /* c */ ;\n", "/* c */"),
        K::LocalDeclStmt
    );
    assert_eq!(
        holder_of("let x = ( \u{201c} ;\n", "\u{201c}"),
        K::ParenExpr
    );
    // The errors are those of the text without them, past the rejected
    // token, and the statement after it reports its own.
    let shifted = |text: &str, by: usize| {
        coded_ranges(text)
            .into_iter()
            .filter(|error| error.0 != codes::MR0017)
            .map(|(code, start, end)| {
                let by = if start > 263 { by } else { 0 };
                (code, start - by, end - by)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(shifted(&run, "\u{201c}".len()), coded_ranges(&bare));
    assert_eq!(shifted(&comment, "/* c */".len()), coded_ranges(&bare));
}
