//! Tests of the control-flow checks.

use mora_diag::Diagnostic;
use mora_span::{FileId, Project, ProjectKind, SourceFile};

/// A database for the tests.
#[salsa::db]
#[derive(Clone, Default)]
struct TestDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for TestDb {}

#[salsa::db]
impl mora_span::Db for TestDb {}

/// The control-flow errors of `src/Program.mr` alone in an app.
fn diagnostics_of(text: &str) -> Vec<Diagnostic> {
    let db = TestDb::default();
    let file = SourceFile::new(&db, FileId::new(0), "src/Program.mr".into(), text.into());
    let project = Project::new(
        &db,
        "app".into(),
        vec![file],
        "App".into(),
        ProjectKind::App,
        "src/Program.mr".into(),
        Vec::new(),
    );
    crate::diagnostics(&db, project).clone()
}

/// The codes `text` reports.
fn reported(text: &str) -> Vec<String> {
    diagnostics_of(text)
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn a_local_is_read_only_once_assigned() {
    let text = "let c = true;\nlet a: int;\nif (c) {\n    a = 1;\n} else {\n    a = 2;\n}\n\
                let b = a;\nvar d: int;\nif (c) {\n    d = 1;\n}\nlet e = d;\n";
    assert_eq!(reported(text), ["MR0181"]);
}

/// `text` with the edits of every fix its diagnostics offer applied.
fn fixed(text: &str) -> String {
    let mut edits: Vec<_> = diagnostics_of(text)
        .iter()
        .flat_map(|d| d.fixes.iter().flat_map(|fix| fix.edits.clone()))
        .collect();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.range.start()));
    let mut out = text.to_owned();
    for edit in &edits {
        out.replace_range(
            std::ops::Range::<usize>::from(edit.span.range),
            &edit.replacement,
        );
    }
    out
}

/// The messages `text` reports.
fn messages(text: &str) -> Vec<String> {
    diagnostics_of(text)
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn loops_may_run_any_number_of_times() {
    let c = "let c = true;\n";
    for (text, expected) in [
        ("let x: int;\nwhile (c) {\n    x = 1;\n}\n", vec!["MR0182"]),
        (
            "var y: int;\nwhile (c) {\n    y = 1;\n}\nlet z = y;\n",
            vec!["MR0181"],
        ),
        (
            "var w: int;\nwhile (true) {\n    w = 1;\n    break;\n}\nlet v = w;\n",
            vec![],
        ),
        (
            "var d: int;\ndo {\n    d = 1;\n} while (c);\nlet e = d;\n",
            vec![],
        ),
        (
            "var f: int;\nfor (var i = 0; i < 3; i++) {\n    f = i;\n}\nlet g = f;\n",
            vec!["MR0181"],
        ),
        (
            "var h: int;\nforeach (let k in [1]) {\n    h = k;\n}\nlet m = h;\n",
            vec!["MR0181"],
        ),
        (
            "var n: int;\nfor (;;) {\n    n = 1;\n    break;\n}\nlet o = n;\n",
            vec![],
        ),
        (
            "var p: int;\nwhile (c) {\n    if (c) {\n        continue;\n    }\n    p = 1;\n}\n",
            vec![],
        ),
        (
            "let q: int;\nwhile (c) {\n    let r: int;\n    r = 1;\n}\nq = 1;\n",
            vec![],
        ),
        (
            "let s: int;\nwhile (c) {\n    let t = (s = 1, 2);\n}\n",
            vec!["MR0182"],
        ),
        (
            "var u: int;\ndo {\n    if (c) {\n        continue;\n    }\n    u = 1;\n} while (c);\nlet uu = u;\n",
            vec!["MR0181"],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
}

#[test]
fn a_let_is_assigned_exactly_once() {
    let c = "let c = true;\n";
    for (text, expected) in [
        ("let a = 1;\na = 2;\n", vec!["MR0182"]),
        ("let b: int;\nb = 1;\nb = 2;\n", vec!["MR0182"]),
        (
            "let d: int;\nif (c) {\n    d = 1;\n}\nd = 2;\n",
            vec!["MR0182"],
        ),
        (
            "let e: int;\nif (c) {\n    e = 1;\n} else {\n    e = 2;\n}\n",
            vec![],
        ),
        (
            "let f = 1;\nf += 1;\nf++;\n--f;\n",
            vec!["MR0182", "MR0182", "MR0182"],
        ),
        ("var g = 1;\ng = 2;\ng += 1;\n", vec![]),
        (
            "let h = (1, 2);\nif (h is (let i, _)) {\n    i = 3;\n}\n",
            vec!["MR0182"],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
    assert_eq!(
        messages("let a = 1;\na = 2;\nlet b: int;\nb = 1;\nb = 2;\n"),
        [
            "`a` is declared with `let`, so it cannot be assigned again",
            "`b` is declared with `let` and may be assigned already",
        ]
    );
    // The fix declares it with `var`, and the result is right.
    let text = "let a = 1;\na = 2;\nlet b: int;\nb = 1;\nb = 2;\n";
    let after = fixed(text);
    assert_eq!(after, "var a = 1;\na = 2;\nvar b: int;\nb = 1;\nb = 2;\n");
    assert_eq!(reported(&after), Vec::<String>::new());
    // A pattern binding has no `let` before its name, so no fix.
    let found = diagnostics_of("let p = (1, 2);\nlet (x, y) = p;\nx = 1;\n");
    assert_eq!(found.len(), 1);
    assert!(found[0].fixes.is_empty());
}

#[test]
fn closures_see_what_is_assigned_where_they_are_declared() {
    for (text, expected) in [
        ("let x: int;\nlet f = () => x;\nx = 1;\n", vec!["MR0181"]),
        ("var y = 1;\nlet g = () => {\n    y = 2;\n};\n", vec![]),
        (
            "let z = 1;\nlet h = () => {\n    z = 2;\n};\n",
            vec!["MR0182"],
        ),
        ("let w: int;\nF() {\n    w = 1;\n}\n", vec![]),
        (
            "Main() {\n    let w: int;\n    G() {\n        w = 1;\n    }\n}\n",
            vec!["MR0182"],
        ),
        (
            "Main() {\n    let v: int;\n    H(): int => v;\n    v = 1;\n}\n",
            vec!["MR0181"],
        ),
        ("let a: int;\na = 1;\nlet k = () => a;\n", vec![]),
        (
            "var b: int;\nlet m = () => {\n    b = 1;\n};\nlet n = b;\n",
            vec!["MR0181"],
        ),
    ] {
        assert_eq!(reported(text), expected, "{text}");
    }
    assert_eq!(
        messages("let z = 1;\nlet h = () => {\n    z = 2;\n};\n"),
        ["`z` is declared with `let` and cannot be assigned in a closure"]
    );
}

#[test]
fn break_and_continue_leave_a_loop() {
    let c = "let c = true;\nlet n = 1;\n";
    for (text, expected) in [
        ("break;\n", vec!["MR0183"]),
        ("continue;\n", vec!["MR0183"]),
        (
            "while (c) {\n    let f = () => {\n        break;\n    };\n}\n",
            vec!["MR0183"],
        ),
        (
            "while (c) {\n    G() {\n        continue;\n    }\n}\n",
            vec!["MR0183"],
        ),
        (
            "while (c) {\n    match (n) {\n        1 => { break; }\n        _ => { continue; }\n    }\n}\n",
            vec![],
        ),
        (
            "for (;;) {\n    break;\n}\nforeach (let x in [1]) {\n    continue;\n}\ndo {\n    break;\n} while (c);\n",
            vec![],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
    assert_eq!(
        messages("break;\ncontinue;\n"),
        ["`break` outside a loop", "`continue` outside a loop"]
    );
}

#[test]
fn a_function_that_returns_a_value_returns_one_on_every_path() {
    for (text, expected) in [
        ("F(): int {\n}\n", vec!["MR0184"]),
        (
            "G(c: bool): int {\n    if (c) {\n        return 1;\n    }\n}\n",
            vec!["MR0184"],
        ),
        (
            "H(c: bool): int {\n    if (c) {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n",
            vec![],
        ),
        ("I(): int {\n    while (true) {}\n}\n", vec![]),
        (
            "J(): int {\n    while (true) {\n        break;\n    }\n}\n",
            vec!["MR0184"],
        ),
        ("K(): int {\n    panic(\"no\");\n}\n", vec![]),
        ("L(): int {\n    Todo();\n}\n", vec![]),
        ("M(): int {\n    let x = Unreachable();\n}\n", vec![]),
        (
            "N(n: int): int {\n    match (n) {\n        1 => { return 1; }\n        _ => { return 2; }\n    }\n}\n",
            vec![],
        ),
        (
            "O(n: int): int {\n    match (n) {\n        1 => { return 1; }\n        _ => {}\n    }\n}\n",
            vec!["MR0184"],
        ),
        ("P(): int {\n    return;\n}\n", vec!["MR0184"]),
        (
            "Q(): int => 1;\nR() {}\nS(): () {\n    return;\n}\n",
            vec![],
        ),
        ("T() {\n    U(): int {\n    }\n}\n", vec!["MR0184"]),
        (
            "V(): int {\n    for (;;) {}\n}\nW(): int {\n    do {} while (true);\n}\n",
            vec![],
        ),
        ("X(): Unknwn {\n}\n", vec![]),
        (
            "Y(): int {\n    let f = () => {\n        return;\n    };\n    return 1;\n}\n",
            vec![],
        ),
        (
            "panic(s: string) {}\nZ(): int {\n    panic(\"x\");\n}\n",
            vec!["MR0184"],
        ),
        (
            "A(c: bool): int {\n    while (c) {\n        return 1;\n    }\n}\n",
            vec!["MR0184"],
        ),
        (
            "B(c: bool): int {\n    return c ? 1 : panic(\"no\");\n}\n",
            vec![],
        ),
    ] {
        assert_eq!(reported(text), expected, "{text}");
    }
    assert_eq!(
        messages("F(): int {\n}\nP(): int {\n    return;\n}\n"),
        [
            "`F` can reach its end without returning a value",
            "`return` without a value in `P`",
        ]
    );
    // The label is on the closing brace.
    let text = "F(): int {\n}\n";
    let found = diagnostics_of(text);
    assert_eq!(&text[found[0].primary.span.range], "}");
}

#[test]
fn unreachable_code_reports_nothing() {
    let text = "F(): int {\n    return 1;\n    let x: int;\n    let y = x;\n}\n\
                G() {\n    panic(\"no\");\n    let a: int;\n    let b = a;\n}\n";
    assert_eq!(reported(text), Vec::<String>::new());
    // `break` outside a loop is wrong wherever it is written.
    assert_eq!(reported("F() {\n    return;\n    break;\n}\n"), ["MR0183"]);
}

#[test]
fn every_kind_of_expression_is_walked_for_reads() {
    let text = "let x: int;\nlet a = (x, [x], $\"{x,x}\", x.Y, x[x], -x, x + x, x ?? x, \
                x is 1, x as int, (int)x, x..x, x with { Y = x }, new P(x) { Y = x }, \
                new int[x], c ? x : x, F(x), match (x) { 1 when x > 0 => x, _ => x });\n\
                let c = true;\nF(n: int) {}\nstruct P {\n    Y: int;\n}\n";
    // The first read reports; the rest of the path counts it as assigned.
    assert_eq!(reported(text), ["MR0181"]);
}
