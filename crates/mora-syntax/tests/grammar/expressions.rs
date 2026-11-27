//! Expressions: operators and precedence (§7.1), propagation (§7.4),
//! casts, lambdas, object and array creation, quotes and splices,
//! interpolated strings (§3.5) and generic arguments (§14.4).

use mora_syntax::SyntaxKind as K;
use mora_syntax::ast::{self, AstNode};
use mora_syntax::parse;

use crate::support::{messages, node, outline, shape, shape_marking_generics, top_level_kinds};

#[test]
fn question_marks_follow_section_7_4() {
    assert_eq!(
        shape_marking_generics("a ? b : c"),
        "ConditionalExpr(P, P, P)"
    );
    assert_eq!(shape_marking_generics("x?"), "PropagateExpr(P)");
    assert_eq!(shape_marking_generics("x?.y"), "MemberExpr(P, NameRef)");
    assert_eq!(
        shape_marking_generics("await f()?"),
        "PropagateExpr(AwaitExpr(CallExpr(P)))"
    );
    assert_eq!(
        shape_marking_generics("await f()"),
        "AwaitExpr(CallExpr(P))"
    );
    assert_eq!(
        shape_marking_generics("await -x"),
        "AwaitExpr(PrefixExpr(P))"
    );
    assert_eq!(
        shape_marking_generics("await !ready"),
        "AwaitExpr(PrefixExpr(P))"
    );
    assert_eq!(
        shape_marking_generics("await (int)value"),
        "AwaitExpr(CastExpr(PredefinedType, P))"
    );
    assert_eq!(
        shape_marking_generics("await -f()?"),
        "PropagateExpr(AwaitExpr(PrefixExpr(CallExpr(P))))"
    );
    assert_eq!(
        shape_marking_generics("stackalloc byte[256]"),
        "StackallocExpr(PredefinedType, LiteralExpr)"
    );
    assert_eq!(
        shape_marking_generics("(r?).Name"),
        "MemberExpr(ParenExpr(PropagateExpr(P)), NameRef)"
    );
    assert_eq!(
        shape_marking_generics("a ? b : c ? d : e"),
        "ConditionalExpr(P, P, ConditionalExpr(P, P, P))"
    );
    assert_eq!(
        shape_marking_generics("ok ? x = 1 : y = 2"),
        "ConditionalExpr(P, AssignExpr(P, LiteralExpr), AssignExpr(P, LiteralExpr))"
    );
    assert_eq!(
        shape_marking_generics("x = ok ? 1 : 2"),
        "AssignExpr(P, ConditionalExpr(P, LiteralExpr, LiteralExpr))"
    );
    assert_eq!(
        shape_marking_generics("x? ?? y"),
        "BinExpr(PropagateExpr(P), P)"
    );
    assert_eq!(
        shape_marking_generics("c ? a?[0] : b"),
        "ConditionalExpr(P, IndexExpr(P, Arg(LiteralExpr)), P)"
    );
    assert_eq!(
        shape_marking_generics("c ? r? : d"),
        "ConditionalExpr(P, PropagateExpr(P), P)"
    );
    assert_eq!(shape_marking_generics("f(x?, y)"), "CallExpr(P)");
    assert_eq!(
        shape_marking_generics("flag ? [1] : []"),
        "ConditionalExpr(P, CollectionExpr(CollectionElem(LiteralExpr)), CollectionExpr)"
    );
}

#[test]
fn generic_arguments_follow_section_14_4() {
    assert_eq!(shape_marking_generics("F<A, B>(c)"), "CallExpr(P<>)");
    assert_eq!(
        shape_marking_generics("(F < A, B > c)"),
        "TupleExpr(TupleElem(BinExpr(P, P)), TupleElem(BinExpr(P, P)))"
    );
    assert_eq!(shape_marking_generics("a < b"), "BinExpr(P, P)");
    assert_eq!(
        shape_marking_generics("List<int>.Empty"),
        "MemberExpr(P<>, NameRef)"
    );
}

#[test]
fn casts_need_an_operand_after_the_parentheses() {
    assert_eq!(
        shape_marking_generics("(int)x"),
        "CastExpr(PredefinedType(), P)".replace("()", "")
    );
    assert_eq!(
        shape_marking_generics("(int)-x"),
        "CastExpr(PredefinedType, PrefixExpr(P))"
    );
    assert_eq!(
        shape_marking_generics("(T)x"),
        "CastExpr(PathType(Path(PathSegment(NameRef))), P)"
    );
    assert_eq!(
        shape_marking_generics("(a) - b"),
        "BinExpr(ParenExpr(P), P)"
    );
    assert_eq!(shape_marking_generics("(a)"), "ParenExpr(P)");
}

#[test]
fn precedence_follows_section_7_1() {
    assert_eq!(
        shape_marking_generics("a + b * c"),
        "BinExpr(P, BinExpr(P, P))"
    );
    assert_eq!(
        shape_marking_generics("a * b + c"),
        "BinExpr(BinExpr(P, P), P)"
    );
    assert_eq!(
        shape_marking_generics("-a * b"),
        "BinExpr(PrefixExpr(P), P)"
    );
    assert_eq!(shape_marking_generics("a..b"), "RangeExpr(P, P)");
    assert_eq!(
        shape_marking_generics("a + b..c"),
        "BinExpr(P, RangeExpr(P, P))"
    );
    assert_eq!(
        shape_marking_generics("a * match (b) { _ => 0 }"),
        "BinExpr(P, MatchExpr(P, MatchArmList(MatchArm(DiscardPattern, LiteralExpr))))"
    );
    assert_eq!(
        shape_marking_generics("a == b is int"),
        "BinExpr(P, IsExpr(P, TypePattern(PredefinedType)))"
    );
    assert_eq!(
        shape_marking_generics("a & b ^ c | d"),
        "BinExpr(BinExpr(BinExpr(P, P), P), P)"
    );
    assert_eq!(
        shape_marking_generics("a || b && c"),
        "BinExpr(P, BinExpr(P, P))"
    );
    assert_eq!(
        shape_marking_generics("a ?? b ?? c"),
        "BinExpr(P, BinExpr(P, P))"
    );
    assert_eq!(
        shape_marking_generics("a ?? b ? c : d"),
        "ConditionalExpr(BinExpr(P, P), P, P)"
    );
    assert_eq!(
        shape_marking_generics("x = y = z"),
        "AssignExpr(P, AssignExpr(P, P))"
    );
    assert_eq!(
        shape_marking_generics("x >>= 1"),
        "AssignExpr(P, LiteralExpr)"
    );
    assert_eq!(
        shape_marking_generics("a >> b > c"),
        "BinExpr(BinExpr(P, P), P)"
    );
}

#[test]
fn nothing_chains_after_propagation() {
    assert_eq!(
        shape_marking_generics("items?[flag ? 0 : 1]"),
        "IndexExpr(P, Arg(ConditionalExpr(P, LiteralExpr, LiteralExpr)))"
    );
    assert_eq!(
        shape_marking_generics("(T)stackalloc byte[1]"),
        "CastExpr(PathType(Path(PathSegment(NameRef))), StackallocExpr(PredefinedType, LiteralExpr))"
    );
    let errors: Vec<_> = parse("let _ = r? .Name;")
        .errors()
        .iter()
        .map(|e| e.message.clone())
        .collect();
    // `r?` ends the expression; `.Name` is then left over.
    assert_eq!(errors.first().map(String::as_str), Some("expected `;`"));
    // A leading `[` is always a collection; `@` starts an attribute.
    let stmts = |text: &str| -> Vec<String> {
        let parsed = parse(text);
        assert!(parsed.errors().is_empty(), "{text}: {:?}", parsed.errors());
        top_level_kinds(text)
    };
    assert_eq!(stmts("[x][0];"), ["ExprStmt"]);
    assert_eq!(stmts("[x](arg);"), ["ExprStmt"]);
    assert_eq!(stmts("[a] is [_];"), ["ExprStmt"]);
    assert_eq!(stmts("[a] < b;"), ["ExprStmt"]);
    assert_eq!(stmts("[a] with { };"), ["ExprStmt"]);
    assert_eq!(stmts("[Inline];"), ["ExprStmt"]);
    assert_eq!(stmts("@Cfg(Debug = true)\nLog(1);"), ["AttributedStmt"]);
    assert_eq!(stmts("@Inline { }"), ["AttributedStmt"]);
    assert_eq!(
        stmts("@Cfg(Debug = true)\nlet x: int = 1;"),
        ["AttributedStmt"]
    );
    assert_eq!(
        stmts("@Inline\nTwice(x: int): int => x * 2;"),
        ["FunctionDecl"]
    );
    assert_eq!(stmts("@A\n@B\nstatic X: int;"), ["FieldDecl"]);
}

// expr_bp: a range does not associate, on either side of its `..`.

#[test]
fn a_range_takes_one_dot_dot() {
    assert_eq!(messages("let _ = ..a..b;"), ["expected `;`"]);
    assert_eq!(messages("let _ = a..b..c;"), ["expected `;`"]);
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
    assert_eq!(messages("let _ = (a"), ["expected `)`", "expected `;`"]);
}

// lambda: capture lists.

#[test]
fn a_capture_list_takes_every_capture() {
    assert_eq!(
        shape("[weak a, move b] () => 0"),
        "LambdaExpr(CaptureList(Capture(NameRef), Capture(NameRef)), LambdaParamList, LiteralExpr)"
    );
    assert_eq!(
        messages("let f = [weak a,"),
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
    assert_eq!(messages("let t = code(any IShape);"), Vec::<String>::new());
}

// array_size and new_expr: an array is created with a size, never with
// listed elements.

#[test]
fn arrays_are_created_with_a_size() {
    assert_eq!(
        shape("new int[n + 1]"),
        "NewArrayExpr(PredefinedType, BinExpr(P, LiteralExpr))"
    );
    assert_eq!(messages("let a = new int[4];"), Vec::<String>::new());
    assert_eq!(
        messages("let a = stackalloc byte[4];"),
        Vec::<String>::new()
    );
    for text in [
        "let a = new int[] { 1 };",
        "let a = new[] { 1 };",
        "let a = new int[2] { 1, 2 };",
        "let a = stackalloc int[] { 1 };",
    ] {
        assert!(!messages(text).is_empty(), "{text}");
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
    assert_eq!(messages("let _ = a[1,"), ["expected `]`", "expected `;`"]);
}

// match_head: a match expression may have no arms.

#[test]
fn a_match_expression_may_be_empty() {
    assert_eq!(shape("match (x) { }"), "MatchExpr(P, MatchArmList)");
}

// arg_inner: `out` is not a parameter mode.

#[test]
fn out_is_not_an_argument_mode() {
    assert!(!messages("g(f(out a));").is_empty());
    assert_eq!(messages("g(f(ref a));"), Vec::<String>::new());
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

// `child_between`: the parts of `a ? b : c` are found across whitespace.

#[test]
fn conditional_parts_skip_whitespace() {
    let parsed = parse("var x = a ? b : c;");
    let cond = parsed
        .syntax()
        .descendants()
        .find_map(ast::ConditionalExpr::cast)
        .unwrap();
    assert_eq!(cond.condition().unwrap().syntax().text(), "a");
    assert_eq!(cond.then().unwrap().syntax().text(), "b");
    assert_eq!(cond.r#else().unwrap().syntax().text(), "c");
}

// A `:` at the top level of a hole starts its format (§3.5), so `::`
// there is format text.

#[test]
fn a_double_colon_in_a_hole_is_format_text() {
    let text = r#"let s = $"{Core::Parse(1)}";"#;
    assert_eq!(messages(text), Vec::<String>::new());
    assert_eq!(node(text, K::InterpHole).text(), "{Core::Parse(1)}");
}
