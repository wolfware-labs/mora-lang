//! The value of an operation made only of unsuffixed literals, which takes
//! its target type like one literal (§3.5). This is not constant
//! evaluation (§14.6): it reads literals and arithmetic, nothing else.

use la_arena::Arena;
use mora_syntax::lexer::number::{IntSuffix, RealSuffix, int_value, real_value};

use super::numeric;
use crate::bodies::{BinaryOp, Expr, ExprId, Literal, UnaryOp};

/// The value of a literal operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Value {
    /// An integer, exact.
    Int(i128),
    /// A real, as a `double` computes it.
    Real(f64),
    /// An integer too large to compute, which fits no type.
    Overflow,
    /// A real whose computation gave an infinity at some step, which fits
    /// no type (§3.5).
    Infinity,
    /// A real computed from an integer too large to compute, which fits no
    /// type, and is kept apart from [`Value::Infinity`] for its message.
    OverflowReal,
}

/// The value of `expr` when every leaf of it is an unsuffixed literal
/// and every operator is unary `-`, `+` or `~`, or binary `+`, `-`, `*`,
/// `/`, `%`, `&`, `|` or `^`. `None` for anything else, an integer division
/// by zero, and `~`, `&`, `|` or `^` on a real.
pub(crate) fn value(exprs: &Arena<Expr>, expr: ExprId) -> Option<Value> {
    match &exprs[expr] {
        Expr::Literal(Literal::Int(text)) => match int_value(text)? {
            (value, IntSuffix::None) => Some(Value::Int(value.into())),
            _ => None,
        },
        // A real too large for a `double` is reported by the lexer.
        Expr::Literal(Literal::Real(text)) => match real_value(text)? {
            (value, RealSuffix::None) if value.is_finite() => Some(Value::Real(value)),
            _ => None,
        },
        Expr::Unary { op, expr } => {
            let value = value(exprs, *expr)?;
            match (op, value) {
                (UnaryOp::Plus, _) | (UnaryOp::Neg | UnaryOp::BitNot, Value::Overflow) => {
                    Some(value)
                }
                // `-i128::MIN` is too large to compute.
                (UnaryOp::Neg, Value::Int(v)) => {
                    Some(v.checked_neg().map_or(Value::Overflow, Value::Int))
                }
                (UnaryOp::Neg, Value::Real(v)) => Some(Value::Real(-v)),
                (UnaryOp::Neg, Value::Infinity | Value::OverflowReal) => Some(value),
                (UnaryOp::BitNot, Value::Int(v)) => Some(Value::Int(!v)),
                _ => None,
            }
        }
        Expr::Binary { op, lhs, rhs } => binary(*op, value(exprs, *lhs)?, value(exprs, *rhs)?),
        _ => None,
    }
}

fn binary(op: BinaryOp, a: Value, b: Value) -> Option<Value> {
    use BinaryOp as B;
    // An integer division by zero panics when the program runs (§7.12),
    // so it is not folded; a real divided by zero is an infinity.
    if matches!(op, B::Div | B::Rem)
        && matches!(a, Value::Int(_) | Value::Overflow)
        && b == Value::Int(0)
    {
        return None;
    }
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            let value = match op {
                B::Add => a.checked_add(b),
                B::Sub => a.checked_sub(b),
                B::Mul => a.checked_mul(b),
                B::Div => a.checked_div(b),
                // The divisor is not zero, and `i128::MIN % -1` is 0.
                B::Rem => Some(a.wrapping_rem(b)),
                B::BitAnd => Some(a & b),
                B::BitOr => Some(a | b),
                B::BitXor => Some(a ^ b),
                _ => return None,
            };
            Some(value.map_or(Value::Overflow, Value::Int))
        }
        _ => match (real(a), real(b)) {
            (Some(a), Some(b)) => {
                let value = match op {
                    B::Add => a + b,
                    B::Sub => a - b,
                    B::Mul => a * b,
                    B::Div => a / b,
                    B::Rem => a % b,
                    _ => return None,
                };
                // NaN fits, as `0.0 / 0.0` gives; an infinity never does,
                // even when a later step would make the result finite.
                Some(if value.is_infinite() {
                    Value::Infinity
                } else {
                    Value::Real(value)
                })
            }
            // An operand too large to compute makes the result so too;
            // `&`, `|` and `^` are not defined on a real at all.
            _ => {
                let is_real = |value| {
                    matches!(
                        value,
                        Value::Real(_) | Value::Infinity | Value::OverflowReal
                    )
                };
                let real = is_real(a) || is_real(b);
                let infinity = a == Value::Infinity || b == Value::Infinity;
                match op {
                    B::Add | B::Sub | B::Mul | B::Div | B::Rem if infinity => Some(Value::Infinity),
                    B::Add | B::Sub | B::Mul | B::Div | B::Rem if real => Some(Value::OverflowReal),
                    B::Add | B::Sub | B::Mul | B::Div | B::Rem => Some(Value::Overflow),
                    B::BitAnd | B::BitOr | B::BitXor if !real => Some(Value::Overflow),
                    _ => None,
                }
            }
        },
    }
}

/// `value` as a real, `None` for a value too large to compute. An
/// integer converts exactly where it matters: the result is only checked
/// against the range of a `float` or a `double`.
fn real(value: Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(value as f64),
        Value::Real(value) => Some(value),
        Value::Overflow | Value::Infinity | Value::OverflowReal => None,
    }
}

/// Whether every leaf of `expr` is an unsuffixed literal and every operator
/// one of an arithmetic or bitwise operation, the wrapping ones included:
/// what takes a target type like one literal (§3.5).
pub(crate) fn literal_only(exprs: &Arena<Expr>, expr: ExprId) -> bool {
    use BinaryOp as B;
    match &exprs[expr] {
        Expr::Literal(Literal::Int(text)) => {
            matches!(int_value(text), Some((_, IntSuffix::None)))
        }
        Expr::Literal(Literal::Real(text)) => {
            matches!(real_value(text), Some((_, RealSuffix::None)))
        }
        Expr::Unary {
            op: UnaryOp::Plus | UnaryOp::Neg | UnaryOp::BitNot,
            expr,
        } => literal_only(exprs, *expr),
        Expr::Binary { op, lhs, rhs } => {
            (matches!(op, B::Add | B::Sub | B::Mul | B::Div | B::Rem)
                || numeric::is_integer_only(*op))
                && literal_only(exprs, *lhs)
                && literal_only(exprs, *rhs)
        }
        _ => false,
    }
}

/// Calls `f` with every expression of the literal operation `expr`.
pub(crate) fn parts(exprs: &Arena<Expr>, expr: ExprId, f: &mut impl FnMut(ExprId)) {
    f(expr);
    match &exprs[expr] {
        Expr::Unary { expr, .. } => parts(exprs, *expr, f),
        Expr::Binary { lhs, rhs, .. } => {
            parts(exprs, *lhs, f);
            parts(exprs, *rhs, f);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value of a binary operation on two values.
    fn of(op: BinaryOp, a: Value, b: Value) -> Option<Value> {
        binary(op, a, b)
    }

    #[test]
    fn integers_are_exact_and_overflow_is_kept() {
        use Value::{Int, Overflow, Real};
        assert_eq!(of(BinaryOp::Add, Int(200), Int(100)), Some(Int(300)));
        assert_eq!(of(BinaryOp::Div, Int(7), Int(2)), Some(Int(3)));
        assert_eq!(of(BinaryOp::Div, Int(7), Int(0)), None);
        assert_eq!(of(BinaryOp::BitXor, Int(6), Int(3)), Some(Int(5)));
        assert_eq!(of(BinaryOp::Mul, Int(i128::MAX), Int(2)), Some(Overflow));
        assert_eq!(of(BinaryOp::Add, Overflow, Int(1)), Some(Overflow));
        assert_eq!(of(BinaryOp::Mul, Real(1.5), Int(2)), Some(Real(3.0)));
        assert_eq!(of(BinaryOp::BitAnd, Real(1.5), Int(2)), None);
        assert_eq!(of(BinaryOp::Shl, Int(1), Int(2)), None);
    }

    #[test]
    fn each_integer_operator_computes_its_value() {
        use Value::Int;
        assert_eq!(of(BinaryOp::Rem, Int(7), Int(4)), Some(Int(3)));
        assert_eq!(of(BinaryOp::Rem, Int(7), Int(0)), None);
        assert_eq!(of(BinaryOp::Rem, Int(i128::MIN), Int(-1)), Some(Int(0)));
        assert_eq!(
            of(BinaryOp::Div, Int(i128::MIN), Int(-1)),
            Some(Value::Overflow)
        );
        assert_eq!(of(BinaryOp::BitOr, Int(6), Int(3)), Some(Int(7)));
        assert_eq!(of(BinaryOp::Sub, Int(6), Int(3)), Some(Int(3)));
    }

    #[test]
    fn each_real_operator_computes_its_value() {
        use Value::{Int, Real};
        assert_eq!(of(BinaryOp::Add, Real(1.5), Int(2)), Some(Real(3.5)));
        assert_eq!(of(BinaryOp::Sub, Real(1.5), Real(0.5)), Some(Real(1.0)));
        assert_eq!(of(BinaryOp::Div, Real(3.0), Int(2)), Some(Real(1.5)));
        assert_eq!(of(BinaryOp::Rem, Real(3.5), Int(2)), Some(Real(1.5)));
    }

    #[test]
    fn an_operand_too_large_to_compute_overflows_the_result() {
        use Value::{Int, Overflow, Real};
        assert_eq!(of(BinaryOp::Mul, Overflow, Int(3)), Some(Overflow));
        assert_eq!(of(BinaryOp::BitOr, Int(1), Overflow), Some(Overflow));
        assert_eq!(of(BinaryOp::BitAnd, Real(1.5), Overflow), None);
        assert_eq!(of(BinaryOp::BitXor, Overflow, Real(1.5)), None);
        assert_eq!(of(BinaryOp::Shl, Overflow, Int(1)), None);
        assert_eq!(of(BinaryOp::Div, Overflow, Int(0)), None);
        assert_eq!(of(BinaryOp::Rem, Overflow, Int(0)), None);
        assert_eq!(of(BinaryOp::Div, Overflow, Int(2)), Some(Overflow));
        assert_eq!(of(BinaryOp::Div, Real(1.5), Int(0)), Some(Value::Infinity));
    }

    #[test]
    fn an_infinity_at_any_step_stays_an_infinity() {
        use Value::{Infinity, Int, Overflow, Real};
        assert_eq!(of(BinaryOp::Mul, Real(1e308), Real(10.0)), Some(Infinity));
        assert_eq!(of(BinaryOp::Sub, Infinity, Infinity), Some(Infinity));
        assert_eq!(of(BinaryOp::Mul, Infinity, Real(0.0)), Some(Infinity));
        assert_eq!(
            of(BinaryOp::Add, Overflow, Real(1.5)),
            Some(Value::OverflowReal)
        );
        assert_eq!(
            of(BinaryOp::Mul, Value::OverflowReal, Real(2.0)),
            Some(Value::OverflowReal)
        );
        assert_eq!(
            of(BinaryOp::Sub, Value::OverflowReal, Infinity),
            Some(Infinity)
        );
        assert_eq!(of(BinaryOp::BitOr, Value::OverflowReal, Int(1)), None);
        assert_eq!(of(BinaryOp::BitAnd, Infinity, Int(1)), None);
        assert!(matches!(of(BinaryOp::Div, Real(0.0), Real(0.0)), Some(Real(v)) if v.is_nan()));
    }

    #[test]
    fn only_literals_and_arithmetic_or_bitwise_operators_are_literal_only() {
        let mut exprs = Arena::new();
        let int = exprs.alloc(Expr::Literal(Literal::Int("1".to_owned())));
        let real = exprs.alloc(Expr::Literal(Literal::Real("1.5".to_owned())));
        let suffixed = exprs.alloc(Expr::Literal(Literal::Int("1u".to_owned())));
        let float = exprs.alloc(Expr::Literal(Literal::Real("1.5f".to_owned())));
        let other = exprs.alloc(Expr::This);
        let mut binary = |op, lhs, rhs| exprs.alloc(Expr::Binary { op, lhs, rhs });
        let bits = binary(BinaryOp::BitAnd, int, real);
        let wrapping = binary(BinaryOp::WrappingAdd, int, int);
        let left_other = binary(BinaryOp::Add, other, int);
        let right_other = binary(BinaryOp::Add, int, other);
        let shift = binary(BinaryOp::Shl, int, int);
        let mut unary = |op, expr| exprs.alloc(Expr::Unary { op, expr });
        let not = unary(UnaryOp::BitNot, int);
        let logical = unary(UnaryOp::Not, int);
        let only = |expr| literal_only(&exprs, expr);
        assert!(only(int) && only(real) && only(bits) && only(wrapping) && only(not));
        for expr in [
            suffixed,
            float,
            other,
            left_other,
            right_other,
            shift,
            logical,
        ] {
            assert!(!only(expr));
        }
    }

    #[test]
    fn unary_operators_apply_to_the_value() {
        let mut exprs = Arena::new();
        let mut literal = |literal| exprs.alloc(Expr::Literal(literal));
        let real = literal(Literal::Real("1.5".to_owned()));
        let int = literal(Literal::Int("6".to_owned()));
        let suffixed = literal(Literal::Int("6u".to_owned()));
        let float = literal(Literal::Real("1.5f".to_owned()));
        let mut unary = |op, expr| exprs.alloc(Expr::Unary { op, expr });
        let neg = unary(UnaryOp::Neg, real);
        let not = unary(UnaryOp::BitNot, int);
        let plus = unary(UnaryOp::Plus, int);
        let not_real = unary(UnaryOp::BitNot, real);
        let neg_int = unary(UnaryOp::Neg, int);
        assert_eq!(value(&exprs, neg), Some(Value::Real(-1.5)));
        assert_eq!(value(&exprs, not), Some(Value::Int(-7)));
        assert_eq!(value(&exprs, plus), Some(Value::Int(6)));
        assert_eq!(value(&exprs, neg_int), Some(Value::Int(-6)));
        assert_eq!(value(&exprs, not_real), None);
        assert_eq!(value(&exprs, suffixed), None);
        assert_eq!(value(&exprs, float), None);
        let mut seen = Vec::new();
        parts(&exprs, neg, &mut |part| seen.push(part));
        assert_eq!(seen, [neg, real]);
    }
}
