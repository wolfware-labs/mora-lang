//! Patterns after `is` and in match arms (§7.8).

use mora_syntax::SyntaxKind as K;

use crate::support::{assert_clean_with, has, messages, outline, shape, shape_marking_generics};

#[test]
fn patterns_follow_section_7_8() {
    let pattern = |text: &str| {
        let s = shape_marking_generics(&format!("x is {text}"));
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
        shape_marking_generics("match (s) { None => 0, ok => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(ConstantPattern(P), LiteralExpr), MatchArm(ConstantPattern(P), LiteralExpr)))"
    );
    assert_eq!(
        shape_marking_generics("match (n) { _ when ok => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, WhenClause(P), LiteralExpr)))"
    );
    assert_eq!(
        shape_marking_generics("match (n) { _ when xs.Any(x => x > 0) => 1 }"),
        "MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, WhenClause(CallExpr(MemberExpr(P, NameRef))), LiteralExpr)))"
    );
}

/// The pattern of `x is <text>`, which must parse without errors.
fn pattern(text: &str) -> String {
    let s = shape(&format!("x is {text}"));
    s.strip_prefix("IsExpr(P, ")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap()
        .to_owned()
}

// pattern_operand: a constant pattern stops before `as`, `<` and `==`.

#[test]
fn a_constant_pattern_stops_before_relational_operators() {
    assert_eq!(
        shape("x is 1 as T"),
        "AsExpr(IsExpr(P, ConstantPattern(LiteralExpr)), PathType(Path(PathSegment(NameRef))))"
    );
    assert_eq!(
        shape("x is 1 == y"),
        "BinExpr(IsExpr(P, ConstantPattern(LiteralExpr)), P)"
    );
}

// primary_pattern and is_pattern_word: `and`, `or` and `when` after a type
// continue the pattern rather than name a binding.

#[test]
fn a_pattern_word_after_a_type_is_not_a_binding() {
    assert_eq!(
        outline("x is A.B and C;").0,
        "SourceFile(ExprStmt(IsExpr(P, BinPattern(ConstantPattern(MemberExpr(P, NameRef)), ConstantPattern(P)))))"
    );
    assert_eq!(
        pattern("A or B"),
        "BinPattern(ConstantPattern(P), ConstantPattern(P))"
    );
}

// is_plain_path: a case pattern starts with a plain path.

#[test]
fn a_generic_type_before_parentheses_is_not_a_case_pattern() {
    assert_eq!(
        outline("x is List<int>(a);"),
        (
            "SourceFile(ExprStmt(IsExpr(P, TypePattern(PathType(Path(PathSegment(NameRef, GenericArgList(PredefinedType))))))), ExprStmt(ParenExpr(P)))".into(),
            vec!["expected `;`".into()]
        )
    );
}

// tuple_designation: stops at the end of the file.

#[test]
fn an_unclosed_tuple_designation_stops_at_the_end() {
    assert_eq!(
        messages("let (a,"),
        [
            "expected `)`",
            "expected `=`",
            "expected an expression",
            "expected `;`"
        ]
    );
}

// paren_or_positional and single_pattern_in_parens.

#[test]
fn parentheses_hold_one_pattern_or_fields() {
    assert_eq!(pattern("(A)"), "ParenPattern(ConstantPattern(P))");
    assert_eq!(
        pattern("(X: 0)"),
        "PositionalPattern(PatternField(NameRef, ConstantPattern(LiteralExpr)))"
    );
    assert_eq!(
        pattern("([1, 2])"),
        "ParenPattern(ListPattern(ConstantPattern(LiteralExpr), ConstantPattern(LiteralExpr)))"
    );
    assert_eq!(pattern("()"), "PositionalPattern");
    assert_eq!(
        messages("let _ = x is (1"),
        ["expected `)`", "expected `;`"]
    );
}

// list_pattern: may be empty.

#[test]
fn a_list_pattern_may_be_empty() {
    assert_eq!(pattern("[]"), "ListPattern");
}

// `nth_text`: `and` after a type in a pattern combines patterns.

#[test]
fn and_after_a_type_pattern_combines_patterns() {
    let text = "let b: bool = x is Foo and Bar;";
    assert_clean_with(text, K::BinPattern);
    assert!(!has(text, K::DeclarationPattern));
}
