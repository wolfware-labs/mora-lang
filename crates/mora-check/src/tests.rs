//! Tests of the control-flow checks.

use mora_diag::Diagnostic;
use mora_hir::test_support::{TestDb, app, apply_fixes, codes, file_named};
use salsa::Setter;

/// The control-flow errors of `src/Program.mr` alone in an app.
fn diagnostics_of(text: &str) -> Vec<Diagnostic> {
    let db = TestDb::new();
    let project = app(&db, &[("src/Program.mr", text)]);
    crate::diagnostics(&db, project).clone()
}

/// The codes `text` reports.
fn reported(text: &str) -> Vec<String> {
    codes(&diagnostics_of(text))
}

/// `text` with the edits of every fix its diagnostics offer applied.
fn fixed(text: &str) -> String {
    apply_fixes(text, diagnostics_of).text
}

/// The messages `text` reports.
fn messages(text: &str) -> Vec<String> {
    mora_hir::test_support::messages(&diagnostics_of(text))
}

#[test]
fn an_edit_checks_the_edited_file_again_only() {
    let mut db = TestDb::new();
    let text = |n: u32| format!("A(): int {{\n    let x: int;\n    return x + {n};\n}}\n");
    let project = app(
        &db,
        &[
            ("src/Program.mr", &text(1)),
            ("src/Other.mr", "B(): int {\n}\n"),
        ],
    );
    let file = file_named(&db, project, "src/Program.mr");
    assert_eq!(
        codes(crate::diagnostics(&db, project)),
        ["MR0184", "MR0181"]
    );
    db.take_executed();
    file.set_text(&mut db).to(text(2));
    assert_eq!(
        codes(crate::diagnostics(&db, project)),
        ["MR0184", "MR0181"]
    );
    let rebuilt = db
        .take_executed()
        .into_iter()
        .filter(|key| key.starts_with("file_diagnostics("))
        .count();
    assert_eq!(rebuilt, 1);
}

#[test]
fn a_local_is_read_only_once_assigned() {
    let text = "let c = true;\nlet a: int;\nif (c) {\n    a = 1;\n} else {\n    a = 2;\n}\n\
                let b = a;\nvar d: int;\nif (c) {\n    d = 1;\n}\nlet e = d;\n";
    assert_eq!(reported(text), ["MR0181"]);
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
    // Nor has the binding of a type pattern, whose type may end in `let`.
    let text = "struct Bullet {\n    Y: int;\n}\nlet o = new Bullet { Y = 1 };\n\
                if (o is Bullet b) {\n    b = o;\n}\n";
    let found = diagnostics_of(text);
    assert_eq!(reported(text), ["MR0182"]);
    assert!(found[0].fixes.is_empty());
    // A `foreach` or binding pattern `let` before the name is fixed.
    assert_eq!(
        fixed("foreach (let k in [1]) {\n    k = 2;\n}\nif (1 is let m) {\n    m = 2;\n}\n"),
        "foreach (var k in [1]) {\n    k = 2;\n}\nif (1 is var m) {\n    m = 2;\n}\n"
    );
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
    // A function body the parser recovered from has no end to point at,
    // even when its last statement ends with a brace.
    assert_eq!(reported("F(): int {\n"), Vec::<String>::new());
    assert_eq!(
        reported("F(): int {\n    if (true) {}\n"),
        Vec::<String>::new()
    );
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

#[test]
fn patterns_defaults_and_qualified_calls() {
    for (text, expected) in [
        (
            "Main() {\n    var x: int;\n    G(a: int = x) {}\n}\n",
            vec!["MR0181"],
        ),
        ("let n = 1;\nlet t = n is int k && k > 0;\n", vec![]),
        (
            "union U { A(v: int), B }\nlet o = U.B;\nvar x: int;\nlet t = o is U.A(< x);\n",
            vec!["MR0181"],
        ),
        ("H() {}\nF(): int {\n    App.H();\n}\n", vec!["MR0184"]),
    ] {
        assert_eq!(reported(text), expected, "{text}");
    }
}

#[test]
fn a_call_of_std_panic_ends_a_path() {
    assert_eq!(
        reported("F(): int {\n    Std.panic(\"x\");\n}\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        reported("G(): int {\n    Std.Debug.panic();\n}\n"),
        ["MR0184"]
    );
    // `Debug` is a name of the prelude, but not the standard library's root.
    assert_eq!(reported("H(): int {\n    Debug.panic();\n}\n"), ["MR0184"]);
}

#[test]
fn methods_and_extension_functions_return_a_value_on_every_path() {
    let method = "interface I {\n    M(): int;\n}\nclass C : I {\n    I.M(): int {\n    }\n}\n";
    assert_eq!(reported(method), ["MR0184"]);
    let extension = "struct S {\n    X: int;\n}\nextension (s: S) {\n    F(): int {\n    }\n    \
                     G(): int {\n        return 1;\n    }\n}\n";
    assert_eq!(reported(extension), ["MR0184"]);
    // An operator waits for its return type (D-138).
    let operator =
        "struct V {\n    X: int;\n    public static operator +(a: V, b: V): V {\n    }\n}\n";
    assert_eq!(reported(operator), Vec::<String>::new());
}

#[test]
fn only_a_path_back_to_the_loop_start_assigns_again() {
    let c = "let c = true;\nlet x: int;\n";
    for (text, expected) in [
        ("while (c) {\n    x = 1;\n    break;\n}\n", vec![]),
        (
            "while (true) {\n    x = 1;\n    break;\n}\nlet y = x;\n",
            vec![],
        ),
        ("do {\n    x = 1;\n} while (false);\n", vec![]),
        (
            "while (c) {\n    if (c) {\n        x = 1;\n        break;\n    }\n}\n",
            vec![],
        ),
        (
            "while (c) {\n    if (c) {\n        x = 1;\n        continue;\n    }\n}\n",
            vec!["MR0182"],
        ),
        (
            "while (c) {\n    while (c) {\n        x = 1;\n        break;\n    }\n}\n",
            vec!["MR0182"],
        ),
        (
            "for (var i = 0; i < 3; i++) {\n    x = i;\n    return;\n}\n",
            vec![],
        ),
        ("foreach (let k in [1]) {\n    x = k;\n}\n", vec!["MR0182"]),
        (
            "while (c) {\n    while (c) {\n        if (c) {\n            x = 1;\n            return;\n        }\n    }\n}\n",
            vec![],
        ),
        (
            "while (c) {\n    while (c) {\n        if (c) {\n            x = 1;\n        }\n    }\n}\n",
            vec!["MR0182"],
        ),
        (
            "do {\n    if (c) {\n        continue;\n    }\n    x = 1;\n} while (c);\n",
            vec!["MR0182"],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
}

#[test]
fn a_condition_assigns_what_its_true_or_false_paths_assign() {
    let c = "let c = true;\nvar x: int;\n";
    for (text, expected) in [
        ("if (c && (x = 1) > 0) {\n    let y = x;\n}\n", vec![]),
        (
            "if (c || (x = 1) > 0) {\n} else {\n    let y = x;\n}\n",
            vec![],
        ),
        ("if (!(c || (x = 1) > 0)) {\n    let y = x;\n}\n", vec![]),
        (
            "if (c && (x = 1) > 0) {\n} else {\n    let y = x;\n}\n",
            vec!["MR0181"],
        ),
        ("let b = c && (x = 1) > 0;\nlet y = x;\n", vec!["MR0181"]),
        ("let b = c ?? (x = 1);\nlet y = x;\n", vec!["MR0181"]),
        ("let b = 1 + (x = 1);\nlet y = x;\n", vec![]),
        ("while (c && (x = 1) > 0) {\n    let y = x;\n}\n", vec![]),
        ("let z = c && (x = 1) > 0 ? x : 0;\n", vec![]),
        (
            "match (1) {\n    _ when c && (x = 1) > 0 => x,\n    _ => 0,\n};\n",
            vec![],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
}

#[test]
fn an_arm_after_a_guard_sees_what_the_guard_assigned() {
    let text = "let x: int;\nmatch (1) {\n    _ when (x = 1) > 0 => {}\n    _ => {\n        x = 2;\n    }\n}\n";
    assert_eq!(reported(text), ["MR0182"]);
}

#[test]
fn a_deconstructing_assignment_assigns_each_place() {
    let c = "let c = true;\n";
    for (text, expected) in [
        (
            "let a: int;\nvar b: int;\n(a, b) = (1, 2);\nlet d = a + b;\n",
            vec![],
        ),
        (
            "let a = 1;\nvar b: int;\n(a, b) = (1, 2);\n",
            vec!["MR0182"],
        ),
        (
            "var a: int;\nvar b: int;\nvar e: int;\n(a, (b, _)) = (1, (2, 3));\nlet d = a + b + e;\n",
            vec!["MR0181"],
        ),
        (
            "let a: int;\nwhile (c) {\n    (a, _) = (1, 2);\n}\n",
            vec!["MR0182"],
        ),
        ("var a: int;\n(a, a.Length) = (\"x\", 1);\n", vec!["MR0181"]),
        // Only `=` deconstructs: a compound assignment reads its places.
        ("let a = 1;\nlet b = 2;\n(a, b) += (1, 2);\n", vec![]),
        (
            "var a: int;\nvar b = 2;\n(a, b) += (1, 2);\n",
            vec!["MR0181"],
        ),
        (
            "let a: int;\nlet b: int;\n((a, (b))) = (1, 2);\nlet d = a + b;\n",
            vec![],
        ),
    ] {
        assert_eq!(reported(&format!("{c}{text}")), expected, "{text}");
    }
}

#[test]
fn async_bodies_wait_for_their_return_types() {
    // An item, a local function, an interface member and an extension
    // function, each `async`, with an end and a `return;` (D-138).
    let found = [
        "async F(): int {\n}\nasync G(): int {\n    return;\n}\n",
        "H() {\n    async L(): int {\n    }\n    async M(): int {\n        return;\n    }\n}\n",
        "interface I {\n    M(): int;\n}\nclass C : I {\n    async I.M(): int {\n        return;\n    }\n}\n",
        "struct S {\n    X: int;\n}\nextension (s: S) {\n    async F(): int {\n    }\n}\n",
    ]
    .map(messages);
    assert_eq!(found, [(); 4].map(|()| Vec::<String>::new()));
}

#[test]
fn a_guarded_last_arm_leaves_its_guard_false_path_open() {
    let returns = "F(c: bool, x: int): int {\n    match (x) {\n        _ when c => {\n            return 1;\n        }\n    }\n}\n";
    assert_eq!(
        messages(returns),
        ["`F` can reach its end without returning a value"]
    );
    let closed = "F(c: bool, x: int): int {\n    match (x) {\n        _ when c => {\n            return 1;\n        }\n        _ => {\n            return 2;\n        }\n    }\n}\n";
    assert_eq!(messages(closed), Vec::<String>::new());
    let assigns = "let c = true;\nlet a: int;\nmatch (1) {\n    _ when c => {\n        a = 1;\n    }\n}\nlet b = a;\n";
    assert_eq!(messages(assigns), ["`a` is used before it is assigned"]);
}

#[test]
fn an_is_binding_is_assigned_only_where_the_test_holds() {
    let read = ["`b` is used before it is assigned"];
    for (test, expected) in [
        (
            "if (o is int b) {\n    } else {\n        let d = b;\n    }",
            read.to_vec(),
        ),
        (
            "if (!(o is int b)) {\n        let d = b;\n    }",
            read.to_vec(),
        ),
        (
            "if (o is int b || c) {\n        let d = b;\n    }",
            read.to_vec(),
        ),
        (
            "if (o is int b && b > 0) {\n        let d = b;\n    }",
            vec![],
        ),
        (
            "if (!(o is int b) || b > 0) {\n    } else {\n        let d = b;\n    }",
            vec![],
        ),
        ("let t = o is int b && b > 0;", vec![]),
        ("let t = o is int b ? b : b;", read.to_vec()),
        (
            "match (o) {\n        int b => b,\n        _ => 0,\n    };",
            vec![],
        ),
        ("let (b, e) = (1, 2);\n    let d = b + e;", vec![]),
    ] {
        let text = format!("F(o: int, c: bool) {{\n    {test}\n}}\n");
        assert_eq!(messages(&text), expected, "{test}");
    }
    let text = "F(o: int) {\n    if (!(o is int b)) {\n        let d = b;\n    }\n}\n";
    let note = diagnostics_of(text)[0].note.clone().unwrap_or_default();
    assert!(
        note.starts_with("a pattern's binding is assigned only where"),
        "{note}"
    );
}

#[test]
fn bindings_under_not_and_or_follow_the_test() {
    let read = |name: &str| vec![format!("`{name}` is used before it is assigned")];
    for (test, expected) in [
        (
            "if (o is not int u) {\n        let x = u;\n    }",
            read("u"),
        ),
        (
            "if (o is not int u) {\n    } else {\n        let x = u;\n    }",
            vec![],
        ),
        (
            "if (!(o is not int u)) {\n        let x = u;\n    }",
            vec![],
        ),
        (
            "if (o is int a or int b) {\n        let x = a;\n    }",
            read("a"),
        ),
        (
            "if (o is int a or int b) {\n        let x = b;\n    }",
            read("b"),
        ),
        (
            "if (o is int a and > 0) {\n        let x = a;\n    }",
            vec![],
        ),
        (
            "if (o is not (int a and > 0)) {\n    } else {\n        let x = a;\n    }",
            vec![],
        ),
        (
            "if (p is (not int a, let y)) {\n        let x = a + y;\n    }",
            read("a"),
        ),
        (
            "if (p is (> 0, let y)) {\n    } else {\n        let x = y;\n    }",
            read("y"),
        ),
    ] {
        let text = format!("F(o: int, p: (int, int)) {{\n    {test}\n}}\n");
        assert_eq!(messages(&text), expected, "{test}");
    }
}

#[test]
fn a_capture_reads_the_local_where_the_lambda_is_declared() {
    let read = vec!["`c` is used before it is assigned".to_owned()];
    for (text, expected) in [
        ("var c: int;\nlet f = [move c] () => 0;\n", read.clone()),
        ("var c: int;\nlet f = [weak c] () => 0;\n", read.clone()),
        ("var c: int;\nlet f = [move c] () => c;\n", read.clone()),
        ("var c: int;\nc = 1;\nlet f = [move c] () => c;\n", vec![]),
        // Capturing a `let` does not assign it.
        ("let c = 1;\nlet f = [move c] () => 0;\n", vec![]),
        ("let c: int;\nlet f = [move c] () => 0;\nc = 1;\n", read),
    ] {
        assert_eq!(messages(text), expected, "{text}");
    }
}

#[test]
fn a_later_arm_sees_what_an_earlier_pattern_evaluated() {
    for (text, expected) in [
        (
            "var x: int;\nmatch (1) {\n    < (x = 1) => {}\n    _ => {\n        let y = x;\n    }\n}\n",
            vec![],
        ),
        (
            "let x: int;\nmatch (1) {\n    < (x = 1) => {}\n    _ => {\n        x = 2;\n    }\n}\n",
            vec!["`x` is declared with `let` and may be assigned already"],
        ),
        // The last arm's pattern failing is for exhaustiveness (M3.4).
        (
            "F(o: int): int {\n    match (o) {\n        0 => {\n            return 1;\n        }\n    }\n}\n",
            vec![],
        ),
    ] {
        assert_eq!(messages(text), expected, "{text}");
    }
}

#[test]
fn a_recovered_return_reports_only_the_parse_error() {
    assert_eq!(
        reported("F(): int {\n    return\n}\n"),
        Vec::<String>::new()
    );
}
