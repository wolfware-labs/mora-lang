//! The diagnostics for type errors: `MR0010`, `MR0203` and `MR0204`.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Db, SourceFile, Span};

use super::{Context, TypeError, constant, numeric, ty_name};
use crate::bodies::{BinaryOp, Body, BodySourceMap, Expr, ExprId, Literal, UnaryOp};
use crate::ty::{Primitive, Ty};

/// Where the parts of a body are, for the diagnostics of its type errors.
pub(crate) struct Source<'a> {
    /// The database the names of types are read from.
    pub(crate) db: &'a dyn Db,
    /// The file of the body.
    pub(crate) file: SourceFile,
    /// The body.
    pub(crate) body: &'a Body,
    /// Where its expressions are.
    pub(crate) map: &'a BodySourceMap,
}

impl Source<'_> {
    fn span(&self, expr: ExprId) -> Span {
        Span::new(self.file.id(self.db), self.map.expr(expr))
    }

    fn text(&self, expr: ExprId) -> &str {
        self.file
            .text(self.db)
            .get(std::ops::Range::<usize>::from(self.map.expr(expr)))
            .unwrap_or_default()
    }

    fn name(&self, ty: &Ty) -> String {
        ty_name(self.db, ty)
    }

    /// The edits that put `before` and `after` around `expr`, with
    /// parentheses around an operation, which would otherwise bind
    /// differently.
    fn wrap(&self, expr: ExprId, before: &str, after: &str) -> Vec<TextEdit> {
        let range = self.map.expr(expr);
        let file = self.file.id(self.db);
        let (before, after) = if is_operation(&self.body.exprs[expr]) {
            (format!("{before}("), format!("){after}"))
        } else {
            (before.to_owned(), after.to_owned())
        };
        [(range.start(), before), (range.end(), after)]
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(offset, text)| TextEdit::insert(file, offset, text))
            .collect()
    }

    /// The fix that casts `expr` to `ty`.
    fn cast_fix(&self, expr: ExprId, ty: Primitive) -> Fix {
        let keyword = ty.keyword();
        Fix::new(
            format!("convert it explicitly with `({keyword})`"),
            self.wrap(expr, &format!("({keyword})"), ""),
        )
    }
}

/// Whether `expr` is an operation below the unary operators, which a cast
/// or a comparison put before or after it would split.
fn is_operation(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Binary { .. }
            | Expr::Assign { .. }
            | Expr::Conditional { .. }
            | Expr::Is { .. }
            | Expr::As { .. }
            | Expr::Range { .. }
            | Expr::Lambda(_)
            | Expr::With { .. }
    )
}

/// The diagnostic for a type error of a body.
pub(crate) fn type_error(source: &Source<'_>, error: &TypeError) -> Diagnostic {
    match error {
        TypeError::Mismatch {
            expr,
            expected,
            found,
            context,
        } => mismatch(source, *expr, expected, found, *context),
        TypeError::Literal { expr, ty } => literal(source, *expr, *ty),
        TypeError::Binary {
            expr,
            op,
            lhs,
            rhs,
            lhs_ty,
            rhs_ty,
        } => binary(source, *expr, *op, (*lhs, lhs_ty), (*rhs, rhs_ty)),
        TypeError::Logical { op, operand, ty } => logical(source, *op, *operand, ty),
        TypeError::Unary {
            expr,
            op,
            operand,
            ty,
        } => unary(source, *expr, *op, *operand, ty),
        TypeError::Cast {
            expr,
            operand,
            from,
            to,
        } => cast(source, *expr, *operand, from, to),
    }
}

/// `MR0204`: an operand of `&&` or `||` that is not a `bool`.
fn logical(source: &Source<'_>, op: BinaryOp, operand_expr: ExprId, operand: &Ty) -> Diagnostic {
    let t = source.name(operand);
    Diagnostic::error(
        codes::MR0204,
        format!(
            "`{}` takes `bool` operands, and this one is `{t}`",
            op.text()
        ),
        Label::new(source.span(operand_expr), format!("this is `{t}`")),
    )
    .with_note("`&&` and `||` combine `bool` values; compare a number explicitly, as in `x != 0`")
}

/// `MR0203`: a value whose type does not convert to the expected one.
fn mismatch(
    source: &Source<'_>,
    expr: ExprId,
    expected: &Ty,
    found: &Ty,
    context: Context,
) -> Diagnostic {
    let (e, f) = (source.name(expected), source.name(found));
    let diagnostic = Diagnostic::error(
        codes::MR0203,
        format!("mismatched types: expected `{e}`, found `{f}`"),
        Label::new(source.span(expr), format!("this is `{f}`")),
    );
    let (found_number, expected_number) =
        (numeric::as_numeric(found), numeric::as_numeric(expected));
    let found = match found {
        Ty::Primitive(ty) => Some(*ty),
        _ => None,
    };
    if let Context::Branch(other) = context {
        return diagnostic
            .with_secondary(Label::new(source.span(other), format!("this is `{e}`")))
            .with_note(
                "the branches of `c ? a : b` must have one type that the other converts to",
            );
    }
    if context == Context::Condition {
        let diagnostic = diagnostic.with_note("a condition must be a `bool`");
        return match found_number {
            Some(_) => diagnostic.with_fix(Fix::new(
                "compare it with zero",
                source.wrap(expr, "", " != 0"),
            )),
            None => diagnostic,
        };
    }
    if let (Some(_), Some(to)) = (found_number, expected_number) {
        return diagnostic
            .with_note(format!(
                "`{f}` converts to `{e}` only through an explicit cast, because not every value of `{f}` is a value of `{e}`"
            ))
            .with_fix(source.cast_fix(expr, to));
    }
    match (found, expected) {
        (Some(Primitive::Bool), Ty::Primitive(_)) | (_, Ty::Primitive(Primitive::Bool)) => {
            diagnostic.with_note("there is no implicit conversion to or from `bool`")
        }
        (Some(Primitive::Char), Ty::Primitive(_)) | (_, Ty::Primitive(Primitive::Char)) => {
            diagnostic.with_note("there is no implicit conversion to or from `char`")
        }
        (_, Ty::String) => {
            let diagnostic = diagnostic.with_note(
                "nothing converts to `string` implicitly; interpolation formats a value as text",
            );
            match found {
                Some(_) => diagnostic.with_fix(Fix::new(
                    "format it with interpolation",
                    source.wrap(expr, "$\"{", "}\""),
                )),
                None => diagnostic,
            }
        }
        _ => diagnostic,
    }
}

/// `MR0204`: a cast between types that are not both numeric.
fn cast(source: &Source<'_>, expr: ExprId, operand: ExprId, from: &Ty, to: &Ty) -> Diagnostic {
    let char_and_integer = |a: &Ty, b: &Ty| {
        *a == Ty::Primitive(Primitive::Char)
            && numeric::as_numeric(b).is_some_and(numeric::is_integer)
    };
    let note = if char_and_integer(from, to) || char_and_integer(to, from) {
        "a cast `(T)x` converts only from one numeric type to another, and a `char` is not a number; convert between `char` and integers with a named conversion function"
    } else {
        "a cast `(T)x` converts only from one numeric type to another"
    };
    let (from, to) = (source.name(from), source.name(to));
    Diagnostic::error(
        codes::MR0204,
        format!("cannot cast `{from}` to `{to}`"),
        Label::new(source.span(expr), "not a numeric conversion"),
    )
    .with_secondary(Label::new(
        source.span(operand),
        format!("this is `{from}`"),
    ))
    .with_note(note)
}

/// `MR0010`: a literal that does not fit the type it takes.
fn literal(source: &Source<'_>, expr: ExprId, ty: Option<Primitive>) -> Diagnostic {
    let text = source.text(expr);
    let span = source.span(expr);
    let Some(ty) = ty else {
        return Diagnostic::error(
            codes::MR0010,
            format!("the literal `{text}` does not fit in any integer type"),
            Label::new(span, "out of range"),
        )
        .with_note(format!(
            "an integer literal is at least {} and at most {}",
            i64::MIN,
            u64::MAX
        ));
    };
    let keyword = ty.keyword();
    // A literal, a negated literal, or an operation made only of literals.
    let exprs = &source.body.exprs;
    let mut real = false;
    let mut leaves = 0;
    constant::parts(exprs, expr, &mut |part| {
        if let Expr::Literal(literal) = &exprs[part] {
            leaves += 1;
            real |= matches!(literal, Literal::Real(_));
        }
    });
    let what = if leaves > 1 {
        "the value of"
    } else {
        "the literal"
    };
    if real {
        return Diagnostic::error(
            codes::MR0010,
            format!("{what} `{text}` is too large for a `{keyword}`"),
            Label::new(span, format!("this would be infinity as a `{keyword}`")),
        )
        .with_note("write it as a `double`, or use a smaller value");
    }
    match numeric::integer_range(ty) {
        Some((min, max)) => Diagnostic::error(
            codes::MR0010,
            format!("{what} `{text}` does not fit in `{keyword}`"),
            Label::new(span, "out of range"),
        )
        .with_note(format!("`{keyword}` holds values from {min} to {max}")),
        None => Diagnostic::error(
            codes::MR0010,
            format!("{what} `{text}` is not exact as a `{keyword}`"),
            Label::new(span, format!("a `{keyword}` would round it")),
        )
        .with_note({
            // The largest power of two up to which every integer is exact.
            let digits = if ty == Primitive::Float {
                f32::MANTISSA_DIGITS
            } else {
                f64::MANTISSA_DIGITS
            };
            let limit = 1u64 << digits;
            format!(
                "a `{keyword}` holds every integer from -{limit} to {limit} exactly, and only some beyond"
            )
        }),
    }
}

/// `MR0204`: a binary operator that is not defined for its operands.
fn binary(
    source: &Source<'_>,
    expr: ExprId,
    op: BinaryOp,
    (lhs_expr, lhs): (ExprId, &Ty),
    (rhs_expr, rhs): (ExprId, &Ty),
) -> Diagnostic {
    // A compound assignment is named as written, `+=`.
    let compound = matches!(source.body.exprs[expr], Expr::Assign { .. });
    let sym = if compound {
        format!("{}=", op.text())
    } else {
        op.text().to_owned()
    };
    let (l, r) = (source.name(lhs), source.name(rhs));
    if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr) {
        return Diagnostic::error(
            codes::MR0204,
            format!("`{sym}` cannot shift a `{l}`"),
            Label::unlabeled(source.span(expr)),
        )
        .with_secondary(Label::new(source.span(lhs_expr), format!("this is `{l}`")))
        .with_note("only a value of an integer type can be shifted");
    }
    let diagnostic = Diagnostic::error(
        codes::MR0204,
        format!("`{sym}` is not defined for `{l}` and `{r}`"),
        Label::unlabeled(source.span(expr)),
    )
    .with_secondary(Label::new(source.span(lhs_expr), format!("this is `{l}`")))
    .with_secondary(Label::new(source.span(rhs_expr), format!("this is `{r}`")));
    if let (Some(a), Some(b)) = (numeric::as_numeric(lhs), numeric::as_numeric(rhs)) {
        return match numeric::promote(a, b) {
            // Defined only on integers.
            Some(_) => diagnostic.with_note(format!("`{sym}` is defined only on integer types")),
            None => {
                let native = |ty| matches!(ty, Primitive::NInt | Primitive::NUInt);
                let note = if native(a) || native(b) {
                    "`nint` and `nuint` mix with other integer types only through an explicit cast"
                        .to_owned()
                } else {
                    format!("`{l}` and `{r}` have no common type that holds every value of both")
                };
                let diagnostic = diagnostic.with_note(note);
                // A cast cannot make an operator defined only on integers
                // work on a `float` or a `double`.
                if numeric::is_integer_only(op)
                    && (numeric::is_floating_point(a) || numeric::is_floating_point(b))
                {
                    return diagnostic;
                }
                // The place of a compound assignment cannot be cast; its
                // value can, to the place's type: the operation then has two
                // operands of one numeric type, which every operator left
                // here is defined on.
                let (side, to) = if compound { (true, a) } else { cast_side(a, b) };
                let operand = if side { rhs_expr } else { lhs_expr };
                diagnostic.with_fix(source.cast_fix(operand, to))
            }
        };
    }
    match (lhs, rhs) {
        (Ty::String, _) | (_, Ty::String) if op == BinaryOp::Add => diagnostic.with_note(
            "`+` joins two `string` values; interpolation formats other values as text, as in `$\"{a}{b}\"`",
        ),
        (Ty::Primitive(Primitive::Bool), Ty::Primitive(Primitive::Bool)) => {
            diagnostic.with_note("`bool` values are not ordered and are not numbers")
        }
        _ => diagnostic,
    }
}

/// Which operand of a mixed numeric operation without a common type to
/// cast, `true` for the right one, and the type to cast it to: `double`
/// when one is a floating-point type, else the wider type, or the signed
/// one of two as wide, or the left one's.
fn cast_side(a: Primitive, b: Primitive) -> (bool, Primitive) {
    if numeric::is_floating_point(a) || numeric::is_floating_point(b) {
        return (numeric::is_floating_point(a), Primitive::Double);
    }
    let a_first =
        (numeric::bits(a), numeric::is_signed(a)) >= (numeric::bits(b), numeric::is_signed(b));
    if a_first { (true, a) } else { (false, b) }
}

/// `MR0204`: a unary operator that is not defined for its operand.
fn unary(
    source: &Source<'_>,
    expr: ExprId,
    op: UnaryOp,
    operand_expr: ExprId,
    operand: &Ty,
) -> Diagnostic {
    let sym = op.text();
    let t = source.name(operand);
    let diagnostic = Diagnostic::error(
        codes::MR0204,
        format!("`{sym}` is not defined for `{t}`"),
        Label::new(source.span(expr), format!("the operand is `{t}`")),
    );
    match (op, operand) {
        (UnaryOp::Not, _) => diagnostic
            .with_note("`!` negates a `bool`; compare a number explicitly, as in `x == 0`"),
        (UnaryOp::Neg, Ty::Primitive(ty @ (Primitive::ULong | Primitive::NUInt))) => {
            let signed = if *ty == Primitive::ULong {
                Primitive::Long
            } else {
                Primitive::NInt
            };
            diagnostic
                .with_note(format!(
                    "the negation of a `{t}` may not fit in any integer type; cast it to a signed type first"
                ))
                .with_fix(source.cast_fix(operand_expr, signed))
        }
        (UnaryOp::BitNot, Ty::Primitive(_)) => {
            diagnostic.with_note("`~` is defined only on integer types")
        }
        _ => diagnostic.with_note(format!("`{sym}` is defined only on numeric types")),
    }
}
