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
    /// assignment, is not defined for its operands (`MR0204`).
    Binary {
        expr: ExprId,
        op: BinaryOp,
        lhs: Ty,
        rhs: Ty,
    },
    /// The prefix or postfix operator of `expr` is not defined for its
    /// operand (`MR0204`); `++` and `--` are the prefix forms.
    Unary {
        expr: ExprId,
        op: UnaryOp,
        operand: Ty,
    },
    /// The cast `expr` converts between types that are not both numeric
    /// (`MR0204`).
    Cast { expr: ExprId, from: Ty, to: Ty },
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
}
