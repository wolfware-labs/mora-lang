//! The diagnostics for type errors: `MR0010`, `MR0185`, `MR0203` and `MR0204`,
//! those of calls, `MR0205` to `MR0208`, in [`call`], and those of memberwise
//! creation, `MR0143`, `MR0147` and `MR0148`, in [`struct_new`].

mod call;
mod struct_new;

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
    /// parentheses around it when it binds looser than `min`, so that they
    /// would otherwise split it.
    fn wrap(&self, expr: ExprId, min: Precedence, before: &str, after: &str) -> Vec<TextEdit> {
        let range = self.map.expr(expr);
        let file = self.file.id(self.db);
        let (before, after) = if precedence(&self.body.exprs[expr]) < min {
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
            self.wrap(expr, Precedence::Unary, &format!("({keyword})"), ""),
        )
    }

    /// The fix that compares the number `expr` with zero, giving a `bool`.
    fn compare_with_zero(&self, expr: ExprId) -> Fix {
        Fix::new(
            "compare it with zero",
            self.wrap(expr, Precedence::Relational, "", " != 0"),
        )
    }
}

/// The levels of §7.1, loosest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Precedence {
    AssignOrLambda,
    Conditional,
    Coalesce,
    Or,
    And,
    BitOr,
    BitXor,
    BitAnd,
    Equality,
    Relational,
    Shift,
    Additive,
    Multiplicative,
    With,
    Range,
    Unary,
    Primary,
}

/// The level of §7.1 that `expr` binds at.
fn precedence(expr: &Expr) -> Precedence {
    use BinaryOp as B;
    use Precedence as P;
    match expr {
        Expr::Assign { .. } | Expr::Lambda(_) => P::AssignOrLambda,
        Expr::Conditional { .. } => P::Conditional,
        Expr::Binary { op, .. } => match op {
            B::Coalesce => P::Coalesce,
            B::Or => P::Or,
            B::And => P::And,
            B::BitOr => P::BitOr,
            B::BitXor => P::BitXor,
            B::BitAnd => P::BitAnd,
            B::Eq | B::Ne => P::Equality,
            B::Lt | B::Gt | B::Le | B::Ge => P::Relational,
            B::Shl | B::Shr | B::UShr => P::Shift,
            B::Add | B::Sub | B::WrappingAdd | B::WrappingSub => P::Additive,
            B::Mul | B::Div | B::Rem | B::WrappingMul => P::Multiplicative,
        },
        Expr::Is { .. } | Expr::As { .. } => P::Relational,
        Expr::With { .. } => P::With,
        Expr::Range { .. } => P::Range,
        Expr::Unary { .. } | Expr::Cast { .. } | Expr::Await(_) => P::Unary,
        _ => P::Primary,
    }
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
        TypeError::Arity {
            expr,
            signature,
            missing,
        } => call::arity(source, *expr, signature, missing.as_deref()),
        TypeError::Mode {
            arg,
            signature,
            param,
            needs,
            given,
            fixable,
        } => call::mode(source, *arg, signature, param, *needs, *given, *fixable),
        TypeError::NoOverload {
            expr,
            name,
            candidates,
        } => call::no_overload(source, *expr, name, candidates),
        TypeError::Ambiguous {
            expr,
            name,
            candidates,
        } => call::ambiguous(source, *expr, name, candidates),
        TypeError::NamedArgument { arg, problem } => call::named(source, *arg, problem),
        TypeError::NotCallable { expr, ty } => call::not_callable(source, *expr, ty),
        TypeError::MissingField { expr, ty, field } => {
            struct_new::missing_field(source, *expr, ty, field)
        }
        TypeError::NotAField {
            value,
            ty,
            name,
            member,
            suggestion,
        } => struct_new::not_a_field(source, *value, ty, name, *member, suggestion.as_deref()),
        TypeError::RepeatedField {
            value,
            first,
            previous,
            ty,
            name,
        } => struct_new::repeated_field(source, [*first, *previous, *value], ty, name),
        TypeError::AssignmentValue { expr, split } => assignment_value(source, *expr, *split),
        TypeError::NotVisible { expr, item, from } => {
            call::not_visible(source, *expr, *item, *from)
        }
    }
}

/// `MR0185`: an assignment whose value is used. When `split` is the
/// statement of a block that `assignment` is the value of, the fix writes it
/// as a statement before that one.
fn assignment_value(source: &Source<'_>, assignment: ExprId, split: Option<ExprId>) -> Diagnostic {
    let Expr::Assign { lhs, .. } = &source.body.exprs[assignment] else {
        unreachable!("MR0185 is reported on an assignment");
    };
    let diagnostic = Diagnostic::error(
        codes::MR0185,
        "an assignment gives no value to use here",
        Label::new(
            source.span(assignment),
            "this assignment must stand as a statement of its own",
        ),
    )
    .with_note("assign first, then read the variable: an assignment is only a statement");
    let Some(statement) = split else {
        return diagnostic;
    };
    let file = source.file.id(source.db);
    let before = source.map.expr(statement).start();
    diagnostic.with_fix(Fix::new(
        "assign first, then use the variable",
        vec![
            TextEdit::insert(file, before, format!("{}; ", source.text(assignment))),
            TextEdit::replace(source.span(assignment), source.text(*lhs)),
        ],
    ))
}

/// `MR0204`: an operand of `&&` or `||` that is not a `bool`.
fn logical(source: &Source<'_>, op: BinaryOp, operand_expr: ExprId, operand: &Ty) -> Diagnostic {
    let t = source.name(operand);
    let diagnostic = Diagnostic::error(
        codes::MR0204,
        format!(
            "`{}` takes `bool` operands, and this one is `{t}`",
            op.text()
        ),
        Label::new(source.span(operand_expr), format!("this is `{t}`")),
    )
    .with_note("`&&` and `||` combine `bool` values; compare a number explicitly, as in `x != 0`");
    match numeric::as_numeric(operand) {
        Some(_) => diagnostic.with_fix(source.compare_with_zero(operand_expr)),
        None => diagnostic,
    }
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
    if context == Context::Ref {
        return diagnostic.with_note(
            "a `ref` argument is the caller's place itself, which the function reads and writes, \
             so it has exactly the parameter's type",
        );
    }
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
            Some(_) => diagnostic.with_fix(source.compare_with_zero(expr)),
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
                    source.wrap(expr, Precedence::Unary, "$\"{", "}\""),
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
        // An integer too large to compute is a different cause from a real
        // that becomes an infinity.
        let (label, note) = if constant::value(exprs, expr) == Some(constant::Value::OverflowReal) {
            (
                "an integer in it is too large to compute".to_owned(),
                "use smaller values",
            )
        } else {
            (
                format!("this would be infinity as a `{keyword}`"),
                "write it as a `double`, or use a smaller value",
            )
        };
        return Diagnostic::error(
            codes::MR0010,
            format!("{what} `{text}` is too large for a `{keyword}`"),
            Label::new(span, label),
        )
        .with_note(note);
    }
    match numeric::integer_range(ty) {
        Some((min, max)) => Diagnostic::error(
            codes::MR0010,
            format!("{what} `{text}` does not fit in `{keyword}`"),
            Label::new(span, "out of range"),
        )
        .with_note(match ty {
            // The range of every target, not only of this one.
            Primitive::NInt | Primitive::NUInt => {
                let (portable, wide) = if ty == Primitive::NInt {
                    (Primitive::Int, Primitive::Long)
                } else {
                    (Primitive::UInt, Primitive::ULong)
                };
                format!(
                    "a literal of type `{keyword}` must be from {min} to {max}, the range of \
                     `{}`, so that it is valid on every target; cast a `{}` for a larger value",
                    portable.keyword(),
                    wide.keyword(),
                )
            }
            _ => format!("`{keyword}` holds values from {min} to {max}"),
        }),
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
        // A cast cannot make an operator defined only on integers work on a
        // `float` or a `double`, so that is what is wrong, with no fix.
        if numeric::is_integer_only(op)
            && (numeric::is_floating_point(a) || numeric::is_floating_point(b))
        {
            return diagnostic.with_note(format!("`{sym}` is defined only on integer types"));
        }
        // Any other operator on two numbers fails only when they have no
        // common type.
        let native = |ty| matches!(ty, Primitive::NInt | Primitive::NUInt);
        let note = if native(a) || native(b) {
            "`nint` and `nuint` mix with other integer types only through an explicit cast"
                .to_owned()
        } else {
            format!("`{l}` and `{r}` have no common type that holds every value of both")
        };
        // The place of a compound assignment cannot be cast; its value can,
        // to the place's type: the operation then has two operands of one
        // numeric type, which every operator left here is defined on.
        let (side, to) = if compound { (true, a) } else { cast_side(a, b) };
        let operand = if side { rhs_expr } else { lhs_expr };
        return diagnostic
            .with_note(note)
            .with_fix(source.cast_fix(operand, to));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_db::{TestDb, app, file_named};
    use crate::{BodyOwner, Stmt, body, body_source_map};

    /// The initializer of each local of the top-level statements of
    /// `text`, with what is needed to make a fix for it.
    fn with_inits(text: &str, f: impl FnOnce(&Source<'_>, Vec<ExprId>)) {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let inits = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local { init, .. } => *init,
                _ => None,
            })
            .collect();
        let source = Source {
            db: &db,
            file,
            body,
            map,
        };
        f(&source, inits);
    }

    #[test]
    fn each_kind_of_expression_binds_at_its_level() {
        use Precedence as P;
        let cases = [
            ("y = 1", P::AssignOrLambda),
            ("() => 1", P::AssignOrLambda),
            ("c ? 1 : 2", P::Conditional),
            ("a ?? b", P::Coalesce),
            ("c || c", P::Or),
            ("c && c", P::And),
            ("a | b", P::BitOr),
            ("a ^ b", P::BitXor),
            ("a & b", P::BitAnd),
            ("a != b", P::Equality),
            ("a <= b", P::Relational),
            ("a is int", P::Relational),
            ("a as int", P::Relational),
            ("a >>> b", P::Shift),
            ("a -% b", P::Additive),
            ("a *% b", P::Multiplicative),
            ("p with { X = 1 }", P::With),
            ("a..b", P::Range),
            ("-a", P::Unary),
            ("(long)a", P::Unary),
            ("await a", P::Unary),
            ("a", P::Primary),
            ("f(a)", P::Primary),
        ];
        let text: String = cases
            .iter()
            .enumerate()
            .map(|(index, (expr, _))| format!("let v{index} = {expr};\n"))
            .collect();
        with_inits(&text, |source, inits| {
            let levels: Vec<_> = inits
                .iter()
                .map(|&init| precedence(&source.body.exprs[init]))
                .collect();
            let expected: Vec<_> = cases.iter().map(|&(_, level)| level).collect();
            assert!(levels == expected, "{:?}", cases.map(|(expr, _)| expr));
        });
        // The levels are ordered loosest first, as §7.1 lists them.
        let order = [
            P::AssignOrLambda,
            P::Conditional,
            P::Coalesce,
            P::Or,
            P::And,
            P::BitOr,
            P::BitXor,
            P::BitAnd,
            P::Equality,
            P::Relational,
            P::Shift,
            P::Additive,
            P::Multiplicative,
            P::With,
            P::Range,
            P::Unary,
            P::Primary,
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn wrap_adds_parentheses_only_below_the_level_it_needs() {
        with_inits("let a = 1;\nlet s = a + a;\n", |source, inits| {
            let sum = inits[1];
            let texts = |min| -> Vec<String> {
                source
                    .wrap(sum, min, "<", ">")
                    .into_iter()
                    .map(|edit| edit.replacement)
                    .collect()
            };
            // At its own level, `a + a` needs no parentheses.
            assert_eq!(texts(Precedence::Additive), ["<", ">"]);
            assert_eq!(texts(Precedence::Shift), ["<", ">"]);
            // Below the level it needs, it gets them.
            assert_eq!(texts(Precedence::Multiplicative), ["<(", ")>"]);
        });
    }
}
