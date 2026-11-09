// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests that pin parser behavior found untested by `cargo mutants` (§20.5).

use mora_syntax::{SyntaxKind, SyntaxNode, parse};

/// A node as nested node kinds, without tokens: `BinExpr(PathExpr, LiteralExpr)`.
/// Paths in expressions are written `P`.
fn render(node: &SyntaxNode) -> String {
    let kind = format!("{:?}", node.kind());
    if node.kind() == SyntaxKind::PathExpr {
        return "P".into();
    }
    let children: Vec<String> = node
        .children()
        .filter(|c| c.kind() != SyntaxKind::ArgList)
        .map(|c| render(&c))
        .collect();
    if children.is_empty() {
        kind
    } else {
        format!("{kind}({})", children.join(", "))
    }
}

/// The expression of `let _ = <text>;`, which must parse without errors.
fn shape(text: &str) -> String {
    let source = format!("let _ = {text};");
    let parse = parse(&source);
    assert!(parse.errors().is_empty(), "{text}: {:?}", parse.errors());
    let declarator = parse
        .syntax()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::VariableDeclarator)
        .unwrap();
    render(&declarator.children().nth(1).unwrap())
}

/// The pattern of `x is <text>`, which must parse without errors.
fn pattern(text: &str) -> String {
    let s = shape(&format!("x is {text}"));
    s.strip_prefix("IsExpr(P, ")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap()
        .to_owned()
}

/// The whole file as nested node kinds, and the messages of its errors.
fn outline(text: &str) -> (String, Vec<String>) {
    let parse = parse(text);
    assert_eq!(parse.syntax().text(), text, "not lossless");
    let errors = parse.errors().iter().map(|e| e.message.clone()).collect();
    (render(&parse.syntax()), errors)
}

/// The messages of the errors in `text`.
fn errors(text: &str) -> Vec<String> {
    outline(text).1
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

// at_expr_start: a predefined type starts an expression statement.

#[test]
fn a_statement_can_start_with_a_predefined_type() {
    assert_eq!(
        outline("int.Parse(s);").0,
        "SourceFile(ExprStmt(CallExpr(MemberExpr(PredefinedTypeExpr(PredefinedType), NameRef))))"
    );
    assert_eq!(errors("F() { return int.MaxValue; }"), Vec::<String>::new());
}

// expr_bp: a range does not associate, on either side of its `..`.

#[test]
fn a_range_takes_one_dot_dot() {
    assert_eq!(errors("let _ = ..a..b;"), ["expected `;`"]);
    assert_eq!(errors("let _ = a..b..c;"), ["expected `;`"]);
    assert_eq!(shape("..a"), "RangeExpr(P)");
    assert_eq!(shape("a.."), "RangeExpr(P)");
}

// conditional_follows: `?` is a conditional only when a `:` follows at
// the same depth before the expression ends.

#[test]
fn a_conditional_needs_a_colon_at_its_own_depth() {
    assert_eq!(
        shape("f() ? 1 : 2"),
        "ConditionalExpr(CallExpr(P), LiteralExpr, LiteralExpr)"
    );
    assert_eq!(
        shape("c ? f(a, b) : d"),
        "ConditionalExpr(P, CallExpr(P), P)"
    );
    // A `;` ends the first expression; the `:` after it is another's.
    assert_eq!(
        outline("let a = r? + 1; let b = c ? 1 : 2;").0,
        "SourceFile(LocalDeclStmt(LocalDecl(VariableDeclarator(Name, BinExpr(PropagateExpr(P), LiteralExpr)))), LocalDeclStmt(LocalDecl(VariableDeclarator(Name, ConditionalExpr(P, LiteralExpr, LiteralExpr)))))"
    );
    // A `:` inside parentheses belongs to them.
    assert_eq!(
        shape("r? + g(n: 1)"),
        "BinExpr(PropagateExpr(P), CallExpr(P))"
    );
}

// at_cast: `(int)` is a cast only when an operand follows.

#[test]
fn a_predefined_type_in_parentheses_casts_only_an_operand() {
    assert_eq!(
        shape("(int)"),
        "ParenExpr(PredefinedTypeExpr(PredefinedType))"
    );
    assert_eq!(outline("let _ = (int)").1, ["expected `;`"]);
    // A cast can open the file.
    assert_eq!(
        outline("(int)x + 1;"),
        (
            "SourceFile(ExprStmt(BinExpr(CastExpr(PredefinedType, P), LiteralExpr)))".into(),
            vec![]
        )
    );
}

// at_lambda: an unclosed `(` at the end of the file is not a lambda.

#[test]
fn an_unclosed_parenthesis_at_the_end_is_not_a_lambda() {
    assert_eq!(errors("let _ = (a"), ["expected `)`", "expected `;`"]);
}

// lambda: capture lists.

#[test]
fn a_capture_list_takes_every_capture() {
    assert_eq!(
        shape("[weak a, move b] () => 0"),
        "LambdaExpr(CaptureList(Capture(NameRef), Capture(NameRef)), LambdaParamList, LiteralExpr)"
    );
    assert_eq!(
        errors("let f = [weak a,"),
        [
            "expected `]`",
            "expected an identifier",
            "expected `=>`",
            "expected an expression",
            "expected `;`"
        ]
    );
}

// primary: a splice is an expression.

#[test]
fn a_splice_is_an_expression() {
    assert_eq!(shape("$x"), "SpliceExpr(Splice(NameRef))");
}

// quote: what `code(...)` holds is a type only when it cannot be an expression.

#[test]
fn a_quote_holds_a_type_only_when_it_cannot_be_an_expression() {
    assert_eq!(shape("code(int)"), "QuoteExpr(PredefinedType)");
    assert_eq!(shape("code(x)"), "QuoteExpr(P)");
    assert_eq!(
        shape("code(int.MaxValue)"),
        "QuoteExpr(MemberExpr(PredefinedTypeExpr(PredefinedType), NameRef))"
    );
    assert_eq!(
        shape("code(any IShape)"),
        "QuoteExpr(AnyType(PathType(Path(PathSegment(NameRef)))))"
    );
    assert_eq!(errors("let t = code(any IShape);"), Vec::<String>::new());
}

// array_size and new_expr: an array is created with a size, never with
// listed elements.

#[test]
fn arrays_are_created_with_a_size() {
    assert_eq!(
        shape("new int[n + 1]"),
        "NewArrayExpr(PredefinedType, BinExpr(P, LiteralExpr))"
    );
    assert_eq!(errors("let a = new int[4];"), Vec::<String>::new());
    assert_eq!(errors("let a = stackalloc byte[4];"), Vec::<String>::new());
    for text in [
        "let a = new int[] { 1 };",
        "let a = new[] { 1 };",
        "let a = new int[2] { 1, 2 };",
        "let a = stackalloc int[] { 1 };",
    ] {
        assert!(!errors(text).is_empty(), "{text}");
    }
    // The empty brackets are the missing size, not an array element type.
    let (tree, _) = outline("let a = new int[] { 1 };");
    assert!(tree.contains("NewArrayExpr(PredefinedType"), "{tree}");
    assert!(!tree.contains("ArrayType"), "{tree}");
}

// object_initializer: every member.

#[test]
fn an_object_initializer_takes_every_member() {
    assert_eq!(
        shape("new P { A = 1, B = 2 }"),
        "NewExpr(PathType(Path(PathSegment(NameRef))), ObjectInitializer(MemberInit(NameRef, LiteralExpr), MemberInit(NameRef, LiteralExpr)))"
    );
}

// postfix: an index stops at the end of the file.

#[test]
fn an_unclosed_index_stops_at_the_end() {
    assert_eq!(errors("let _ = a[1,"), ["expected `]`", "expected `;`"]);
}

// match_head: a match expression may have no arms.

#[test]
fn a_match_expression_may_be_empty() {
    assert_eq!(shape("match (x) { }"), "MatchExpr(P, MatchArmList)");
}

// arg_inner: `out` is not a parameter mode.

#[test]
fn out_is_not_an_argument_mode() {
    assert!(!errors("g(f(out a));").is_empty());
    assert_eq!(errors("g(f(ref a));"), Vec::<String>::new());
}

// interp_string: an unclosed hole skips to its `}`.

#[test]
fn an_unclosed_hole_skips_to_its_brace() {
    assert_eq!(
        outline("let s = $\"{x y}\"; let t = 1;"),
        (
            "SourceFile(LocalDeclStmt(LocalDecl(VariableDeclarator(Name, InterpString(InterpHole(P))))), LocalDeclStmt(LocalDecl(VariableDeclarator(Name, LiteralExpr))))".into(),
            vec!["expected the `}` that closes the hole".into()]
        )
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
        errors("let (a,"),
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
    assert_eq!(errors("let _ = x is (1"), ["expected `)`", "expected `;`"]);
}

// list_pattern: may be empty.

#[test]
fn a_list_pattern_may_be_empty() {
    assert_eq!(pattern("[]"), "ListPattern");
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
    assert_eq!(errors("F() { return }"), ["expected `;`"]);
}

// match_head: one match with arms, as a statement and as an expression.

#[test]
fn match_statements_take_arms() {
    assert_eq!(errors("match (x) { }"), Vec::<String>::new());
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
    assert_eq!(errors("match (x) { 1 => F() _ => G() }")[0], "expected `}`");
}

#[test]
fn a_match_expression_is_a_primary() {
    assert_eq!(
        shape("match (x) { _ => 1 } + 2"),
        "BinExpr(MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, LiteralExpr))), LiteralExpr)"
    );
}

// types::arrow: `->` is reported with a fix to `=>`, then read as `=>`.

#[test]
fn a_thin_arrow_gets_a_fix_to_the_fat_arrow() {
    for text in [
        "let f: (int) -> int = g;",
        "let f = x -> x + 1;",
        "let f = (x: int) -> x;",
    ] {
        let parsed = parse(text);
        assert_eq!(parsed.errors().len(), 1, "{text}");
        let error = &parsed.errors()[0];
        assert_eq!(error.message, "expected `=>`", "{text}");
        assert_eq!(&text[error.range], "->", "{text}");
        let fixed = error.fix.as_ref().unwrap().apply(text);
        assert_eq!(fixed, text.replace("->", "=>"));
        assert!(parse(&fixed).errors().is_empty(), "{fixed}");
    }
}

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

// scan_type_no_fn: a cast never takes a lambda for a function type.

#[test]
fn a_parenthesized_lambda_is_not_a_cast() {
    assert_eq!(
        shape("((x) => x)(5)"),
        "CallExpr(ParenExpr(LambdaExpr(LambdaParamList(LambdaParam(Name)), P)))"
    );
}

// scan_type: a named tuple element skips its name only before `:`.

#[test]
fn a_parenthesized_type_argument_opens_generic_arguments() {
    // A comparison chain would be `BinExpr(BinExpr(P, ParenExpr(P)), ParenExpr(P))`.
    assert_eq!(shape("F<(A)>(x)"), "CallExpr(P)");
}

// types::arrow in declaration bodies and match arms: `->` gets the same
// fix to `=>` there.

#[test]
fn a_thin_arrow_body_or_arm_gets_the_fix() {
    for text in [
        "F() -> 1;",
        "Twice(x: int): int -> x * 2;",
        "class A { Count: int -> 0; Get(): int -> 1; }",
        "let y = match (x) { _ -> 1 };",
        "match (x) { 0 -> F(), _ -> { G(); } }",
    ] {
        let parsed = parse(text);
        let arrows = text.matches("->").count();
        assert_eq!(
            parsed.errors().len(),
            arrows,
            "{text}: {:?}",
            parsed.errors()
        );
        let mut fixed = text.to_owned();
        for error in parsed.errors().iter().rev() {
            assert_eq!(error.message, "expected `=>`", "{text}");
            assert_eq!(&text[error.range], "->", "{text}");
            fixed = error.fix.as_ref().unwrap().apply(&fixed);
        }
        assert_eq!(fixed, text.replace("->", "=>"));
        assert!(parse(&fixed).errors().is_empty(), "{fixed}");
    }
}
