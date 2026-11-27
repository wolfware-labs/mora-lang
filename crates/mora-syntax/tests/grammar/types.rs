//! Types: weak, splice, tuple, array and function types, and where a
//! member's type ends before its body (§5, §5.8).

use mora_syntax::SyntaxKind as K;

use crate::support::{assert_clean_with, has, messages, outline};

// types::member_ty: after a member's name or a function's parameters, `=>`
// starts the body, so a function type there needs parentheses.

#[test]
fn a_tuple_typed_member_keeps_its_body() {
    let (tree, errors) =
        outline("class A { Pair(): (int, int) => (1, 2); Two: (int, int) => (1, 2); }");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!tree.contains("FnType"), "{tree}");
    assert!(tree.contains("GetterDecl"), "{tree}");
}

#[test]
fn a_parenthesized_function_type_is_a_field() {
    let (tree, errors) = outline("class A { OnTick: ((int) => ()); }");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(tree.contains("FieldDecl"), "{tree}");
    assert!(tree.contains("FnType"), "{tree}");
}

#[test]
fn a_parameter_takes_a_bare_function_type() {
    let (tree, errors) = outline("Sort(compare: (int, int) => int, done: () => ()) { }");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(tree.matches("FnType").count(), 2, "{tree}");
}

/// A getter of type `ty` parses without errors: the member's shape
/// needs `scan_type` to find the end of the type.
fn assert_getter_type(ty: &str) -> String {
    let text = format!("class A {{ P: {ty} => x; }}");
    assert_clean_with(&text, K::GetterDecl);
    text
}

// `ty`: `weak T` is a type of its own.

#[test]
fn a_weak_field_type_parses() {
    let text = "class A { Parent: weak Node?; }";
    assert_clean_with(text, K::WeakType);
    assert!(has(text, K::FieldDecl));
}

// `scan_type`: a splice type, a tuple type, a nested or array type before
// `=>` makes a getter.

#[test]
fn a_spread_splice_type_makes_a_getter() {
    assert_getter_type("$..ts");
}

#[test]
fn a_parenthesized_splice_type_makes_a_getter() {
    assert_getter_type("$(T)");
    assert_getter_type("$(f())");
}

#[test]
fn an_unclosed_splice_type_ends_at_the_end_of_the_file() {
    assert_eq!(
        messages("F() { $(a"),
        ["expected `)`", "expected `;`", "expected `}`"]
    );
}

#[test]
fn a_weak_or_unit_type_makes_a_getter() {
    let text = assert_getter_type("weak Node?");
    assert!(has(&text, K::WeakType));
    assert_getter_type("()");
}

#[test]
fn a_tuple_type_makes_a_getter() {
    assert_getter_type("(a: int, b: string)");
    assert_getter_type("(int, string)");
    assert_getter_type("(int, string,)");
}

#[test]
fn a_nested_generic_type_makes_a_getter() {
    assert_getter_type("Outer<int>.Inner");
}

#[test]
fn an_array_type_makes_a_getter() {
    let text = assert_getter_type("int[]");
    assert!(has(&text, K::ArrayType));
}

#[test]
fn an_unclosed_index_is_not_an_array_type() {
    let text = "F() { T[0 x; }";
    assert!(!has(text, K::LocalDeclStmt));
    assert!(has(text, K::IndexExpr));
    assert_eq!(messages(text), ["expected `]`", "expected `;`"]);
}

#[test]
fn an_unclosed_array_suffix_ends_the_member_type() {
    // The type of `P` is `int`, and the `=>` inside the brackets does not
    // make a getter of it.
    let text = "class A { P: int[x => 0; }";
    assert!(has(text, K::FieldDecl));
    assert!(!has(text, K::GetterDecl));
}

// `scan_uses`: a `uses` bound inside a member's function type.

#[test]
fn a_getter_of_a_bounded_function_type_parses() {
    for text in [
        "class A { F: ((int) => int uses none) => G; }",
        "class A { F: ((int) => int uses fs.read) => G; }",
        "class A { F: ((int) => int uses (clock, env)) => G; }",
    ] {
        assert_clean_with(text, K::GetterDecl);
        assert!(has(text, K::UsesClause), "{text:?}");
    }
    assert_clean_with("class A { F: ((int) => int uses none); }", K::FieldDecl);
    // A bound without parentheses ends at a comma.
    assert_clean_with(
        "class A { F: ((int) => int uses clock, int) => G; }",
        K::GetterDecl,
    );
}

// `at_any`: `any` starts a boxed interface type only before a name.

#[test]
fn any_alone_is_a_type_name() {
    for text in ["struct S { X: any; }", "struct S { X: List<any>; }"] {
        assert_clean_with(text, K::PathType);
        assert!(!has(text, K::AnyType), "{text}");
    }
    assert_clean_with("struct S { X: any IShape; }", K::AnyType);
}
