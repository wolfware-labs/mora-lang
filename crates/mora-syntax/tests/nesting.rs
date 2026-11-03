// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The nesting limit (§3.1, §14.4): every construct that nests is accepted
//! up to the limit, reported once just past it, and parsed without running
//! out of stack far past it.

use mora_diag::codes;
use mora_syntax::{SyntaxKind as K, parse};

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
