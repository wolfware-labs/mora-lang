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

use std::collections::HashMap;

use mora_span::{Db, Project, SourceFile};

use crate::arena::arena_index;
use crate::bodies::{ArgMode, BinaryOp, BodyOwner, ExprId, LocalId, UnaryOp};
use crate::defs::ItemId;
use crate::resolve::item_data;
use crate::ty::{ParamMode, Primitive, Ty};

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
    /// The function each call that is checked calls.
    calls: HashMap<ExprId, Callee>,
    /// The value of each operation made only of literals that took a target
    /// type and fits it.
    constants: HashMap<ExprId, Constant>,
    /// The type errors, in the order they were found.
    pub(crate) errors: Vec<TypeError>,
}

/// The value of an operation made only of literals that takes its target
/// type (§3.5): the exact result, converted to that type. It is the
/// operation's value, to be used as is: evaluating the operators in the
/// target type could give another result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Constant {
    /// A value of an integer type.
    Int(i128),
    /// A value of `float` or `double`, as the bits of the `double` that
    /// holds it, exactly for a `float` too.
    Real(u64),
}

impl std::fmt::Display for Constant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Constant::Int(value) => write!(f, "{value}"),
            Constant::Real(bits) => write!(f, "{}", f64::from_bits(*bits)),
        }
    }
}

impl Inference {
    /// What the call `call` calls, when the call is checked: the overload
    /// that overload resolution chose, or the one function of its name.
    pub fn callee(&self, call: ExprId) -> Option<Callee> {
        self.calls.get(&call).copied()
    }

    /// The value of `expr` when it is an operation made only of literals
    /// that took its target type (§3.5): what the operation evaluates to.
    pub fn constant(&self, expr: ExprId) -> Option<Constant> {
        self.constants.get(&expr).copied()
    }

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
    /// The operand `operand` of `op`, `&&` or `||`, is of type `ty`, not
    /// `bool` (`MR0204`). No type can declare these operators (§6.9), so
    /// each operand is checked on its own.
    Logical {
        op: BinaryOp,
        operand: ExprId,
        ty: Ty,
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
    /// A call of one declaration, `signature`, with an argument too many,
    /// `expr`, or without an argument for the parameter `missing`, the call
    /// `expr` (`MR0205`).
    Arity {
        expr: ExprId,
        signature: String,
        missing: Option<String>,
    },
    /// The argument `arg` of a call of one declaration, `signature`, is
    /// written with the marker `given` where its parameter `param` has the
    /// mode `needs` (`MR0205`); `fixable` when writing `ref` before it
    /// would fix the call, because it is a place of a fitting type.
    Mode {
        arg: ExprId,
        signature: String,
        param: String,
        needs: ParamMode,
        given: Option<ArgMode>,
        fixable: bool,
    },
    /// No overload named `name` is applicable to the call `expr`; each
    /// candidate with its signature (`MR0205`).
    NoOverload {
        expr: ExprId,
        name: String,
        candidates: Vec<(ItemId, String)>,
    },
    /// Of the overloads named `name` applicable to the call `expr`, none is
    /// better than the others; each of them with its signature (`MR0206`).
    Ambiguous {
        expr: ExprId,
        name: String,
        candidates: Vec<(ItemId, String)>,
    },
    /// The named argument `arg`, or the positional one after a named one,
    /// is wrong (`MR0207`).
    NamedArgument { arg: ExprId, problem: NamedProblem },
    /// The callee `expr` has the type `ty`, which is not a function type
    /// (`MR0208`).
    NotCallable { expr: ExprId, ty: Ty },
}

/// What is wrong with an argument's name (`MR0207`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamedProblem {
    /// A positional argument after a named one.
    AfterNamed,
    /// A name given twice.
    Duplicate { name: String },
    /// A name the function `function` has no parameter for.
    Unknown { name: String, function: String },
    /// A name for a parameter given by position too.
    AlreadyGiven { name: String },
}

/// What a checked call calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A function or a method, by its declaration.
    Item(ItemId),
    /// A local function of the body.
    Local(LocalId),
}

impl Callee {
    /// The declaration of a [`Callee::Item`].
    pub fn item(self) -> Option<ItemId> {
        match self {
            Callee::Item(item) => Some(item),
            Callee::Local(_) => None,
        }
    }
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

/// The type errors of the body of `owner`, found by checking it again
/// rather than through [`infer`]. The diagnostics of a file run once per
/// edit of that file, and each body's memo of [`infer`] would otherwise be
/// one more memo that every later revision has to verify.
pub(crate) fn type_errors(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Vec<TypeError> {
    check::infer_body(db, project, file, owner).errors
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
        let infer_all = |db: &TestDb| {
            for owner in body_owners(db, file) {
                infer(db, project, file, owner);
            }
        };
        infer_all(&db);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        infer_all(&db);
        let inferred = db
            .take_executed()
            .iter()
            .filter(|key| key.starts_with("infer("))
            .count();
        assert_eq!(inferred, 1);
    }

    #[test]
    fn diagnostics_check_the_bodies_of_an_edited_file_only() {
        let mut db = TestDb::new();
        let text = |n: u32| format!("A(): int {{\n    return {n};\n}}\nB(): int => \"b\";\n");
        let project = app(
            &db,
            &[
                ("src/Program.mr", &text(1)),
                ("src/Other.mr", "C(): int => \"c\";\n"),
            ],
        );
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(codes(diagnostics(&db, project)), ["MR0203", "MR0203"]);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        assert_eq!(codes(diagnostics(&db, project)), ["MR0203", "MR0203"]);
        let executed = db.take_executed();
        // No memo of `infer` is made for the diagnostics, and only the
        // edited file's are built again.
        assert!(
            !executed.iter().any(|key| key.starts_with("infer(")),
            "{executed:?}"
        );
        let rebuilt = executed
            .iter()
            .filter(|key| key.starts_with("body_diagnostics("))
            .count();
        assert_eq!(rebuilt, 1);
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
    fn a_real_the_lexer_reported_is_not_reported_again() {
        let text =
            "let a: double = -1e400;\nlet b: float = -1e400;\nlet c: double = 1e400 + 1.0;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_real_operation_that_is_not_a_number_fits() {
        let text = "let a: double = 0.0 / 0.0;\nlet b: float = 0.0 / 0.0;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn only_arithmetic_prefix_operators_fold_a_huge_literal_operation() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        // `!` and `^` are not arithmetic: the operation follows §7.12, where
        // `ulong * ulong` is a `ulong`, and `!` on it is MR0204.
        assert_eq!(reported(&format!("let a: bool = !{huge};\n")), ["MR0204"]);
        assert_eq!(
            reported(&format!("let b: long = ^{huge};\n")),
            Vec::<String>::new()
        );
        assert_eq!(reported(&format!("let c: long = ~{huge};\n")), ["MR0010"]);
        // Dividing by zero is not folded, so it is not MR0010.
        assert_eq!(
            reported(&format!("let d: ulong = {huge} / 0;\n")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn operators_on_a_struct_are_left_to_its_declarations() {
        let text = "public struct Money {\n    public Cents: int;\n    \
                    public static operator +(a: Money, b: Money): Money => a;\n    \
                    public static operator -(a: Money): Money => a;\n}\n\
                    F(a: Money, b: Money) {\n    let c: int = a + b;\n    let d = -a;\n    \
                    let e = a << 1;\n    let f = 1 << a;\n    var g = a;\n    g++;\n    \
                    g += b;\n    let h = !a;\n    let i = ~a;\n    let j = a == b;\n    \
                    let k = a < b;\n    let m = 1 + a;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        // Comparing a struct with a number is still not defined.
        let mixed = text.replace("let k = a < b;", "let k = a == 1;");
        assert_eq!(reported(&mixed), ["MR0204"]);
    }

    #[test]
    fn tuples_of_different_types_compare_to_an_unknown_result() {
        let text = "let t: (byte, byte) = (1, 2);\nlet a = t == (1, 2);\nlet b = t != (1L, 2);\n\
                    let c = t == t;\nlet d: (Left: byte, Right: byte) = t;\nlet e = t == d;\n";
        assert_eq!(
            local_types(text),
            [
                "t: (byte, byte)",
                "a: ?",
                "b: ?",
                "c: bool",
                "d: (Left: byte, Right: byte)",
                "e: bool"
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
        // A tuple compared with something that is not a tuple of its length
        // is not defined.
        let wrong = "let t = (1, 2);\nlet a = t < t;\nlet b = t == 1;\nlet c = 1 != t;\n\
                     let d = t == (1, 2, 3);\n";
        assert_eq!(reported(wrong), ["MR0204"; 4]);
    }

    #[test]
    fn logical_operators_take_bool_operands() {
        let text = "let n = 1;\nlet a = n && true;\nlet b = true || n;\nlet c = 1 && 2;\n\
                    let d: bool = n > 0 && n < 9;\n";
        // `1 && 2` has two operands that are not `bool`.
        assert_eq!(reported(text), ["MR0204"; 4]);
        assert_eq!(
            note("let n = 1;\nlet a = n || true;\n"),
            "`&&` and `||` combine `bool` values; compare a number explicitly, as in `x != 0`"
        );
        // An operand of unknown type does not hide the other one.
        let text = "let e: Unknwn = 1;\nlet a = e && 1;\nlet b = Console.A() || \"s\";\n";
        let reported = diagnostics_of(text);
        let shown: Vec<_> = reported
            .iter()
            .map(|d| format!("{} {}", d.code, &text[d.primary.span.range]))
            .collect();
        assert_eq!(shown, ["MR0127 Unknwn", "MR0204 1", "MR0204 \"s\""]);
        assert_eq!(
            reported[1].message,
            "`&&` takes `bool` operands, and this one is `int`"
        );
    }

    #[test]
    fn a_compound_assignment_is_named_and_fixed_through_its_value() {
        let text = "var u = 1ul;\nlet l = 2L;\nu += l;\nvar f: float = 1.5;\nf -= l;\n\
                    var s = \"\";\ns += 1;\nvar x = 1.5;\nx <<= 1;\n";
        let reported = diagnostics_of(text);
        let shown: Vec<_> = reported
            .iter()
            .map(|d| {
                let fix = d.fixes.first().map(|fix| {
                    let edit = &fix.edits[0];
                    let at = usize::try_from(u32::from(edit.span.range.start())).unwrap();
                    (
                        fix.message.clone(),
                        text[at..].lines().next().unwrap().to_owned(),
                    )
                });
                (d.message.clone(), fix)
            })
            .collect();
        let fixed = |message: &str, at: &str| Some((message.to_owned(), at.to_owned()));
        assert_eq!(
            shown,
            [
                (
                    "`+=` is not defined for `ulong` and `long`".to_owned(),
                    fixed("convert it explicitly with `(ulong)`", "l;")
                ),
                (
                    "`-=` is not defined for `float` and `long`".to_owned(),
                    fixed("convert it explicitly with `(float)`", "l;")
                ),
                (
                    "`+=` is not defined for `string` and `int`".to_owned(),
                    None
                ),
                ("`<<=` cannot shift a `double`".to_owned(), None),
            ]
        );
    }

    #[test]
    fn every_cast_fix_for_mixed_operands_type_checks() {
        const NUMERIC: [&str; 12] = [
            "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "nint", "nuint",
            "float", "double",
        ];
        const OPERATORS: [&str; 11] = ["+", "-", "*", "/", "%", "&", "|", "^", "+%", "-%", "*%"];
        let mut text = String::new();
        let mut index = 0;
        for place in NUMERIC {
            for value in NUMERIC {
                for op in OPERATORS {
                    text.push_str(&format!(
                        "F{index}(p: {place}, x: {value}) {{\n    var q = p;\n    q {op}= x;\n    \
                         let r = p {op} x;\n}}\n"
                    ));
                    index += 1;
                }
            }
        }
        let fixed = apply_fixes(&text);
        assert!(fixed.edits > 100, "{}", fixed.edits);
        assert_eq!(fixed.remaining, Vec::<String>::new());
    }

    /// A text with the edits of every fix its diagnostics offer applied.
    struct Fixed {
        /// The text after the edits.
        text: String,
        /// How many edits were applied.
        edits: usize,
        /// The lines a fix changed that still report a diagnostic, which
        /// must be none.
        remaining: Vec<String>,
    }

    /// `text` with every fix its diagnostics offer applied. A fix adds no
    /// line, so a line has one number in both texts.
    fn apply_fixes(text: &str) -> Fixed {
        let mut edits: Vec<_> = diagnostics_of(text)
            .iter()
            .flat_map(|d| d.fixes.iter().flat_map(|fix| fix.edits.clone()))
            .collect();
        // Edits apply from the end of the text, so earlier offsets hold.
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.range.start()));
        let mut fixed = text.to_owned();
        for edit in &edits {
            fixed.replace_range(
                std::ops::Range::<usize>::from(edit.span.range),
                &edit.replacement,
            );
        }
        let line_of =
            |index: &mora_span::LineIndex, offset| index.line_col(offset).map(|at| at.line);
        let before = mora_span::LineIndex::new(text);
        let fixed_lines: std::collections::BTreeSet<_> = edits
            .iter()
            .map(|edit| line_of(&before, edit.span.range.start()))
            .collect();
        let after = mora_span::LineIndex::new(&fixed);
        let lines: Vec<_> = fixed.lines().collect();
        let remaining = diagnostics_of(&fixed)
            .iter()
            .map(|d| line_of(&after, d.primary.span.range.start()))
            .filter(|line| fixed_lines.contains(line))
            .map(|line| line.map_or("?", |line| lines[line as usize]).to_owned())
            .collect();
        Fixed {
            edits: edits.len(),
            text: fixed,
            remaining,
        }
    }

    #[test]
    fn a_number_in_a_logical_operation_is_compared_with_zero() {
        let text = "F(n: int, x: double, b: byte, c: bool, s: string) {\n    \
                    let a = n && c;\n    let d = c || x;\n    let e = (n + 1) && c;\n    \
                    let f = (n & 1) || c;\n    let g = (c ? n : 2) && c;\n    \
                    let h = c && (b ^ b);\n    let i = s && c;\n}\n";
        let fixed = apply_fixes(text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        let fixed = fixed.text;
        for expected in [
            "let a = n != 0 && c;",
            "let d = c || x != 0;",
            "let e = (n + 1 != 0) && c;",
            "let f = ((n & 1) != 0) || c;",
            "let g = ((c ? n : 2) != 0) && c;",
            "let h = c && ((b ^ b) != 0);",
            "let i = s && c;",
        ] {
            assert!(fixed.contains(expected), "{expected}\n{fixed}");
        }
        // A string has no fix.
        assert_eq!(reported(&fixed), ["MR0204"]);
    }

    #[test]
    fn an_integer_only_operator_on_a_float_says_so() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        for (text, sym) in [
            (
                "let p = 1L;\nlet x: float = 1;\nlet a = p & x;\n".to_owned(),
                "&",
            ),
            (
                "let l = 1L;\nlet f: float = 1;\nlet a = l | f;\n".to_owned(),
                "|",
            ),
            ("var g: float = 1;\nlet l = 1L;\ng &= l;\n".to_owned(), "&="),
            (format!("let a: long = 1.5 & {huge};\n"), "&"),
            (
                "let y = 1.5;\nlet z: nint = 1;\nlet a = y & z;\n".to_owned(),
                "&",
            ),
            ("let y = 1.5;\nlet a = y ^ y;\n".to_owned(), "^"),
        ] {
            assert_eq!(
                note(&text),
                format!("`{sym}` is defined only on integer types"),
                "{text}"
            );
            assert!(diagnostics_of(&text)[0].fixes.is_empty(), "{text}");
        }
    }

    #[test]
    fn an_integer_too_large_to_compute_in_a_real_is_named_as_such() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        let label = |text: &str| {
            let reported = diagnostics_of(text);
            assert_eq!(codes(&reported), ["MR0010"], "{text}");
            reported[0].primary.message.clone().unwrap_or_default()
        };
        assert_eq!(
            label(&format!("let a: double = {huge} * 1.5;\n")),
            "an integer in it is too large to compute"
        );
        assert_eq!(
            label(&format!("let b: double = -({huge} * 1.5) * 2.0;\n")),
            "an integer in it is too large to compute"
        );
        assert_eq!(
            label("let c: double = 1e308 * 10.0;\n"),
            "this would be infinity as a `double`"
        );
        assert_eq!(
            label(&format!("let d: double = {huge} * 1.5 + 1e308 * 10.0;\n")),
            "this would be infinity as a `double`"
        );
        assert_eq!(
            reported(&format!("let e: long = {huge} * 1.5;\n")),
            ["MR0204"]
        );
    }

    #[test]
    fn integer_operators_on_literals_fold_as_integers_for_a_float() {
        let db = TestDb::new();
        let text = "let a: float = ~1;\nlet b: double = 1 & 2;\nlet c: double = (1 | 2) * 3;\n\
                    let d: float = ~1 ^ 2;\nlet e: double = 1 +% 2;\nlet f: double = -0;\n\
                    let g: double = -0.0;\nlet h: int = -1;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        assert_eq!(diagnostics(&db, project), &[]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let inits: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local { init, .. } => *init,
                _ => None,
            })
            .collect();
        let values: Vec<_> = inits.iter().map(|&init| inference.constant(init)).collect();
        let real = |value: f64| Some(Constant::Real(value.to_bits()));
        assert_eq!(
            values,
            [
                real(-2.0),
                real(0.0),
                real(9.0),
                real(-4.0),
                // `+%` is not an operator of §3.5, so the operation follows
                // §7.12; a negated literal is a literal, not an operation.
                None,
                None,
                None,
                None,
            ]
        );
        assert_eq!(
            inference.expr(inits[4]),
            Some(&Ty::Primitive(Primitive::Int))
        );
        let dumped = hir_dump(&db, project, file);
        assert!(dumped.contains("let f#5: double = -0;"), "{dumped}");
    }

    #[test]
    fn an_operation_on_literals_has_its_exact_value_in_the_target_type() {
        let db = TestDb::new();
        let text = "let a: double = 9007199254740993 - 9007199254740992;\n\
                    let b: float = 1 / 3;\nlet c: byte = 200 + 55;\nlet d: float = 0.1 * 1;\n\
                    let e: double = 0.1 * 1;\nlet f: float = 1 + 2;\nlet g: int = 1 + n;\n\
                    let h: byte = 200 + 100;\nlet n = 1;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let values: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local {
                    init: Some(init), ..
                } => Some(inference.constant(*init)),
                _ => None,
            })
            .collect();
        let float = |value: f32| Some(Constant::Real(f64::from(value).to_bits()));
        assert_eq!(
            values,
            [
                Some(Constant::Real(1.0f64.to_bits())),
                float(0.0),
                Some(Constant::Int(255)),
                float(0.1),
                Some(Constant::Real(0.1f64.to_bits())),
                float(3.0),
                // Not made only of literals, and not fitting.
                None,
                None,
                // A literal on its own is not an operation.
                None,
            ]
        );
        assert_eq!(Constant::Real(0.5f64.to_bits()).to_string(), "0.5");
        let dumped = hir_dump(&db, project, file);
        assert!(
            dumped.contains("let a#0: double = 9007199254740993 - 9007199254740992{= 1};"),
            "{dumped}"
        );
        assert!(dumped.contains("let b#1: float = 1 / 3{= 0};"), "{dumped}");
    }

    #[test]
    fn a_native_integer_literal_fits_every_target() {
        assert_eq!(
            note("let n: nint = 3000000000;\n"),
            "a literal of type `nint` must be from -2147483648 to 2147483647, the range of \
             `int`, so that it is valid on every target; cast a `long` for a larger value"
        );
        assert_eq!(
            note("let u: nuint = 5000000000;\n"),
            "a literal of type `nuint` must be from 0 to 4294967295, the range of `uint`, so \
             that it is valid on every target; cast a `ulong` for a larger value"
        );
        assert_eq!(
            reported("let n: nint = 2147483647;\nlet m: nint = (nint)3000000000L;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_operand_of_unknown_type_leaves_the_result_unknown() {
        let text = "let a: int = !Missing;\nlet b: int = Missing && true;\n\
                    let c: int = true || Missing;\nlet d: int = Missing == 1;\n\
                    let e: int = 1 < Missing;\nlet f: int = !Console.A();\n\
                    let g: int = Console.A() != 1;\nlet h: int = Console.A() || true;\n";
        assert_eq!(reported(text), ["MR0127"; 5]);
        assert_eq!(
            local_types(
                "let f = !Console.A();\nlet g = Console.A() == 1;\nlet h = true && Console.A();\n"
            ),
            ["f: ?", "g: ?", "h: ?"]
        );
        // With known operands, the result is a `bool` even after an error.
        let known = "let a: int = !true;\nlet b: int = 1 == 1;\nlet c: int = \"a\" < 1;\n\
                     let d: int = 1 && true;\n";
        assert_eq!(
            reported(known),
            ["MR0203", "MR0203", "MR0204", "MR0203", "MR0204", "MR0203"]
        );
    }

    #[test]
    fn a_real_divided_by_an_integer_zero_is_folded() {
        assert_eq!(reported("let a: double = 1.5 / 0;\n"), ["MR0010"]);
        assert_eq!(reported("let b: double = 1.5 / 0.0;\n"), ["MR0010"]);
        assert_eq!(reported("let c: int = 1 / 0;\n"), Vec::<String>::new());
    }

    #[test]
    fn the_remainder_of_the_smallest_value_by_minus_one_is_zero() {
        let text = "let a: long = (-(9223372036854775808 * 9223372036854775808) * 2) % -1;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_bitwise_operator_on_a_real_and_a_huge_integer_is_not_folded() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        assert_eq!(
            reported(&format!("let a: long = 1.5 & {huge};\n")),
            ["MR0204"]
        );
        assert_eq!(
            reported(&format!("let b: long = {huge} | 1;\n")),
            ["MR0010"]
        );
    }

    #[test]
    fn an_infinity_at_any_step_does_not_fit() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        for (text, expected) in [
            (
                "let a: double = (1e308 * 10.0) - (1e308 * 10.0);\n".to_owned(),
                vec!["MR0010"],
            ),
            (
                "let b: double = (1e308 * 10.0) * 0.0;\n".to_owned(),
                vec!["MR0010"],
            ),
            (
                "let c: float = -(1e308 * 10.0);\n".to_owned(),
                vec!["MR0010"],
            ),
            ("let d: double = 0.0 / 0.0;\n".to_owned(), vec![]),
            // A real for an integer type follows the usual rules.
            ("let e: int = 1e308 * 10.0;\n".to_owned(), vec!["MR0203"]),
            (format!("let f: long = {huge} + 1.5;\n"), vec!["MR0204"]),
            (
                "let g: long = ~(1e308 * 10.0);\n".to_owned(),
                vec!["MR0204"],
            ),
        ] {
            assert_eq!(reported(&text), expected, "{text}");
        }
        // The parts of a fitting real operation take the target type.
        assert_eq!(expr_types("let h: double = 1 + 0.5;\n")[0], "1: double");
    }

    #[test]
    fn tuples_of_tuples_compare_by_shape_at_every_level() {
        let t = "let t = ((1, 2), 3);\n";
        for (text, expected) in [
            ("let a = t == (1, 3);\n", vec!["MR0204"]),
            ("let b = t == ((1, 2, 3), 3);\n", vec!["MR0204"]),
            ("let c = t != ((1, 2), (3, 4));\n", vec!["MR0204"]),
            ("let d = t == ((1L, 2), 3);\n", vec![]),
        ] {
            assert_eq!(reported(&format!("{t}{text}")), expected, "{text}");
        }
        let types = local_types(&format!("{t}let d = t == ((1L, 2), 3);\nlet e = t == t;\n"));
        assert_eq!(types[1..], ["d: ?", "e: bool"]);
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

    #[test]
    fn a_call_has_the_return_type_of_what_it_calls() {
        let text = "Add(a: int, b: int): long => a + b;\nstruct P {\n    X: int;\n    \
                    Get(): int => X;\n    static Make(): string => \"\";\n}\n\
                    class C {\n    static Make(n: int): string => \"\";\n}\n\
                    static Origin: P = new P { X = 1 };\nasync Later(): int => 1;\n\
                    Id<T>(x: T): T => x;\nSum(params xs: int[]): int => 0;\n\
                    let s = Add(1, 2);\nlet p = Origin;\nlet g = p.Get();\nlet h = Origin.Get();\n\
                    let c = C.Make(1);\nlet l = Later();\nlet i = Id(1);\nlet m = Sum(1, 2);\n\
                    let w = p.Make();\nlet o = Console.ReadLine();\n";
        assert_eq!(
            local_types(text),
            [
                "s: long",
                "p: P",
                "g: int",
                "h: int",
                "c: string",
                // Async, generic, `params`, static through a value, and the
                // standard library are not checked yet.
                "l: ?",
                "i: ?",
                "m: ?",
                "w: ?",
                "o: ?",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn arguments_are_checked_against_the_one_declaration() {
        let decls = "F(a: int, b: int = 2): int => a;\nB(x: byte) {}\n\
                     struct P {\n    X: int;\n    Set(v: int) {}\n}\n\
                     Local() {\n    G(x: byte): long => x;\n    let y: int = G(1);\n}\n";
        assert_eq!(reported(decls), ["MR0203"]);
        for (call, expected) in [
            ("let x = F(1);\nlet y = F(b: 3, a: 1);\nB(1);\n", vec![]),
            ("F(\"s\");\n", vec!["MR0203"]),
            ("B(300);\n", vec!["MR0010"]),
            ("F();\n", vec!["MR0205"]),
            ("F(1, 2, 3);\n", vec!["MR0205"]),
            ("F(c: 1);\n", vec!["MR0207"]),
            ("F(1, a: 2);\n", vec!["MR0207"]),
            ("F(a: 1, 2);\n", vec!["MR0207"]),
            ("F(a: 1, a: 2);\n", vec!["MR0207"]),
            ("F(p: 1, q: 2);\n", vec!["MR0207"]),
            ("UseP(p: P) {\n    p.Set(\"s\");\n}\n", vec!["MR0203"]),
        ] {
            let mut expected = expected;
            expected.insert(0, "MR0203");
            assert_eq!(reported(&format!("{decls}{call}")), expected, "{call}");
        }
        let messages =
            messages(&diagnostics_of(&format!("{decls}F();\nF(1, 2, 3);\nF(c: 1);\n"))[1..]);
        assert_eq!(
            messages,
            [
                "missing an argument for `a` in this call of `F(a: int, b: int = ...): int`",
                "too many arguments in this call of `F(a: int, b: int = ...): int`",
                "`F` has no parameter named `c`",
            ]
        );
    }

    #[test]
    fn overload_resolution_picks_the_best_applicable_overload() {
        let decls = "O(x: int): int => 1;\nO(x: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): string => \"\";\n\
                     Q(x: int, y: long) {}\nQ(x: long, y: int) {}\n\
                     D(x: int): int => 1;\nD(x: int, y: int = 0): string => \"\";\n";
        let text = format!(
            "{decls}let a = O(1);\nlet b = O(\"s\");\nlet c = P(1);\nlet d = P(1L);\n\
             let e = D(1);\nlet f = D(1, 2);\nlet u = O(Console.A());\n"
        );
        assert_eq!(
            local_types(&text),
            [
                "a: int",
                "b: string",
                "c: int",
                "d: string",
                "e: int",
                "f: string",
                "u: ?"
            ]
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        let n = "let n = 1;\n";
        for (call, code) in [
            ("O(true);\n", "MR0205"),
            ("Q(1, 1);\n", "MR0206"),
            ("Q(n, n);\n", "MR0206"),
        ] {
            assert_eq!(reported(&format!("{decls}{n}{call}")), [code], "{call}");
        }
        let reported = diagnostics_of(&format!("{decls}O(true);\nQ(1, 1);\n"));
        assert_eq!(
            reported[0].message,
            "no overload of `O` takes these arguments"
        );
        assert_eq!(reported[0].secondary.len(), 2);
        assert_eq!(reported[1].message, "the call of `Q` is ambiguous");
        assert_eq!(reported[1].secondary.len(), 2);
    }

    #[test]
    fn the_chosen_overload_is_recorded() {
        let db = TestDb::new();
        let text = "O(x: int): int => 1;\nO(x: string): string => \"\";\nlet a = O(\"s\");\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let call = body
            .exprs
            .iter()
            .find(|(_, expr)| matches!(expr, Expr::Call { .. }))
            .map(|(id, _)| id)
            .unwrap();
        let string = item_tree(&db, file)
            .items()
            .filter(|(_, item)| item.name == "O")
            .nth(1)
            .map(|(idx, _)| Scope::file(project, file).item(idx))
            .unwrap();
        assert_eq!(inference.callee(call), Some(Callee::Item(string)));
    }

    #[test]
    fn an_argument_marker_must_suit_the_parameter_mode() {
        let text = "Bump(ref x: int) {}\nRead(x: int) {}\nTake(move x: int) {}\n\
                    var n = 1;\nBump(n);\nBump(in n);\nRead(ref n);\nRead(move n);\n\
                    Take(n);\nTake(move n);\nRead(in n);\n";
        assert_eq!(reported(text), ["MR0205"; 4]);
        let fixed = apply_fixes(text);
        assert_eq!(fixed.edits, 4);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        let expected = "Bump(ref n);\nBump(ref n);\nRead(n);\nRead(n);\n";
        assert!(fixed.text.contains(expected), "{}", fixed.text);
        // A local function's modes are not recorded, so its markers are not
        // checked.
        assert_eq!(
            reported("F() {\n    G(x: int) {}\n    var n = 1;\n    G(ref n);\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_marker_fix_keeps_a_comment_and_needs_an_argument() {
        let fixed = apply_fixes("Read(x: int) {}\nvar n = 1;\nRead(move /* n */ n);\n");
        assert!(fixed.text.contains("Read(/* n */ n);"), "{}", fixed.text);
        // The parser reports the missing argument, so the call is not
        // checked and its marker reports nothing more.
        assert_eq!(
            reported("Read(x: int) {}\nRead(move );\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn calling_a_value_that_is_not_a_function_is_mr0208() {
        let text = "struct P {\n    X: int;\n}\nstatic Origin: P = new P { X = 1 };\n\
                    const Max: int = 1;\nlet n = 1;\nn();\nlet p = Origin;\np.X();\n\
                    Origin.X();\nMax();\n(1, 2)();\n";
        assert_eq!(reported(text), ["MR0208"; 5]);
        assert_eq!(
            messages(&diagnostics_of("let n = 1;\nn(2);\n")),
            ["cannot call a value of type `int`"]
        );
        // Calls of unknown things, and arguments inside them, still work.
        let unknown = "let f = (x: int) => x;\nf(1);\nlet s: string = Console.ReadLine(!1);\n";
        assert_eq!(reported(unknown), ["MR0204"]);
    }

    #[test]
    fn a_failed_call_still_types_its_literal_arguments() {
        let types = expr_types("F(a: int): int => a;\nF(1, 2);\n");
        assert!(types.contains(&"2: int".to_owned()), "{types:?}");
    }

    #[test]
    fn named_argument_messages_say_what_is_wrong() {
        let text = "Greet(name: string, greeting: string = \"Hi\") {}\n\
                    Greet(name: \"Ada\", name: \"Bo\");\nGreet(\"Ada\", name: \"Bo\");\n\
                    Greet(greeting: \"Hi\", \"Ada\");\n";
        assert_eq!(
            messages(&diagnostics_of(text)),
            [
                "the argument `name` is named twice",
                "the parameter `name` is given by position and by name",
                "a positional argument after a named one",
            ]
        );
    }

    #[test]
    fn a_local_function_default_value_is_its_own() {
        let text = "F() {\n    G(x: int, y: int = 1): int => x;\n    let z: string = G(1);\n}\n";
        assert_eq!(reported(text), ["MR0203"]);
    }

    #[test]
    fn signatures_in_messages_show_modes_and_return_types() {
        let text = "Bump(ref x: int) {}\nTake(move x: int): int => x;\nRead(x: int) {}\n\
                    var n = 1;\nBump(n);\nTake(in n);\nRead();\n";
        assert_eq!(
            messages(&diagnostics_of(text)),
            [
                "the argument for `ref x` in `Bump(ref x: int)` must be marked `ref`",
                "the parameter `x` of `Take(move x: int): int` does not take an argument marked `in`",
                "missing an argument for `x` in this call of `Read(x: int)`",
            ]
        );
    }

    #[test]
    fn overloads_with_unknown_or_marked_arguments() {
        let decls = "O(x: int): int => 1;\nO(x: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): string => \"\";\n\
                     R(x: double): int => 1;\nR(x: string): string => \"\";\n\
                     M(ref x: int): int => 1;\nM(x: string): string => \"\";\n\
                     struct S {\n    X: int;\n    Get(): int => X;\n}\n\
                     static Origin: S = new S { X = 1 };\n";
        let text = format!(
            "{decls}let g: int? = 1;\nlet a = O(g);\nlet b = P(Console.A());\nlet c = R(1.5);\n\
             var n = 1;\nlet d = M(ref n);\nlet s = Origin;\nlet e = s?.Get();\n"
        );
        assert_eq!(
            local_types(&text),
            [
                "g: _", "a: ?", "b: ?", "c: int", "n: int", "d: int", "s: S", "e: ?"
            ]
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        assert_eq!(reported(&format!("{decls}var n = 1;\nM(n);\n")), ["MR0205"]);
    }

    #[test]
    fn review_unknown_argument_leaves_overloads_unresolved() {
        let decls = "O(x: int, y: int): int => 1;\nO(x: long, y: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): int => 2;\n";
        let text = format!(
            "{decls}let s: string = O(1, Console.ReadLine());\nlet t = O(1, Console.ReadLine());\n\
             let u = P(Console.ReadLine());\n"
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        let types = local_types(&text);
        assert_eq!(types[1..], ["t: ?", "u: int"]);
    }

    #[test]
    fn review_a_literal_prefers_its_natural_type() {
        let text = "A(x: int): int => 1;\nA(x: uint): string => \"\";\n\
                    B(x: long): int => 1;\nB(x: float): string => \"\";\n\
                    C(x: byte): string => \"\";\nC(x: int): int => 1;\n\
                    D(x: float): string => \"\";\nD(x: double): int => 1;\n\
                    E(x: byte): int => 1;\nE(x: short): string => \"\";\n\
                    let a = A(1);\nlet b = B(1);\nlet c = C(1);\nlet d = D(0.1);\nlet e = E(1);\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            local_types(text),
            ["a: int", "b: int", "c: int", "d: int", "e: int"]
        );
    }

    #[test]
    fn review_an_async_callee_is_not_checked() {
        let text = "async Later(ref x: int): int => 1;\nvar n = 1;\nlet a = Later(n);\n\
                    let b = Later(\"s\", 2);\nF() {\n    async G(x: int): int => x;\n    G(\"s\");\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(local_types(text)[1..], ["a: ?", "b: ?"]);
    }

    #[test]
    fn review_named_argument_order_is_checked_for_every_call() {
        for text in [
            "Console.WriteLine(a: 1, 2);\n",
            "Id<T>(x: T): T => x;\nlet a = Id(x: 1, 2);\n",
        ] {
            assert_eq!(reported(text), ["MR0207"], "{text}");
        }
        assert_eq!(reported("let n = 1;\nn(a: 1, 2);\n"), ["MR0208", "MR0207"]);
    }

    #[test]
    fn review_a_recovered_argument_list_is_not_checked() {
        for call in ["F(,1);\n", "F(1,,2);\n", "F(;\n", "F(1\n"] {
            let db = TestDb::new();
            let project = app(
                &db,
                &[("src/Program.mr", &format!("F(a: int, b: int) {{}}\n{call}"))],
            );
            assert_eq!(
                codes(diagnostics(&db, project)),
                Vec::<String>::new(),
                "{call}"
            );
        }
    }

    #[test]
    fn review_ref_is_offered_only_for_a_place_of_a_fitting_type() {
        let text = "Bump(ref x: int) {}\nstruct P {\n    X: int;\n}\nvar n = 1;\n\
                    Bump(\"s\");\nBump(n + 1);\nBump(n);\n";
        let fixes: Vec<_> = diagnostics_of(text)
            .iter()
            .filter(|d| d.code.to_string() == "MR0205")
            .map(|d| d.fixes.len())
            .collect();
        assert_eq!(fixes, [0, 0, 1]);
        assert_eq!(
            reported("Bump(ref x: int) {}\nBump(ref 1);\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn review_a_name_every_overload_takes_by_position_is_mr0207() {
        let text = "O(x: int) {}\nO(x: string) {}\nO(1, x: 2);\nO(y: 1);\n";
        assert_eq!(reported(text), ["MR0207", "MR0205"]);
    }

    #[test]
    fn review_a_callee_has_its_own_type() {
        let types = expr_types("const Max: int = 1;\nMax();\n");
        assert!(types.contains(&"Max: int".to_owned()), "{types:?}");
    }

    #[test]
    fn review_a_signature_says_whether_it_is_generic() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[("src/Program.mr", "Id<T>(x: T): T => x;\nF(x: int) {}\n")],
        );
        for (name, generic) in [("Id", true), ("F", false)] {
            let signature = crate::test_db::signature_of(&db, project, "src/Program.mr", name);
            assert_eq!(
                crate::test_db::function(signature).is_generic,
                generic,
                "{name}"
            );
        }
    }

    #[test]
    fn ranking_does_not_depend_on_declaration_order() {
        let text = "E(x: short): string => \"\";\nE(x: byte): int => 1;\nlet e = E(1);\n\
                    H(x: int): int => 1;\nH(x: long): string => \"\";\n\
                    let h: int = H(18446744073709551615 * 18446744073709551615);\n";
        assert_eq!(local_types(text), ["e: int", "h: int"]);
        // The value too large to compute is still reported once the
        // overload is chosen.
        assert_eq!(reported(text), ["MR0010"]);
    }

    #[test]
    fn overloads_that_tie_are_ambiguous() {
        // Neither takes a default, and each fits one argument better.
        let text = "S(x: byte) {}\nS(x: sbyte) {}\nS(1);\n\
                    Q(x: byte, y: short) {}\nQ(x: short, y: byte) {}\nQ(1, 1);\n";
        assert_eq!(reported(text), ["MR0206", "MR0206"]);
    }

    #[test]
    fn the_one_applicable_overload_is_called_whatever_the_other_arguments() {
        let db = TestDb::new();
        let text = "O(x: int, y: byte): int => 1;\nO(x: string, y: byte): string => \"\";\n\
                    let a = O(Console.A(), 300);\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        // No overload takes `300` as its `byte`, whatever the argument of
        // unknown type is, so the call is MR0205.
        assert_eq!(codes(diagnostics(&db, project)), ["MR0205"]);
        let text = "O(x: int, y: byte): int => 1;\nO(x: string, y: string): string => \"\";\n\
                    let a = O(Console.A(), 1);\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let call = body
            .exprs
            .iter()
            .find(|(_, expr)| {
                matches!(expr, Expr::Call { callee, .. }
                if matches!(body.exprs[*callee], Expr::Path(_)))
            })
            .map(|(id, _)| id)
            .unwrap();
        let int = item_tree(&db, file)
            .items()
            .find(|(_, item)| item.name == "O")
            .map(|(idx, _)| Scope::file(project, file).item(idx))
            .unwrap();
        assert_eq!(inference.callee(call), Some(Callee::Item(int)));
        assert_eq!(local_types(text), ["a: int"]);
    }
}
