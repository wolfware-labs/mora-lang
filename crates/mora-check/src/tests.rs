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

#[test]
fn every_expression_reads_what_it_holds() {
    let decls = "F(n: int) {}\nstruct P {\n    Y: int;\n}\nlet c = true;\nlet p = new P { Y = 1 };\nlet a = [1];\n";
    for form in [
        "(x, 1)",
        "[x]",
        "$\"{x}\"",
        "$\"{1,x}\"",
        "x.Y",
        "a[x]",
        "-x",
        "x + 1",
        "c && x > 0",
        "x?",
        "await x",
        "(int)x",
        "x as int",
        "x is int",
        "x..2",
        "p with { Y = x }",
        "new P(x)",
        "new P { Y = x }",
        "new int[x]",
        "c ? x : 1",
        "F(x)",
        "match (x) { _ => 1 }",
        "match (1) { _ when x > 0 => 1, _ => 2 }",
        "match (1) { < x => 1, _ => 2 }",
        "() => x",
        "(y: int = x) => y",
        "1 is P { Y: x }",
        "1 is [x]",
    ] {
        let text = format!("{decls}var x: int;\nlet r = {form};\n");
        assert_eq!(reported(&text), ["MR0181"], "{form}");
    }
    // A compound assignment reads its place first, and so does `++`.
    assert_eq!(reported("var x: int;\nx += 1;\n"), ["MR0181"]);
    assert_eq!(reported("var x: int;\nx++;\n"), ["MR0181"]);
    assert_eq!(
        reported("var x: int;\nx = 1;\nx += 1;\n"),
        Vec::<String>::new()
    );
}

#[test]
fn an_assignment_anywhere_in_a_loop_counts_for_later_runs() {
    let decls = "F(n: int) {}\nstruct P {\n    Y: int;\n}\nlet c = true;\nlet p = new P { Y = 1 };\nvar arr = [1];\n";
    for form in [
        "let t = -(x = 1);",
        "let t = (x = 1)?;",
        "let t = await (x = 1);",
        "let t = (int)(x = 1);",
        "let t = (x = 1) as int;",
        "let t = (x = 1).Y;",
        "let t = new int[x = 1];",
        "let t = (x = 1) is int;",
        "let t = $\"{(x = 1)}\";",
        "let t = $\"{1,(x = 1)}\";",
        "let t = [x = 1];",
        "F(x = 1);",
        "let t = arr[x = 1];",
        "let t = 1 + (x = 1);",
        "let t = (x = 1)..2;",
        "let t = match (x = 1) { _ => 1 };",
        "let t = match (1) { _ when (x = 1) > 0 => 1, _ => 2 };",
        "let t = p with { Y = (x = 1) };",
        "let t = c ? (x = 1) : 2;",
        "let t = new P(x = 1);",
        "let t = new P { Y = (x = 1) };",
        "arr[x = 1] = 2;",
        "x++;",
        "--x;",
        "if (c) {} else { x = 1; }",
        "while (c) { x = 1; }",
        "do { x = 1; } while (c);",
        "for (var i = (x = 1); c; x = 2) {}",
        "foreach (let k in [x = 1]) {}",
        "match (1) { _ when (x = 1) > 0 => {} _ => {} }",
        "unsafe { x = 1; }",
        "return (x = 1);",
        "let (u, v) = (x = 1, 2);",
        "if ((x = 1) > 0) {}",
    ] {
        let text = format!("{decls}let x: int;\nwhile (c) {{\n    {form}\n}}\n");
        let found = reported(&text);
        assert!(found.contains(&"MR0182".to_owned()), "{form}: {found:?}");
    }
    // A closure's assignment is its own.
    let text = format!(
        "{decls}var x: int;\nwhile (c) {{\n    let f = () => {{\n        x = 1;\n    }};\n}}\n"
    );
    assert_eq!(reported(&text), Vec::<String>::new());
}

#[test]
fn notes_say_where_a_closure_or_a_function_stands() {
    let note = |text: &str| diagnostics_of(text)[0].note.clone().unwrap_or_default();
    let label = |text: &str| {
        diagnostics_of(text)[0]
            .primary
            .message
            .clone()
            .unwrap_or_default()
    };
    assert_eq!(
        label("var x: int;\nlet f = () => x;\n"),
        "the closure reads `x`, which is not assigned where it is declared"
    );
    assert_eq!(
        label("var x: int;\nlet y = x;\n"),
        "`x` may not be assigned here"
    );
    assert_eq!(
        note("while (true) {\n    let f = () => {\n        break;\n    };\n}\n"),
        "a lambda or a local function does not see the loops around it, so its `break` and `continue` have no loop to leave"
    );
    assert_eq!(
        note("break;\n"),
        "`break` and `continue` act on the innermost `while`, `do`, `for` or `foreach` around them"
    );
    assert_eq!(
        note("F(): int {\n}\n"),
        "`F` returns a value, so every path through it ends in `return` with a value, a call of `panic`, `Unreachable` or `Todo`, or a loop that never ends"
    );
}

#[test]
fn rarer_statements_and_owners_are_walked() {
    for (text, expected) in [
        ("var x: int;\nunsafe {\n    x = 1;\n}\nlet y = x;\n", vec![]),
        (
            "let n = 1;\nvar x: int;\nmatch (n) {}\nlet y = x;\n",
            vec!["MR0181"],
        ),
        ("F(): int {\n    match (1) {}\n}\n", vec!["MR0184"]),
        (
            "class C {\n    C(a: int) : this(a, a) {}\n    C(a: int, b: int) {}\n}\n",
            vec![],
        ),
        ("F(a: int = 1): int {\n}\n", vec!["MR0184"]),
        ("G(): int {\n    Std.Debug.panic();\n}\n", vec!["MR0184"]),
        ("let (a, b) = (1, 2);\nlet c = a + b;\n", vec![]),
    ] {
        assert_eq!(reported(text), expected, "{text}");
    }
    // A function body the parser recovered from has no end to point at.
    assert_eq!(reported("F(): int {\n"), Vec::<String>::new());
}

#[test]
fn broken_code_is_walked_without_panicking() {
    let text = "F(n: int): int {\n    var x: int;\n    let y: int;\n    while (n > 0) {\n        \
                if (n is P { Y: let k } or [_, ..]) { x = k; } else { continue; }\n        \
                y = 1;\n        break;\n    }\n    match (n) { 1 when x > 0 => { return x; } _ => {} }\n    \
                let f = (a: int = 1) => a + x;\n    for (var i = 0; i < n; i++) { x += i; }\n    \
                foreach (let (u, v) in [(1, 2)]) { y = u; }\n    unsafe { x = await n; }\n    return x;\n}\n";
    for (cut, _) in text.char_indices() {
        let _ = diagnostics_of(&text[..cut]);
    }
}
