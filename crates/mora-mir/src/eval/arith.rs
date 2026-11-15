//! The arithmetic of constant evaluation on scalar values. Its semantics
//! are those of the program at run time with every checked operation
//! checked, in every build profile (§14.6): a checked operation that
//! overflows, and an integer division or remainder by zero, is an error
//! rather than a wrapped value or a panic.

use std::ops::{Add, Div, Mul, Rem, Sub};

use mora_hir::Primitive;

use crate::{BinOp, Const, UnOp};

/// Why an operation has no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArithError {
    /// A checked operation whose result does not fit its type.
    Overflow,
    /// An integer division by zero.
    DivisionByZero,
    /// An integer remainder by zero.
    RemainderByZero,
    /// A result of type `nint` or `nuint` that differs between targets.
    TargetDependent,
    /// A string longer than [`MAX_STRING_BYTES`], with its length in bytes.
    TooLong(usize),
}

/// The most bytes a string computed at compile time may hold.
pub(crate) const MAX_STRING_BYTES: usize = 1 << 24;

/// An integer type on one target: how many bits it has and whether it is
/// signed.
#[derive(Clone, Copy)]
struct Width {
    bits: u32,
    signed: bool,
}

impl Width {
    const I8: Width = Width::signed(8);
    const U8: Width = Width::unsigned(8);
    const I16: Width = Width::signed(16);
    const U16: Width = Width::unsigned(16);
    const I32: Width = Width::signed(32);
    const U32: Width = Width::unsigned(32);
    const I64: Width = Width::signed(64);
    const U64: Width = Width::unsigned(64);

    const fn signed(bits: u32) -> Width {
        Width { bits, signed: true }
    }

    const fn unsigned(bits: u32) -> Width {
        Width {
            bits,
            signed: false,
        }
    }

    /// The smallest value of the type.
    fn min(self) -> i128 {
        if self.signed {
            -(1 << (self.bits - 1))
        } else {
            0
        }
    }

    /// The largest value of the type.
    fn max(self) -> i128 {
        if self.signed {
            (1 << (self.bits - 1)) - 1
        } else {
            self.mask()
        }
    }

    /// The value with every bit of the type set, as an unsigned number.
    fn mask(self) -> i128 {
        (1 << self.bits) - 1
    }

    /// The result of a checked operation: `None` when it overflowed the
    /// computation, which no type holds.
    fn check(self, value: Option<i128>) -> Result<i128, ArithError> {
        value
            .filter(|value| (self.min()..=self.max()).contains(value))
            .ok_or(ArithError::Overflow)
    }

    /// The value truncated to the type's bits, in two's complement.
    fn wrap(self, value: i128) -> i128 {
        let low = value & self.mask();
        if low > self.max() {
            low - (1 << self.bits)
        } else {
            low
        }
    }
}

/// The integer type `ty` on each target it may have: two for `nint` and
/// `nuint`, which are 32 or 64 bits wide, one for any other.
fn widths(ty: Primitive) -> &'static [Width] {
    match ty {
        Primitive::Byte => &[Width::U8],
        Primitive::SByte => &[Width::I8],
        Primitive::Short => &[Width::I16],
        Primitive::UShort => &[Width::U16],
        Primitive::Int => &[Width::I32],
        Primitive::UInt => &[Width::U32],
        Primitive::Long => &[Width::I64],
        Primitive::ULong => &[Width::U64],
        Primitive::NInt => &[Width::I32, Width::I64],
        Primitive::NUInt => &[Width::U32, Width::U64],
        Primitive::Bool | Primitive::Float | Primitive::Double | Primitive::Char => {
            internal(&format!("`{}` is not an integer type", ty.keyword()))
        }
    }
}

/// The integer of type `ty` that `compute` gives on every target: the
/// value or the error they all give, and `TargetDependent` when they
/// differ.
fn on_every_target(
    ty: Primitive,
    compute: impl Fn(Width) -> Result<i128, ArithError>,
) -> Result<Const, ArithError> {
    let widths = widths(ty);
    let first = compute(widths[0]);
    if widths[1..].iter().all(|&width| compute(width) == first) {
        first.map(|value| Const::Int(value, ty))
    } else {
        Err(ArithError::TargetDependent)
    }
}

/// The result of the binary operation `op` on two scalars, as the program
/// computes it at run time with every checked operation checked (§7.12).
/// Both operands have one type, except for a shift, whose count is an
/// `int`.
pub(crate) fn binary(op: BinOp, left: &Const, right: &Const) -> Result<Const, ArithError> {
    match (left, right) {
        (Const::Int(value, ty), Const::Int(count, _))
            if matches!(op, BinOp::Shl | BinOp::Shr | BinOp::UShr) =>
        {
            shift(op, *value, *ty, *count)
        }
        (Const::Int(left, ty), Const::Int(right, right_ty)) if ty == right_ty => {
            integer(op, *left, *right, *ty)
        }
        (Const::Real(left, ty), Const::Real(right, right_ty)) if ty == right_ty => {
            real(op, *left, *right, *ty)
        }
        (Const::Char(left), Const::Char(right)) => {
            comparison(op, left, right).unwrap_or_else(|| unsupported(op, "`char`"))
        }
        (Const::Bool(left), Const::Bool(right)) => match op {
            BinOp::BitAnd => Ok(Const::Bool(left & right)),
            BinOp::BitOr => Ok(Const::Bool(left | right)),
            BinOp::BitXor => Ok(Const::Bool(left ^ right)),
            _ => equality(op, left == right).unwrap_or_else(|| unsupported(op, "`bool`")),
        },
        (Const::String(left), Const::String(right)) => match op {
            BinOp::Add => concatenate(left, right),
            _ => equality(op, left == right).unwrap_or_else(|| unsupported(op, "`string`")),
        },
        (Const::Unit, Const::Unit) => equality(op, true).unwrap_or_else(|| unsupported(op, "`()`")),
        _ => internal(&format!("`{}` on {left:?} and {right:?}", op.text())),
    }
}

/// `op` on two integers of type `ty`.
fn integer(op: BinOp, left: i128, right: i128, ty: Primitive) -> Result<Const, ArithError> {
    if let Some(result) = comparison(op, &left, &right) {
        return result;
    }
    on_every_target(ty, |width| match op {
        BinOp::Add => width.check(left.checked_add(right)),
        BinOp::Sub => width.check(left.checked_sub(right)),
        BinOp::Mul => width.check(left.checked_mul(right)),
        BinOp::Div if right == 0 => Err(ArithError::DivisionByZero),
        BinOp::Div => width.check(Some(left / right)),
        BinOp::Rem if right == 0 => Err(ArithError::RemainderByZero),
        // The quotient `MinValue / -1` overflows, so the remainder does too.
        BinOp::Rem if left == width.min() && right == -1 => Err(ArithError::Overflow),
        BinOp::Rem => Ok(left % right),
        BinOp::WrappingAdd => Ok(width.wrap(left.wrapping_add(right))),
        BinOp::WrappingSub => Ok(width.wrap(left.wrapping_sub(right))),
        BinOp::WrappingMul => Ok(width.wrap(left.wrapping_mul(right))),
        BinOp::BitAnd => Ok(left & right),
        BinOp::BitOr => Ok(left | right),
        BinOp::BitXor => Ok(left ^ right),
        _ => unsupported(op, "integers"),
    })
}

/// A shift of `value`, of type `ty`, by `count`, masked to the width of
/// `ty`: `count & 31` for a 32-bit type, `count & 63` for a 64-bit one.
fn shift(op: BinOp, value: i128, ty: Primitive, count: i128) -> Result<Const, ArithError> {
    on_every_target(ty, |width| {
        // At most 63, so the cast keeps the value.
        let count = (count & i128::from(width.bits.max(32) - 1)) as u32;
        Ok(match op {
            BinOp::Shl => width.wrap(value << count),
            BinOp::Shr => value >> count,
            _ => width.wrap((value & width.mask()) >> count),
        })
    })
}

/// `op` on two floating-point numbers of type `ty`, given as the bits of
/// `double` values; a `float` operation rounds in single precision.
fn real(op: BinOp, left: u64, right: u64, ty: Primitive) -> Result<Const, ArithError> {
    let (left, right) = (f64::from_bits(left), f64::from_bits(right));
    if let Some(result) = comparison(op, &left, &right) {
        return result;
    }
    let bits = match ty {
        Primitive::Float => f64::from(floating(op, left as f32, right as f32)).to_bits(),
        _ => floating(op, left, right).to_bits(),
    };
    Ok(Const::Real(bits, ty))
}

/// An arithmetic operation in the floating-point type `T`, which has no
/// traps: `%` is the remainder of the truncated quotient.
fn floating<T>(op: BinOp, left: T, right: T) -> T
where
    T: Add<Output = T> + Sub<Output = T> + Mul<Output = T> + Div<Output = T> + Rem<Output = T>,
{
    match op {
        BinOp::Add => left + right,
        BinOp::Sub => left - right,
        BinOp::Mul => left * right,
        BinOp::Div => left / right,
        BinOp::Rem => left % right,
        _ => unsupported(op, "floating-point numbers"),
    }
}

/// The result of `op` when it is a comparison, `None` otherwise.
fn comparison<T: PartialOrd>(op: BinOp, left: &T, right: &T) -> Option<Result<Const, ArithError>> {
    let result = match op {
        BinOp::Eq => left == right,
        BinOp::Ne => left != right,
        BinOp::Lt => left < right,
        BinOp::Le => left <= right,
        BinOp::Gt => left > right,
        BinOp::Ge => left >= right,
        _ => return None,
    };
    Some(Ok(Const::Bool(result)))
}

/// The result of `op` when it is `==` or `!=` on operands that are equal
/// when `equal` is set, `None` otherwise.
fn equality(op: BinOp, equal: bool) -> Option<Result<Const, ArithError>> {
    match op {
        BinOp::Eq => Some(Ok(Const::Bool(equal))),
        BinOp::Ne => Some(Ok(Const::Bool(!equal))),
        _ => None,
    }
}

/// Two strings joined, unless the result is longer than
/// [`MAX_STRING_BYTES`].
fn concatenate(left: &str, right: &str) -> Result<Const, ArithError> {
    let len = left.len() + right.len();
    if len > MAX_STRING_BYTES {
        return Err(ArithError::TooLong(len));
    }
    Ok(Const::String([left, right].concat()))
}

/// The result of the unary operation `op` on a scalar (§7.12).
pub(crate) fn unary(op: UnOp, value: &Const) -> Result<Const, ArithError> {
    match (op, value) {
        (UnOp::Neg, Const::Int(value, ty)) => {
            on_every_target(*ty, |width| width.check(value.checked_neg()))
        }
        // A `float` value negated is still exact as a `double`.
        (UnOp::Neg, Const::Real(bits, ty)) => {
            Ok(Const::Real((-f64::from_bits(*bits)).to_bits(), *ty))
        }
        (UnOp::BitNot, Const::Int(value, ty)) => {
            on_every_target(*ty, |width| Ok(width.wrap(!value)))
        }
        (UnOp::Not, Const::Bool(value)) => Ok(Const::Bool(!value)),
        _ => internal(&format!("`{}` on {value:?}", op.text())),
    }
}

/// `value` converted to the numeric type `to` (§5.10): an integer
/// truncates and wraps, a floating-point number rounds to nearest, and a
/// floating-point number converted to an integer type saturates, with NaN
/// giving 0.
pub(crate) fn cast(value: &Const, to: Primitive) -> Result<Const, ArithError> {
    match (value, to) {
        // Each rounds once, from the integer itself.
        (Const::Int(value, _), Primitive::Float) => {
            Ok(Const::Real(f64::from(*value as f32).to_bits(), to))
        }
        (Const::Int(value, _), Primitive::Double) => Ok(Const::Real((*value as f64).to_bits(), to)),
        (Const::Int(value, _), _) => on_every_target(to, |width| Ok(width.wrap(*value))),
        (Const::Real(bits, _), Primitive::Float) => Ok(Const::Real(
            f64::from(f64::from_bits(*bits) as f32).to_bits(),
            to,
        )),
        (Const::Real(bits, _), Primitive::Double) => Ok(Const::Real(*bits, to)),
        (Const::Real(bits, _), _) => {
            // `as` truncates, saturates at the bounds of `i128` and maps NaN to 0.
            let value = f64::from_bits(*bits) as i128;
            on_every_target(to, |width| Ok(value.clamp(width.min(), width.max())))
        }
        _ => internal(&format!("cast of {value:?} to `{}`", to.keyword())),
    }
}

/// `==` on two scalars of one type: by value for numbers (so -0.0 equals
/// 0.0 and NaN equals nothing).
pub(crate) fn equal(left: &Const, right: &Const) -> bool {
    match (left, right) {
        (Const::Real(left, _), Const::Real(right, _)) => {
            f64::from_bits(*left) == f64::from_bits(*right)
        }
        _ => left == right,
    }
}

/// Panics on an operation that the operands' type does not have, which
/// typing rules out.
fn unsupported(op: BinOp, operands: &str) -> ! {
    internal(&format!("`{}` on {operands}", op.text()))
}

/// Panics with an internal error: the operation is one that only a
/// compiler bug passes in.
fn internal(what: &str) -> ! {
    panic!("internal error evaluating a constant: {what}")
}

#[cfg(test)]
mod tests {
    use mora_hir::Primitive::{self, Byte, Double, Float, Int, Long, NInt, NUInt, UInt, ULong};

    use super::ArithError::{DivisionByZero, Overflow, RemainderByZero, TargetDependent, TooLong};
    use super::{MAX_STRING_BYTES, binary, cast, equal, unary};
    use crate::{BinOp, Const, UnOp};

    fn n(value: i128, ty: Primitive) -> Const {
        Const::Int(value, ty)
    }

    fn i(value: i128) -> Const {
        n(value, Int)
    }

    fn f(value: f32) -> Const {
        Const::Real(f64::from(value).to_bits(), Float)
    }

    fn d(value: f64) -> Const {
        Const::Real(value.to_bits(), Double)
    }

    fn s(text: &str) -> Const {
        Const::String(text.to_owned())
    }

    #[test]
    fn checked_operations_fail_out_of_range() {
        assert_eq!(binary(BinOp::Mul, &i(1_500_000_000), &i(2)), Err(Overflow));
        assert_eq!(
            binary(BinOp::Add, &i(i32::MAX.into()), &i(1)),
            Err(Overflow)
        );
        assert_eq!(
            binary(BinOp::Sub, &i(i32::MIN.into()), &i(1)),
            Err(Overflow)
        );
        assert_eq!(binary(BinOp::Sub, &n(0, Byte), &n(1, Byte)), Err(Overflow));
        assert_eq!(
            binary(BinOp::Add, &n(255, Byte), &n(1, Byte)),
            Err(Overflow)
        );
        let max = n(u64::MAX.into(), ULong);
        assert_eq!(binary(BinOp::Mul, &max, &max), Err(Overflow));
        assert_eq!(unary(UnOp::Neg, &i(i32::MIN.into())), Err(Overflow));
        assert_eq!(
            binary(BinOp::Div, &i(i32::MIN.into()), &i(-1)),
            Err(Overflow)
        );
        assert_eq!(
            binary(BinOp::Rem, &i(i32::MIN.into()), &i(-1)),
            Err(Overflow)
        );
    }

    #[test]
    fn checked_operations_compute_in_range() {
        assert_eq!(binary(BinOp::Add, &i(2), &i(3)), Ok(i(5)));
        assert_eq!(binary(BinOp::Sub, &i(2), &i(3)), Ok(i(-1)));
        assert_eq!(binary(BinOp::Mul, &i(-4), &i(3)), Ok(i(-12)));
        assert_eq!(
            binary(BinOp::Add, &n(254, Byte), &n(1, Byte)),
            Ok(n(255, Byte))
        );
        assert_eq!(unary(UnOp::Neg, &i(7)), Ok(i(-7)));
        assert_eq!(
            unary(UnOp::Neg, &i(i32::MAX.into())),
            Ok(i((-i32::MAX).into()))
        );
    }

    #[test]
    fn division_truncates_toward_zero() {
        assert_eq!(binary(BinOp::Div, &i(-7), &i(2)), Ok(i(-3)));
        assert_eq!(binary(BinOp::Rem, &i(-7), &i(2)), Ok(i(-1)));
        assert_eq!(binary(BinOp::Rem, &i(7), &i(-1)), Ok(i(0)));
        assert_eq!(binary(BinOp::Rem, &i(i32::MIN.into()), &i(3)), Ok(i(-2)));
        assert_eq!(
            binary(BinOp::Div, &i(i32::MIN.into()), &i(1)),
            Ok(i(i32::MIN.into()))
        );
    }

    #[test]
    fn division_and_remainder_by_zero_fail() {
        assert_eq!(binary(BinOp::Div, &i(1), &i(0)), Err(DivisionByZero));
        assert_eq!(binary(BinOp::Rem, &i(1), &i(0)), Err(RemainderByZero));
        let (one, zero) = (n(1, ULong), n(0, ULong));
        assert_eq!(binary(BinOp::Div, &one, &zero), Err(DivisionByZero));
        assert_eq!(binary(BinOp::Rem, &one, &zero), Err(RemainderByZero));
        assert_eq!(binary(BinOp::Div, &d(1.0), &d(0.0)), Ok(d(f64::INFINITY)));
        assert_eq!(binary(BinOp::Div, &f(1.0), &f(0.0)), Ok(f(f32::INFINITY)));
    }

    #[test]
    fn wrapping_operations_wrap() {
        assert_eq!(
            binary(BinOp::WrappingAdd, &i(2_147_483_647), &i(1)),
            Ok(i(-2_147_483_648))
        );
        assert_eq!(
            binary(BinOp::WrappingAdd, &i(2_147_483_646), &i(1)),
            Ok(i(2_147_483_647))
        );
        assert_eq!(
            binary(BinOp::WrappingSub, &i(-2_147_483_648), &i(1)),
            Ok(i(2_147_483_647))
        );
        assert_eq!(
            binary(BinOp::WrappingSub, &n(0, UInt), &n(1, UInt)),
            Ok(n(u32::MAX.into(), UInt))
        );
        assert_eq!(
            binary(BinOp::WrappingMul, &n(200, Byte), &n(2, Byte)),
            Ok(n(144, Byte))
        );
        let max = n(u64::MAX.into(), ULong);
        assert_eq!(binary(BinOp::WrappingMul, &max, &max), Ok(n(1, ULong)));
    }

    #[test]
    fn bitwise_operations_on_integers() {
        assert_eq!(binary(BinOp::BitAnd, &i(0b1100), &i(0b1010)), Ok(i(0b1000)));
        assert_eq!(binary(BinOp::BitOr, &i(0b1100), &i(0b1010)), Ok(i(0b1110)));
        assert_eq!(binary(BinOp::BitXor, &i(0b1100), &i(0b1010)), Ok(i(0b0110)));
        assert_eq!(binary(BinOp::BitAnd, &i(-1), &i(-8)), Ok(i(-8)));
        assert_eq!(unary(UnOp::BitNot, &n(0, Byte)), Ok(n(255, Byte)));
        assert_eq!(
            unary(UnOp::BitNot, &n(5, ULong)),
            Ok(n((u64::MAX - 5).into(), ULong))
        );
    }

    #[test]
    fn shift_counts_are_masked() {
        assert_eq!(binary(BinOp::Shl, &i(1), &i(33)), Ok(i(2)));
        assert_eq!(binary(BinOp::Shl, &i(1), &i(31)), Ok(i(i32::MIN.into())));
        assert_eq!(binary(BinOp::Shl, &n(1, Long), &i(65)), Ok(n(2, Long)));
        assert_eq!(
            binary(BinOp::Shl, &n(1, Long), &i(40)),
            Ok(n(1 << 40, Long))
        );
        assert_eq!(
            binary(BinOp::Shl, &n(3, UInt), &i(31)),
            Ok(n(1 << 31, UInt))
        );
        assert_eq!(binary(BinOp::Shr, &i(-8), &i(1)), Ok(i(-4)));
        assert_eq!(binary(BinOp::Shr, &i(-8), &i(33)), Ok(i(-4)));
        assert_eq!(
            binary(BinOp::Shr, &n(u32::MAX.into(), UInt), &i(28)),
            Ok(n(15, UInt))
        );
        assert_eq!(binary(BinOp::UShr, &i(-8), &i(28)), Ok(i(15)));
        assert_eq!(binary(BinOp::UShr, &i(-8), &i(0)), Ok(i(-8)));
        assert_eq!(binary(BinOp::UShr, &n(-8, Long), &i(60)), Ok(n(15, Long)));
    }

    #[test]
    fn casts_follow_run_time_semantics() {
        assert_eq!(cast(&i(300), Byte), Ok(n(44, Byte)));
        assert_eq!(cast(&i(-1), UInt), Ok(n(4_294_967_295, UInt)));
        assert_eq!(cast(&n(4_294_967_295, UInt), Int), Ok(i(-1)));
        assert_eq!(cast(&d(f64::NAN), Int), Ok(i(0)));
        assert_eq!(cast(&d(1e20), Int), Ok(i(i32::MAX.into())));
        assert_eq!(cast(&d(-1e20), Int), Ok(i(i32::MIN.into())));
        assert_eq!(cast(&d(-1e20), ULong), Ok(n(0, ULong)));
        assert_eq!(cast(&d(-2.9), Int), Ok(i(-2)));
        assert_eq!(cast(&f(2.9), Long), Ok(n(2, Long)));
        assert_eq!(cast(&i(16_777_217), Float), Ok(f(16_777_216.0)));
        let big = n(i64::MAX.into(), Long);
        assert_eq!(cast(&big, Float), Ok(f(9.223_372e18)));
        assert_eq!(cast(&big, Double), Ok(d(9.223_372_036_854_776e18)));
        assert_eq!(cast(&d(0.1), Float), Ok(f(0.1)));
        assert_eq!(cast(&f(0.1), Double), Ok(d(f64::from(0.1f32))));
        assert_eq!(cast(&d(0.1), Double), Ok(d(0.1)));
    }

    #[test]
    fn float_arithmetic_rounds_in_single_precision() {
        let sum = binary(BinOp::Add, &f(0.1), &f(0.2));
        assert_eq!(sum, Ok(f(0.1f32 + 0.2f32)));
        let double_sum = f64::from(0.1f32) + f64::from(0.2f32);
        assert_ne!(sum, Ok(Const::Real(double_sum.to_bits(), Float)));
        assert_eq!(binary(BinOp::Mul, &f(0.1), &f(3.0)), Ok(f(0.1f32 * 3.0)));
        assert!(equal(&d(-0.0), &d(0.0)));
        assert!(equal(&f(-0.0), &f(0.0)));
        assert!(!equal(&d(f64::NAN), &d(f64::NAN)));
        assert!(!equal(&d(1.0), &d(2.0)));
        assert!(equal(&i(3), &i(3)));
        assert!(!equal(&i(3), &i(4)));
        assert!(equal(&s("a"), &s("a")));
        assert_eq!(
            binary(BinOp::Ne, &d(f64::NAN), &d(f64::NAN)),
            Ok(Const::Bool(true))
        );
        assert_eq!(binary(BinOp::Eq, &d(-0.0), &d(0.0)), Ok(Const::Bool(true)));
    }

    #[test]
    fn floating_operations() {
        assert_eq!(binary(BinOp::Add, &d(7.5), &d(2.0)), Ok(d(9.5)));
        assert_eq!(binary(BinOp::Sub, &d(7.5), &d(2.0)), Ok(d(5.5)));
        assert_eq!(binary(BinOp::Mul, &d(7.5), &d(2.0)), Ok(d(15.0)));
        assert_eq!(binary(BinOp::Div, &d(7.5), &d(2.0)), Ok(d(3.75)));
        assert_eq!(binary(BinOp::Rem, &d(-7.5), &d(2.0)), Ok(d(-1.5)));
        assert_eq!(binary(BinOp::Rem, &f(7.5), &f(2.0)), Ok(f(1.5)));
        assert_eq!(binary(BinOp::Sub, &f(1.0), &f(0.25)), Ok(f(0.75)));
        assert_eq!(binary(BinOp::Div, &f(1.0), &f(3.0)), Ok(f(1.0 / 3.0)));
        assert_eq!(unary(UnOp::Neg, &d(1.5)), Ok(d(-1.5)));
        assert_eq!(unary(UnOp::Neg, &f(0.0)), Ok(f(-0.0)));
        assert_eq!(binary(BinOp::Lt, &f(1.0), &f(2.0)), Ok(Const::Bool(true)));
    }

    #[test]
    fn strings_concatenate_up_to_the_limit() {
        assert_eq!(binary(BinOp::Add, &s("ab"), &s("cd")), Ok(s("abcd")));
        let full = Const::String("a".repeat(MAX_STRING_BYTES - 1));
        let at_limit = binary(BinOp::Add, &full, &s("x"));
        assert!(matches!(at_limit, Ok(Const::String(ref text)) if text.len() == MAX_STRING_BYTES));
        let over = Const::String("a".repeat(MAX_STRING_BYTES));
        assert_eq!(
            binary(BinOp::Add, &over, &s("x")),
            Err(TooLong(MAX_STRING_BYTES + 1))
        );
    }

    #[test]
    fn nint_values_must_agree_on_every_target() {
        assert_eq!(
            binary(BinOp::Add, &n(2_147_483_647, NInt), &n(1, NInt)),
            Err(TargetDependent)
        );
        assert_eq!(binary(BinOp::Add, &n(1, NInt), &n(2, NInt)), Ok(n(3, NInt)));
        assert_eq!(cast(&n(1 << 40, Long), NInt), Err(TargetDependent));
        assert_eq!(cast(&n(5, Long), NInt), Ok(n(5, NInt)));
        assert_eq!(cast(&d(1e10), NUInt), Err(TargetDependent));
        assert_eq!(
            binary(BinOp::Shl, &n(1, NInt), &i(32)),
            Err(TargetDependent)
        );
        assert_eq!(binary(BinOp::Shl, &n(1, NInt), &i(4)), Ok(n(16, NInt)));
        assert_eq!(
            binary(BinOp::Div, &n(1, NInt), &n(0, NInt)),
            Err(DivisionByZero)
        );
        assert_eq!(
            binary(BinOp::Sub, &n(0, NUInt), &n(1, NUInt)),
            Err(Overflow)
        );
        assert_eq!(unary(UnOp::BitNot, &n(0, NUInt)), Err(TargetDependent));
        assert_eq!(unary(UnOp::BitNot, &n(0, NInt)), Ok(n(-1, NInt)));
    }

    #[test]
    fn comparisons_and_logic() {
        let ordered: [(u8, u8); 3] = [(1, 2), (2, 2), (3, 2)];
        let expected = [
            (BinOp::Eq, [false, true, false]),
            (BinOp::Ne, [true, false, true]),
            (BinOp::Lt, [true, false, false]),
            (BinOp::Le, [true, true, false]),
            (BinOp::Gt, [false, false, true]),
            (BinOp::Ge, [false, true, true]),
        ];
        for (op, results) in expected {
            for ((a, b), result) in ordered.iter().zip(results) {
                let want = Ok(Const::Bool(result));
                assert_eq!(
                    binary(op, &i((*a).into()), &i((*b).into())),
                    want,
                    "{a} {op:?} {b}"
                );
                let (x, y) = (char::from(b'a' + a), char::from(b'a' + b));
                assert_eq!(
                    binary(op, &Const::Char(x), &Const::Char(y)),
                    want,
                    "{x} {op:?} {y}"
                );
                let (x, y) = (f64::from(*a), f64::from(*b));
                assert_eq!(binary(op, &d(x), &d(y)), want, "{x} {op:?} {y}");
            }
        }
        let t = Const::Bool(true);
        let fl = Const::Bool(false);
        assert_eq!(binary(BinOp::Eq, &s("ab"), &s("ab")), Ok(t.clone()));
        assert_eq!(binary(BinOp::Ne, &s("ab"), &s("ab")), Ok(fl.clone()));
        assert_eq!(binary(BinOp::Eq, &s("ab"), &s("ac")), Ok(fl.clone()));
        assert_eq!(binary(BinOp::Eq, &t, &t), Ok(t.clone()));
        assert_eq!(binary(BinOp::Ne, &t, &fl), Ok(t.clone()));
        assert_eq!(binary(BinOp::Eq, &Const::Unit, &Const::Unit), Ok(t.clone()));
        assert_eq!(binary(BinOp::BitAnd, &t, &fl), Ok(fl.clone()));
        assert_eq!(binary(BinOp::BitOr, &t, &fl), Ok(t.clone()));
        assert_eq!(binary(BinOp::BitXor, &t, &t), Ok(fl.clone()));
        assert_eq!(unary(UnOp::Not, &t), Ok(fl.clone()));
        assert_eq!(unary(UnOp::Not, &fl), Ok(t));
        assert_eq!(unary(UnOp::BitNot, &i(0)), Ok(i(-1)));
    }

    #[test]
    #[should_panic(expected = "internal error evaluating a constant")]
    fn operands_of_different_types_are_a_compiler_bug() {
        let _ = binary(BinOp::Add, &i(1), &n(1, Long));
    }
}
