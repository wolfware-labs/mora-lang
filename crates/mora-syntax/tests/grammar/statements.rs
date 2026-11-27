//! Statements: what starts one, `match` statements and their arms, and
//! `match` and `switch` as identifiers (§7.7, §7.11, §14.4).

use mora_syntax::SyntaxKind as K;

use crate::support::{assert_clean_with, has, messages, outline, shape};

// at_expr_start: a predefined type starts an expression statement.

#[test]
fn a_statement_can_start_with_a_predefined_type() {
    assert_eq!(
        outline("int.Parse(s);").0,
        "SourceFile(ExprStmt(CallExpr(MemberExpr(PredefinedTypeExpr(PredefinedType), NameRef))))"
    );
    assert_eq!(
        messages("F() { return int.MaxValue; }"),
        Vec::<String>::new()
    );
}

// at_match: `match` starts a `match` only before a parenthesized
// scrutinee and `{` (§14.4); anywhere else it is an identifier.

#[test]
fn match_without_parentheses_is_an_identifier() {
    assert_eq!(
        outline("match x { }"),
        (
            "SourceFile(ExprStmt(P), ExprStmt(P), Block)".into(),
            vec!["expected `;`".into(), "expected `;`".into()]
        )
    );
}

#[test]
fn match_is_an_identifier_outside_the_construct() {
    assert_eq!(
        outline("match(x);"),
        ("SourceFile(ExprStmt(CallExpr(P)))".into(), vec![])
    );
    assert_eq!(
        outline("let match = F(); match = match + 1;"),
        (
            "SourceFile(LocalDeclStmt(LocalDecl(VariableDeclarator(Name, CallExpr(P)))), ExprStmt(AssignExpr(P, BinExpr(P, LiteralExpr))))".into(),
            vec![]
        )
    );
    assert_eq!(shape("match"), "P");
    assert_eq!(shape("match(x)"), "CallExpr(P)");
    assert_eq!(shape("match(x).match"), "MemberExpr(CallExpr(P), NameRef)");
    assert_eq!(
        shape("match((a), b) + 1"),
        "BinExpr(CallExpr(P), LiteralExpr)"
    );
    assert_eq!(
        outline("class A { match: int; match(x: int) { } }"),
        (
            "SourceFile(ClassDecl(Name, MemberList(FieldDecl(VariableDeclarator(Name, PredefinedType)), FunctionDecl(Name, ParamList(Param(Name, PredefinedType)), Block))))".into(),
            vec![]
        )
    );
}

#[test]
fn balanced_parentheses_end_the_scrutinee() {
    assert_eq!(
        shape("match ((a) + F(b)) { _ => 1 }"),
        "MatchExpr(BinExpr(ParenExpr(P), CallExpr(P)), MatchArmList(MatchArm(DiscardPattern, LiteralExpr)))"
    );
    // Unclosed parentheses: a call that is not finished.
    assert_eq!(
        outline("match (x"),
        (
            "SourceFile(ExprStmt(CallExpr(P)))".into(),
            vec!["expected `)`".into(), "expected `;`".into()]
        )
    );
    assert_eq!(
        outline("match ((x) {").0,
        "SourceFile(ExprStmt(CallExpr(P)), Block)"
    );
}

#[test]
fn a_match_statement_is_not_a_function() {
    assert_eq!(
        outline("match (x) { _ => F(), }"),
        (
            "SourceFile(MatchStmt(P, MatchArmList(MatchArm(DiscardPattern, CallExpr(P)))))".into(),
            vec![]
        )
    );
    assert_eq!(
        outline("G() { match (x) { _ => F(), } }"),
        (
            "SourceFile(FunctionDecl(Name, ParamList, Block(MatchStmt(P, MatchArmList(MatchArm(DiscardPattern, CallExpr(P)))))))".into(),
            vec![]
        )
    );
    // With a return type, a function may be named `match`.
    assert_eq!(
        outline("match(x: int): () { }"),
        (
            "SourceFile(FunctionDecl(Name, ParamList(Param(Name, PredefinedType)), TupleType, Block))".into(),
            vec![]
        )
    );
    assert_eq!(
        outline("G() { match(x: int): () { } }"),
        (
            "SourceFile(FunctionDecl(Name, ParamList, Block(LocalFunctionStmt(FunctionDecl(Name, ParamList(Param(Name, PredefinedType)), TupleType, Block)))))".into(),
            vec![]
        )
    );
}

#[test]
fn an_attributed_match_is_a_statement() {
    assert_eq!(
        outline("@Cfg(Debug = true) match (x) { _ => F(), }"),
        ("SourceFile(AttributedStmt(Attribute(Path(PathSegment(NameRef)), AttributeArgList(AttributeArg(NameRef, LiteralExpr))), MatchStmt(P, MatchArmList(MatchArm(DiscardPattern, CallExpr(P))))))".into(), vec![])
    );
    assert_eq!(
        outline("G() { @Cfg(Debug = true) match (x) { _ => F(), } }"),
        (
            "SourceFile(FunctionDecl(Name, ParamList, Block(AttributedStmt(Attribute(Path(PathSegment(NameRef)), AttributeArgList(AttributeArg(NameRef, LiteralExpr))), MatchStmt(P, MatchArmList(MatchArm(DiscardPattern, CallExpr(P))))))))".into(),
            vec![]
        )
    );
}

#[test]
fn attributed_and_modified_functions_are_still_declarations() {
    assert_eq!(
        outline("@Cfg(Debug = true) F(x: int) { }"),
        (
            "SourceFile(FunctionDecl(Attribute(Path(PathSegment(NameRef)), AttributeArgList(AttributeArg(NameRef, LiteralExpr))), Name, ParamList(Param(Name, PredefinedType)), Block))".into(),
            vec![]
        )
    );
    assert_eq!(
        outline("@Cfg(Debug = true) match(x: int): () { }"),
        (
            "SourceFile(FunctionDecl(Attribute(Path(PathSegment(NameRef)), AttributeArgList(AttributeArg(NameRef, LiteralExpr))), Name, ParamList(Param(Name, PredefinedType)), TupleType, Block))".into(),
            vec![]
        )
    );
    // A modifier makes it a declaration, as for any other name.
    assert_eq!(
        outline("static match(x: int) { }"),
        (
            "SourceFile(FunctionDecl(Modifier, Name, ParamList(Param(Name, PredefinedType)), Block))".into(),
            vec![]
        )
    );
}

// D-129: `switch` is an ordinary identifier.

#[test]
fn switch_is_an_identifier() {
    assert_eq!(
        outline("let switch = 1;"),
        (
            "SourceFile(LocalDeclStmt(LocalDecl(VariableDeclarator(Name, LiteralExpr))))".into(),
            vec![]
        )
    );
    assert_eq!(
        outline("switch(x);"),
        ("SourceFile(ExprStmt(CallExpr(P)))".into(), vec![])
    );
    // With arms it is a function named `switch`, with the ordinary errors.
    assert_eq!(
        outline("switch (x) { _ => 1 }"),
        (
            "SourceFile(FunctionDecl(Name, ParamList(Param(Name)), Block(ExprStmt(LambdaExpr(LambdaParam(Name), LiteralExpr)))))".into(),
            vec!["expected `:` and a type".into(), "expected `;`".into()]
        )
    );
}

// return_stmt: a missing `;` is the only error.

#[test]
fn return_without_a_semicolon_reports_only_the_semicolon() {
    assert_eq!(messages("F() { return }"), ["expected `;`"]);
}

// match_head: one match with arms, as a statement and as an expression.

#[test]
fn match_statements_take_arms() {
    assert_eq!(messages("match (x) { }"), Vec::<String>::new());
    assert_eq!(
        outline("match (x) { 1 => F(), _ => G(), }").0,
        "SourceFile(MatchStmt(P, MatchArmList(MatchArm(ConstantPattern(LiteralExpr), CallExpr(P)), MatchArm(DiscardPattern, CallExpr(P)))))"
    );
}

#[test]
fn a_block_arm_needs_no_comma() {
    assert_eq!(
        outline("match (x) { 1 => { F(); } _ => G() }"),
        (
            "SourceFile(MatchStmt(P, MatchArmList(MatchBlockArm(ConstantPattern(LiteralExpr), Block(ExprStmt(CallExpr(P)))), MatchArm(DiscardPattern, CallExpr(P)))))".into(),
            vec![]
        )
    );
    // After an expression arm, a missing comma ends the arms.
    assert_eq!(
        messages("match (x) { 1 => F() _ => G() }")[0],
        "expected `}`"
    );
}

#[test]
fn a_match_expression_is_a_primary() {
    assert_eq!(
        shape("match (x) { _ => 1 } + 2"),
        "BinExpr(MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, LiteralExpr))), LiteralExpr)"
    );
}

// `statement_inner`: `unsafe` starts a statement only before a block.

#[test]
fn unsafe_takes_a_block() {
    assert_clean_with("F() { unsafe { Free(p); } }", K::UnsafeStmt);
    let text = "F() { unsafe x; }";
    assert!(!has(text, K::UnsafeStmt));
    assert!(!messages(text).is_empty());
}
