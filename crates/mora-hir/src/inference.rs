//! The types of a body's expressions and locals (§5.10, §7, §14.5).
//!
//! [`infer`] checks one body bidirectionally: a declared type, a parameter
//! type, a return type or a condition is the expected type of the
//! expression it applies to, and flows into literals, tuples and `c ? a : b`;
//! every other expression gets its type from its parts. A value converts to
//! the type expected of it only as §5.10 allows; until the standard library
//! brings `IFrom`, the numeric rows of §5.10 are a table in the checker
//! (D-135).
//!
//! What a later milestone gives meaning to (calls, lambdas, `new`, `match`,
//! patterns, collections, options, names of the standard library, and the
//! types of [`Ty::Unsupported`]) has no type yet: [`Inference::expr`] gives
//! `None` for it. An expression of unknown type, or of [`Ty::Error`], is
//! never the cause of a diagnostic, so nothing is reported twice and
//! nothing is reported about code this milestone does not check.

mod check;
mod constant;
mod diagnostic;
mod numeric;

use mora_span::{Db, Project, SourceFile};

use crate::arena::arena_index;
use crate::bodies::{BinaryOp, BodyOwner, ExprId, LocalId, UnaryOp};
use crate::resolve::item_data;
use crate::ty::{Primitive, Ty};

pub(crate) use diagnostic::{Source, type_error};

/// The types of a body's expressions and locals, and what is wrong with
/// them. It holds no text range, so an edit in another body leaves it
/// equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inference {
    /// The type of each expression, indexed like [`crate::Body::exprs`].
    exprs: Vec<Option<Ty>>,
    /// The type of each local, indexed like [`crate::Body::locals`].
    locals: Vec<Option<Ty>>,
    /// The type errors, in the order they were found.
    pub(crate) errors: Vec<TypeError>,
}

impl Inference {
    /// The type of `expr`: `None` while it is unknown, because a later
    /// milestone gives it meaning, and [`Ty::Error`] when a diagnostic says
    /// why it has none.
    pub fn expr(&self, expr: ExprId) -> Option<&Ty> {
        self.exprs.get(arena_index(expr))?.as_ref()
    }

    /// The type of `local`: the declared one, or that of its initializer.
    /// `None` while it is unknown, as for [`Inference::expr`].
    pub fn local(&self, local: LocalId) -> Option<&Ty> {
        self.locals.get(arena_index(local))?.as_ref()
    }
}

/// A type error in a body, by the expression it is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeError {
    /// `expr` is of type `found` where `expected` is expected, and `found`
    /// does not convert to it (`MR0203`).
    Mismatch {
        expr: ExprId,
        expected: Ty,
        found: Ty,
        context: Context,
    },
    /// The literal `expr`, a negated literal or an expression made only of
    /// literals, does not fit `ty`, or no integer type when `ty` is `None`
    /// (`MR0010`).
    Literal { expr: ExprId, ty: Option<Primitive> },
    /// The binary operator of `expr`, an operation or a compound
    /// assignment, is not defined for its operands `lhs` and `rhs`, of
    /// types `lhs_ty` and `rhs_ty` (`MR0204`).
    Binary {
        expr: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty,
        rhs_ty: Ty,
    },
    /// The prefix or postfix operator of `expr` is not defined for its
    /// operand, of type `ty` (`MR0204`); `++` and `--` are the prefix forms.
    Unary {
        expr: ExprId,
        op: UnaryOp,
        operand: ExprId,
        ty: Ty,
    },
    /// The cast `expr` of `operand` converts between types that are not
    /// both numeric (`MR0204`).
    Cast {
        expr: ExprId,
        operand: ExprId,
        from: Ty,
        to: Ty,
    },
}

/// What expects the type of a [`TypeError::Mismatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// A declared, parameter, return or field type.
    Value,
    /// A condition, which expects `bool`.
    Condition,
    /// The other branch of `c ? a : b`, which gives the expected type.
    Branch(ExprId),
}

/// Checks the body of `owner` and gives the types of its expressions and
/// locals.
#[salsa::tracked(returns(ref))]
pub fn infer(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Inference {
    check::infer_body(db, project, file, owner)
}

/// Whether everything about `ty` is known, so that a diagnostic can be
/// about it: an unsupported type or an error inside it makes it unknown.
pub(crate) fn is_known(ty: &Ty) -> bool {
    match ty {
        Ty::Primitive(_) | Ty::String | Ty::Struct(_) => true,
        Ty::Tuple(elems) => elems.iter().all(|elem| is_known(&elem.ty)),
        Ty::Unsupported(_) | Ty::Error => false,
    }
}

/// Whether two types are the same, the names of tuple elements aside.
fn same(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(&a.ty, &b.ty))
        }
        _ => a == b,
    }
}

/// Whether a value of `from` converts implicitly to `to` (§5.10): the same
/// type, or one of the numeric rows.
pub(crate) fn converts(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Primitive(from), Ty::Primitive(to)) => numeric::converts(*from, *to),
        _ => same(from, to),
    }
}

/// `ty` as code writes it, as messages and the HIR dump show it.
pub(crate) fn ty_name(db: &dyn Db, ty: &Ty) -> String {
    match ty {
        Ty::Primitive(primitive) => primitive.keyword().to_owned(),
        Ty::String => "string".to_owned(),
        Ty::Tuple(elems) => {
            let mut parts: Vec<String> = elems
                .iter()
                .map(|elem| match &elem.name {
                    Some(name) => format!("{name}: {}", ty_name(db, &elem.ty)),
                    None => ty_name(db, &elem.ty),
                })
                .collect();
            if let [only] = parts.as_mut_slice() {
                only.push(',');
            }
            format!("({})", parts.join(", "))
        }
        Ty::Struct(item) => item_data(db, *item).name.clone(),
        Ty::Unsupported(_) | Ty::Error => "_".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use salsa::Setter;

    use crate::test_db::{TestDb, app, codes, file_named, messages};
    use crate::*;

    /// The codes `src/Program.mr` alone in an app reports.
    fn reported(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        codes(diagnostics(&db, project))
    }

    /// The type of each local of the top-level statements of `text`, `?`
    /// for one that is unknown.
    fn local_types(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.locals
            .iter()
            .map(|(local, data)| {
                let ty = inference
                    .local(local)
                    .map_or_else(|| "?".to_owned(), |ty| super::ty_name(&db, ty));
                format!("{}: {ty}", data.name)
            })
            .collect()
    }

    #[test]
    fn literals_take_their_target_type() {
        let text = "let a: byte = 255;\nlet b: float = 1.5;\nlet c = 3_000_000_000;\n\
                    let d = 18446744073709551615;\nlet e: sbyte = -128;\nlet f = 1.5f;\n\
                    let g = 2u;\nlet h = -2147483648;\nlet i = 2.5;\nlet j: double = 1;\n\
                    let k: (byte, float) = (1, 2.0);\nlet l = 'x';\nlet m = \"s\";\n";
        assert_eq!(
            local_types(text),
            [
                "a: byte",
                "b: float",
                "c: long",
                "d: ulong",
                "e: sbyte",
                "f: float",
                "g: uint",
                "h: int",
                "i: double",
                "j: double",
                "k: (byte, float)",
                "l: char",
                "m: string",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_literal_that_does_not_fit_its_type_is_reported_once() {
        let text = "let a: byte = 256;\nlet b: sbyte = -129;\nlet c = 4294967296u;\n\
                    let d: float = 1e39;\nlet e = -18446744073709551615;\n\
                    let f: float = 16777217;\nlet g = 9223372036854775808L;\n";
        assert_eq!(reported(text), ["MR0010"; 7]);
        // The lexer reports a literal larger than any `ulong`, and the
        // checker says nothing more about it.
        assert_eq!(
            reported("let a = 18446744073709551616;\nlet b: float = 1e400;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn operands_are_promoted() {
        let text = "let b: byte = 1;\nlet u: uint = 2;\nlet x: float = 1.5;\nlet n = 3;\n\
                    let l = 4L;\nlet s: short = 5;\n\
                    let bb = b + b;\nlet iu = n + u;\nlet nf = n + x;\nlet half = x * 0.5;\n\
                    let twice = b * 2;\nlet ll = l + 1;\nlet shifted = b << 1;\nlet neg = -u;\n\
                    let inv = ~s;\nlet nd = n * 0.5;\nlet cmp = b < u;\nlet wrap = n +% 1;\n\
                    let big = b + 300;\nlet cat = \"a\" + \"b\";\nlet both = true & false;\n";
        let types = local_types(text);
        assert_eq!(
            types[6..],
            [
                "bb: int",
                "iu: long",
                "nf: double",
                "half: float",
                "twice: int",
                "ll: long",
                "shifted: int",
                "neg: long",
                "inv: int",
                "nd: double",
                "cmp: bool",
                "wrap: int",
                "big: int",
                "cat: string",
                "both: bool",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn operators_not_defined_for_their_operands_are_mr0204() {
        let text = "let l = 1L;\nlet u = 2ul;\nlet x = 1.5;\nlet n = 1;\nlet t = true;\n\
                    let a = l + u;\nlet b = x & x;\nlet c = -u;\nlet d = !n;\nlet e = \"s\" + n;\n\
                    let f = t < t;\nlet g = (string)n;\nvar h = t;\nh++;\nlet i = x << 1;\n\
                    let j = x +% x;\nlet k = ~x;\nlet m = n == t;\nlet o = (int)'c';\n";
        assert_eq!(reported(text), ["MR0204"; 13]);
    }

    #[test]
    fn values_that_do_not_convert_are_mr0203() {
        let text = "let l = 1L;\nlet x: int = l;\nlet b: bool = 1;\nlet c: char = 65;\n\
                    let s: string = 1;\nif (1) {}\nwhile (\"s\") {}\nvar small: byte = 1;\n\
                    small += 1;\nlet pick = true ? 1 : \"a\";\nlet f: float = 1.5 * 2.0;\n\
                    let t: (int, string) = (1, 2);\n";
        // `small += 1` narrows back to `byte`, and `1.5 * 2.0` is made only
        // of literals, so it takes the type `float`.
        assert_eq!(reported(text), ["MR0203"; 8]);
    }

    #[test]
    fn return_values_and_parameters_follow_the_signature() {
        let text = "Half(x: int): long => x / 2;\nName(x: int): string {\n    return x;\n}\n\
                    Wide(x: long = 1): int => 0;\nNarrow(x: byte = 300) {}\n\
                    Unit() {\n    return 1;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0203", "MR0010", "MR0203"]);
    }

    #[test]
    fn struct_fields_and_constants_have_their_types() {
        let text = "struct P {\n    X: int;\n    Y: string;\n    \
                    Sum(): int => X + Y;\n}\nconst Limit: long = 10;\n\
                    F(p: P): int => p.X;\nG(p: P): int => p.Y;\nH(): int => Limit;\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0204", "MR0203", "MR0203"]);
    }

    #[test]
    fn unknown_types_cause_no_diagnostic() {
        let text = "let a = Missing + 1;\nlet b = Console.ReadLine() + 1;\nlet c: Unknwn = 1;\n\
                    let d: int = Console.ReadLine();\nlet e = [1, 2];\nlet f = e[0] + \"s\";\n\
                    let g: int? = 1;\nlet h = (x: int) => x + \"s\";\nlet k = d + g;\n";
        // `h`'s lambda adds a string to an `int`, which is checked.
        assert_eq!(reported(text), ["MR0127", "MR0127", "MR0204"]);
    }

    #[test]
    fn mixed_operands_get_a_cast_fix() {
        let db = TestDb::new();
        let text = "let l = 1L;\nlet u = 2ul;\nlet a = l + (u + u);\nlet x: float = 1.5;\n\
                    let b = l * x;\nlet c = -u;\nvar d: byte = 1;\nd += 1;\nif (l) {}\n\
                    let s: string = l;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let fixed: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| {
                let fix = &d.fixes[0];
                let edits: Vec<_> = fix
                    .edits
                    .iter()
                    .map(|edit| (u32::from(edit.span.range.start()), edit.replacement.clone()))
                    .collect();
                (fix.message.clone(), edits)
            })
            .collect();
        let at = |pattern: &str| u32::try_from(text.find(pattern).unwrap()).unwrap();
        let end = |pattern: &str| at(pattern) + u32::try_from(pattern.len()).unwrap();
        let last = |pattern: &str| u32::try_from(text.rfind(pattern).unwrap()).unwrap();
        assert_eq!(
            fixed,
            [
                (
                    "convert it explicitly with `(long)`".to_owned(),
                    vec![
                        (at("(u + u)") + 1, "(long)(".to_owned()),
                        (end("u + u"), ")".to_owned())
                    ]
                ),
                (
                    "convert it explicitly with `(double)`".to_owned(),
                    vec![(at("l * x"), "(double)".to_owned())]
                ),
                (
                    "convert it explicitly with `(long)`".to_owned(),
                    vec![(at("u;\nvar"), "(long)".to_owned())]
                ),
                (
                    "compare it with zero".to_owned(),
                    vec![(at("l) {}") + 1, " != 0".to_owned())]
                ),
                (
                    "format it with interpolation".to_owned(),
                    vec![
                        (last("l;\n"), "$\"{".to_owned()),
                        (last("l;\n") + 1, "}\"".to_owned())
                    ]
                ),
            ]
        );
    }

    #[test]
    fn operations_on_literals_take_their_target_type() {
        let text = "let n = 1;\nlet a: float = 1.5 * 2.0;\nlet b: byte = 1 + 2;\n\
                    let c: sbyte = -(64 * 2);\nlet d: byte = ~0 & 255;\nlet e: float = 1 / 3;\n\
                    let f: ulong = 18446744073709551615 - 1;\nlet g: double = (1 + 2) * 0.5;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let wrong = "let n = 1;\nlet a: byte = 200 + 100;\nlet b: sbyte = -(64 * 2) - 1;\n\
                     let c: long = 9223372036854775807 * 4;\nlet d: float = 1e38 * 10.0;\n\
                     let e: byte = 1 + n;\nlet f: int = 1.5 * 2.0;\nlet g: byte = 1 / 0;\n";
        // Not made only of literals, a real for an integer type, and a
        // division by zero follow the usual rules.
        assert_eq!(
            reported(wrong),
            [
                "MR0010", "MR0010", "MR0010", "MR0010", "MR0203", "MR0203", "MR0203"
            ]
        );
    }

    #[test]
    fn a_compound_assignment_narrows_back_to_its_place() {
        let text = "var b: byte = 1;\nb += 1;\nb +%= 300;\nb <<= 1;\nvar i = 0;\ni += 2L;\n\
                    i *= 1.5;\nvar s = \"\";\ns += \"x\";\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let wrong = "var s = \"\";\ns += 1;\nvar l = 1L;\nl += 1ul;\nvar f = true;\nf += f;\n";
        assert_eq!(reported(wrong), ["MR0204"; 3]);
    }

    #[test]
    fn messages_name_the_types() {
        let db = TestDb::new();
        let text = "let l = 1L;\nlet u = 2ul;\nlet a = l + u;\nlet b: int = l;\n\
                    let c: byte = 300;\nlet d: byte = 200 + 100;\nlet e = (int)'c';\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        assert_eq!(
            messages(diagnostics(&db, project)),
            [
                "`+` is not defined for `long` and `ulong`",
                "mismatched types: expected `int`, found `long`",
                "the literal `300` does not fit in `byte`",
                "the value of `200 + 100` does not fit in `byte`",
                "cannot cast `char` to `int`",
            ]
        );
    }

    #[test]
    fn an_edit_inside_one_body_infers_only_that_body_again() {
        let mut db = TestDb::new();
        let text = |n: u32| format!("A(): int {{\n    return {n};\n}}\nB(): int => 2;\n");
        let project = app(&db, &[("src/Program.mr", &text(1))]);
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(diagnostics(&db, project), &[]);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        assert_eq!(diagnostics(&db, project), &[]);
        let inferred = db
            .take_executed()
            .iter()
            .filter(|key| key.starts_with("infer("))
            .count();
        assert_eq!(inferred, 1);
    }

    /// What `src/Program.mr` alone in an app reports.
    fn diagnostics_of(text: &str) -> Vec<mora_diag::Diagnostic> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        diagnostics(&db, project).clone()
    }

    /// The note of the one diagnostic `text` reports.
    fn note(text: &str) -> String {
        let reported = diagnostics_of(text);
        assert_eq!(reported.len(), 1, "{text}: {reported:#?}");
        reported[0].note.clone().unwrap_or_default()
    }

    /// Each expression of the top-level statements of `text`, as written,
    /// with its type, `?` for one that is unknown.
    fn expr_types(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.exprs
            .iter()
            .map(|(expr, _)| {
                let ty = inference
                    .expr(expr)
                    .map_or_else(|| "?".to_owned(), |ty| super::ty_name(&db, ty));
                format!("{}: {ty}", &text[map.expr(expr)])
            })
            .collect()
    }

    #[test]
    fn equality_and_ordering_cover_bool_char_and_string() {
        let text = "let a = \"a\" == \"b\";\nlet b = 'a' != 'b';\nlet c = true == false;\n\
                    let d = \"a\" < \"b\";\nlet e = 'a' >= 'b';\nlet f = true & false;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(reported("let f = true & 1;\n"), ["MR0204"]);
    }

    #[test]
    fn an_expression_body_of_a_unit_function_is_not_a_return_value() {
        let text = "Unit() => 1;\nAlso(): () => 1;\nBlock(): int {\n    return 1;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn only_a_synchronous_local_function_checks_what_it_returns() {
        assert_eq!(reported("F() {\n    G(): int => \"s\";\n}\n"), ["MR0203"]);
        assert_eq!(
            reported("F() {\n    async H(): int => \"s\";\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn expressions_inside_matches_patterns_and_arguments_are_checked() {
        let n = "let n = 1;\n";
        for (text, code) in [
            ("let a = match (n) { 1 => !n, _ => true };\n", "MR0204"),
            (
                "let a = match (n) { var m when n => 1, _ => 2 };\n",
                "MR0203",
            ),
            ("let b = n is < -true;\n", "MR0204"),
            ("let c = (n, n) is (1, < -true);\n", "MR0204"),
            ("Console.WriteLine(!n);\n", "MR0204"),
        ] {
            assert_eq!(reported(&format!("{n}{text}")), [code], "{text}");
        }
    }

    #[test]
    fn every_part_of_an_operation_on_literals_takes_the_target_type() {
        assert_eq!(
            expr_types("let c: sbyte = -(1 + 2);\n"),
            [
                "1: sbyte",
                "2: sbyte",
                "1 + 2: sbyte",
                "-(1 + 2): sbyte",
                // The top-level statements, a block, have no value.
                "let c: sbyte = -(1 + 2);: ?"
            ]
        );
    }

    #[test]
    fn operations_on_literals_follow_their_operators() {
        let fine = "let a: byte = +(1 + 2);\nlet b: float = -(1.5 * 2.0);\n";
        assert_eq!(reported(fine), Vec::<String>::new());
        let huge = "18446744073709551615 * 18446744073709551615 * 18446744073709551615";
        for (text, code) in [
            ("let d: double = 1e308 * 10.0;\n".to_owned(), "MR0010"),
            (format!("let l: long = -({huge});\n"), "MR0010"),
            (format!("let l: double = {huge} * 1.5;\n"), "MR0010"),
            ("let x: double = ~(1.5 + 1.0);\n".to_owned(), "MR0204"),
            ("let b: byte = 1u + 1;\n".to_owned(), "MR0203"),
            ("let f: float = 1.5f * 2.0;\n".to_owned(), "MR0203"),
        ] {
            assert_eq!(reported(&text), [code], "{text}");
        }
    }

    #[test]
    fn negating_the_smallest_value_too_large_to_compute_is_mr0010() {
        // `-(2^63 * 2^63) * 2` is `-2^127`, whose negation overflows.
        let text = "let x: long = -(-(9223372036854775808 * 9223372036854775808) * 2);\n";
        assert_eq!(reported(text), ["MR0010"]);
    }

    #[test]
    fn literal_operands_take_the_other_operand_type() {
        let text = "let x: float = 1.5;\nlet y = x * 1e39;\nlet z = x * -0.5;\nlet w = 0.5 * x;\n\
                    let n = -1u;\nlet c = 1 ?? 2;\nlet u: ushort = 65535;\n";
        assert_eq!(
            local_types(text),
            [
                "x: float",
                "y: double",
                "z: float",
                "w: float",
                "n: long",
                "c: ?",
                "u: ushort"
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn operands_of_unknown_type_cause_no_diagnostic() {
        let text = "let e: Unknwn = 1;\nlet a = -e;\nlet b = e << 1;\nvar c = e;\nc++;\n\
                    let d = true ? e : \"s\";\nlet f = true ? Console.A() : 1;\n";
        assert_eq!(reported(text), ["MR0127"]);
        assert_eq!(
            reported("let a = -\"s\";\nlet b = -true;\nlet c = -'c';\n"),
            ["MR0204"; 3]
        );
    }

    #[test]
    fn initializers_check_against_their_declared_type() {
        let text = "const Limit: byte = 300;\nclass A {\n    X: byte = 300;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0010", "MR0010"]);
    }

    #[test]
    fn tuples_match_by_length_and_element_types_but_not_names() {
        let text = "let t: (int, int) = (1, 2, 3);\nlet p = (1, 2);\n\
                    let q: (Left: int, Right: int) = p;\nlet r: (int,) = p;\n";
        assert_eq!(reported(text), ["MR0203", "MR0203"]);
    }

    #[test]
    fn members_after_a_value_and_optional_members() {
        let text = "struct P {\n    X: int;\n}\nstatic Origin: P = new P { X = 1 };\n\
                    let p = Origin;\nlet v = p?.X;\nlet x = Origin.X;\n";
        assert_eq!(local_types(text), ["p: P", "v: ?", "x: int"]);
        assert_eq!(
            reported(&format!("{text}let s: string = Origin.X;\n")),
            ["MR0203"]
        );
    }

    #[test]
    fn a_parameter_without_a_name_leaves_the_others_unknown() {
        let text = "F(a: int = !1, : string, c: bool): bool => c;\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0204"]);
    }

    #[test]
    fn notes_say_why_a_value_does_not_convert() {
        for (text, expected) in [
            (
                "let b: bool = 1;\n",
                "there is no implicit conversion to or from `bool`",
            ),
            (
                "let c: char = 65;\n",
                "there is no implicit conversion to or from `char`",
            ),
            (
                "let d: double = 9007199254740993;\n",
                "a `double` holds every integer from -9007199254740992 to 9007199254740992 exactly, and only some beyond",
            ),
            (
                "let g: float = 16777217;\n",
                "a `float` holds every integer from -16777216 to 16777216 exactly, and only some beyond",
            ),
            (
                "let n: nint = 1;\nlet i = 2;\nlet a = n + i;\n",
                "`nint` and `nuint` mix with other integer types only through an explicit cast",
            ),
            (
                "let s = \"s\" + 1;\n",
                "`+` joins two `string` values; interpolation formats other values as text, as in `$\"{a}{b}\"`",
            ),
            ("let s = \"s\" - 1;\n", ""),
            (
                "let u = true < false;\n",
                "`bool` values are not ordered and are not numbers",
            ),
            (
                "let v = !1;\n",
                "`!` negates a `bool`; compare a number explicitly, as in `x == 0`",
            ),
            ("let w = ~1.5;\n", "`~` is defined only on integer types"),
        ] {
            assert_eq!(note(text), expected, "{text}");
        }
        let reported = diagnostics_of("let s: string = (1, 2);\nlet f: float = 1e39;\n");
        assert!(reported[0].fixes.is_empty());
        assert_eq!(
            reported[1].message,
            "the literal `1e39` is too large for a `float`"
        );
    }

    #[test]
    fn mixed_and_negated_native_integers_get_a_cast_fix() {
        let text = "let n: nint = 1;\nlet b: byte = 2;\nlet k = b + n;\nlet nu: nuint = 1;\nlet x = -nu;\n";
        let fixes: Vec<_> = diagnostics_of(text)
            .iter()
            .map(|d| d.fixes[0].message.clone())
            .collect();
        assert_eq!(
            fixes,
            [
                "convert it explicitly with `(nint)`",
                "convert it explicitly with `(nint)`"
            ]
        );
    }

    #[test]
    fn a_cast_between_char_and_an_integer_points_to_a_function() {
        let named = "a cast `(T)x` converts only from one numeric type to another, and a `char` is not a number; convert between `char` and integers with a named conversion function";
        let plain = "a cast `(T)x` converts only from one numeric type to another";
        for (text, expected) in [
            ("let a = (int)'c';\n", named),
            ("let l = 1L;\nlet b = (char)l;\n", named),
            ("let c = (int)true;\n", plain),
            ("let d = (string)'c';\n", plain),
        ] {
            assert_eq!(note(text), expected, "{text}");
        }
        let text = "let t = true;\nlet c = (int)t;\n";
        let reported = diagnostics_of(text);
        let operand = reported[0].secondary[0].span.range;
        assert_eq!(&text[operand], "t");
    }

    #[test]
    fn types_are_named_as_code_writes_them() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "struct P {}\n")]);
        let file = file_named(&db, project, "src/Program.mr");
        let point = crate::test_db::item_named(&db, project, file, "P");
        let int = TupleElem {
            name: None,
            ty: Ty::Primitive(Primitive::Int),
        };
        let named = TupleElem {
            name: Some("Id".to_owned()),
            ty: Ty::Struct(point),
        };
        assert_eq!(
            super::ty_name(&db, &Ty::Tuple(vec![named, int.clone()])),
            "(Id: P, int)"
        );
        assert_eq!(super::ty_name(&db, &Ty::Tuple(vec![int])), "(int,)");
        assert_eq!(super::ty_name(&db, &Ty::Error), "_");
    }
}
