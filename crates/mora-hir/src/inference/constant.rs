//! The value of an operation made only of unsuffixed literals, which takes
//! its target type like one literal (§3.5). This is not constant
//! evaluation (§14.6): it reads literals and arithmetic, nothing else.

use la_arena::Arena;
use mora_syntax::lexer::number::{IntSuffix, RealSuffix, int_value, real_value};

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
        Expr::Literal(Literal::Real(text)) => match real_value(text)? {
            (value, RealSuffix::None) => Some(Value::Real(value)),
            _ => None,
        },
        Expr::Unary { op, expr } => {
            let value = value(exprs, *expr)?;
            match (op, value) {
                (UnaryOp::Plus, _) | (_, Value::Overflow) => Some(value),
                (UnaryOp::Neg, Value::Int(v)) => Some(Value::Int(-v)),
                (UnaryOp::Neg, Value::Real(v)) => Some(Value::Real(-v)),
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
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            let value = match op {
                B::Add => a.checked_add(b),
                B::Sub => a.checked_sub(b),
                B::Mul => a.checked_mul(b),
                // Division by zero panics when the program runs (§7.12).
                B::Div if b == 0 => return None,
                B::Rem if b == 0 => return None,
                B::Div => a.checked_div(b),
                B::Rem => a.checked_rem(b),
                B::BitAnd => Some(a & b),
                B::BitOr => Some(a | b),
                B::BitXor => Some(a ^ b),
                _ => return None,
            };
            Some(value.map_or(Value::Overflow, Value::Int))
        }
        (Value::Overflow, _) | (_, Value::Overflow) => match op {
            B::Add | B::Sub | B::Mul | B::Div | B::Rem | B::BitAnd | B::BitOr | B::BitXor => {
                Some(Value::Overflow)
            }
            _ => None,
        },
        (a, b) => {
            let (a, b) = (real(a), real(b));
            let value = match op {
                B::Add => a + b,
                B::Sub => a - b,
                B::Mul => a * b,
                B::Div => a / b,
                B::Rem => a % b,
                _ => return None,
            };
            Some(Value::Real(value))
        }
    }
}

fn real(value: Value) -> f64 {
    match value {
        // Exact for the integers that matter here; the result is only
        // checked against the range of a `float` or a `double`.
        Value::Int(value) => value as f64,
        Value::Real(value) => value,
        Value::Overflow => f64::INFINITY,
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
}
