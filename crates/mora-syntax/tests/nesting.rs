// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The nesting limit (§3.1, §14.4): every construct that nests is accepted
//! up to the limit, reported once just past it, and parsed without running
//! out of stack far past it.

use mora_diag::codes;
use mora_syntax::parse;

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
fn what_follows_a_construct_that_is_too_deep_parses() {
    // The error node ends at the bracket that closes it, so the enclosing
    // levels and the next statement keep their trees.
    let text = format!(
        "let x = f({}1{}, 2);\nlet y = 3;",
        "(".repeat(300),
        ")".repeat(300)
    );
    assert_eq!(too_deep(&text), 1);
    let types = format!(
        "let x: Map<{}int{}, int> = m;",
        "List<".repeat(300),
        ">".repeat(300)
    );
    assert_eq!(too_deep(&types), 1);
}
