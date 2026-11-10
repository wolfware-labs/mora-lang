//! The numeric rows of §5.10 and the promotions of §7.12.
//!
//! The standard library gives these conversions by implementing `IFrom`;
//! until the checker can look `IFrom` up (M3.6), this table stands in for
//! those implementations (D-135).

use crate::bodies::BinaryOp;
use crate::ty::{Primitive, Ty};

/// Whether `ty` is a numeric type: an integer type, `float` or `double`.
pub(crate) fn is_numeric(ty: Primitive) -> bool {
    !matches!(ty, Primitive::Bool | Primitive::Char)
}

/// The primitive type `ty` is, when it is a numeric type.
pub(crate) fn as_numeric(ty: &Ty) -> Option<Primitive> {
    match ty {
        Ty::Primitive(ty) if is_numeric(*ty) => Some(*ty),
        _ => None,
    }
}

/// Whether `ty` is a floating-point type: `float` or `double`.
pub(crate) fn is_floating_point(ty: Primitive) -> bool {
    ty.is_floating_point()
}

/// Whether `ty` is an integer type.
pub(crate) fn is_integer(ty: Primitive) -> bool {
    ty.is_integer()
}

/// Whether `op` is defined only on two integer operands of one promoted
/// type: `&`, `|`, `^` and the wrapping operators (§7.12).
pub(crate) fn is_integer_only(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::WrappingAdd
            | BinaryOp::WrappingSub
            | BinaryOp::WrappingMul
    )
}

/// Whether a value of `from` converts implicitly to `to` (§5.10): the
/// same type, an integer type to a wider one of the same signedness or a
/// wider signed one, an integer type to `float` or `double` where every
/// value is exact, and `float` to `double`. `nint` and `nuint` convert to
/// nothing else and nothing converts to them.
pub(crate) fn converts(from: Primitive, to: Primitive) -> bool {
    use Primitive::{Byte, Double, Float, Int, Long, SByte, Short, UInt, ULong, UShort};
    from == to
        || match from {
            Byte => matches!(
                to,
                Short | UShort | Int | UInt | Long | ULong | Float | Double
            ),
            SByte => matches!(to, Short | Int | Long | Float | Double),
            UShort => matches!(to, Int | UInt | Long | ULong | Float | Double),
            Short => matches!(to, Int | Long | Float | Double),
            UInt => matches!(to, Long | ULong | Double),
            Int => matches!(to, Long | Double),
            Float => to == Double,
            _ => false,
        }
}

/// The type a binary operator on two numeric operands works in (§7.12,
/// rules 2 and 3), or `None` when they have no common type, as `long` and
/// `ulong`. Rule 1, for literal operands, is applied before: the literal
/// already has the other operand's type.
pub(crate) fn promote(a: Primitive, b: Primitive) -> Option<Primitive> {
    use Primitive::{Double, Float, Int, Long, NInt, NUInt, UInt, ULong};
    if a == b && matches!(a, Int | UInt | NInt | NUInt | Long | ULong | Float | Double) {
        return Some(a);
    }
    [Int, UInt, Long, ULong, Float, Double]
        .into_iter()
        .find(|&to| converts(a, to) && converts(b, to))
}

/// The type of the operand of unary `+`, `-` and `~`, and of the left
/// operand of a shift, after widening: `byte`, `sbyte`, `short` and
/// `ushort` widen to `int` (§7.12).
pub(crate) fn widen_small(ty: Primitive) -> Primitive {
    match ty {
        Primitive::Byte | Primitive::SByte | Primitive::Short | Primitive::UShort => Primitive::Int,
        other => other,
    }
}

/// The smallest and largest value of an integer type. `nint` and `nuint`
/// have the range they have on every target, that of `int` and `uint`.
pub(crate) fn integer_range(ty: Primitive) -> Option<(i128, i128)> {
    let range = match ty {
        Primitive::Byte => (0, u8::MAX.into()),
        Primitive::SByte => (i8::MIN.into(), i8::MAX.into()),
        Primitive::Short => (i16::MIN.into(), i16::MAX.into()),
        Primitive::UShort => (0, u16::MAX.into()),
        Primitive::Int | Primitive::NInt => (i32::MIN.into(), i32::MAX.into()),
        Primitive::UInt | Primitive::NUInt => (0, u32::MAX.into()),
        Primitive::Long => (i64::MIN.into(), i64::MAX.into()),
        Primitive::ULong => (0, u64::MAX.into()),
        _ => return None,
    };
    Some(range)
}

/// Whether the integer `value` is a value of `ty` exactly: in the range of
/// an integer type, or exact as a `float` or `double`.
pub(crate) fn holds(ty: Primitive, value: i128) -> bool {
    // The conversions below are the point: a value is exact when it comes
    // back unchanged. A value that rounds up to 2^127 would saturate on the
    // way back to `i128::MAX`, so that rounding is not exact.
    match ty {
        Primitive::Float => {
            let rounded = value as f32;
            rounded < i128::MAX as f32 && rounded as i128 == value
        }
        Primitive::Double => {
            let rounded = value as f64;
            rounded < i128::MAX as f64 && rounded as i128 == value
        }
        _ => integer_range(ty).is_some_and(|(min, max)| (min..=max).contains(&value)),
    }
}

/// The type of an unsuffixed integer literal without a type to take: the
/// first of `int`, `long` and `ulong` that holds its value (§3.5).
pub(crate) fn default_integer(value: i128) -> Option<Primitive> {
    [Primitive::Int, Primitive::Long, Primitive::ULong]
        .into_iter()
        .find(|&ty| holds(ty, value))
}

/// How many bits a numeric type has, `nint` and `nuint` counting as 64,
/// for choosing which operand of a mixed operation to cast.
pub(crate) fn bits(ty: Primitive) -> u32 {
    match ty {
        Primitive::Byte | Primitive::SByte | Primitive::Bool => 8,
        Primitive::Short | Primitive::UShort => 16,
        Primitive::Int | Primitive::UInt | Primitive::Float | Primitive::Char => 32,
        Primitive::Long
        | Primitive::ULong
        | Primitive::NInt
        | Primitive::NUInt
        | Primitive::Double => 64,
    }
}

/// Whether `ty` is a signed type: a signed integer type, `float` or
/// `double`.
pub(crate) fn is_signed(ty: Primitive) -> bool {
    matches!(
        ty,
        Primitive::SByte
            | Primitive::Short
            | Primitive::Int
            | Primitive::Long
            | Primitive::NInt
            | Primitive::Float
            | Primitive::Double
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn converts_implicitly_is_the_table_and_leaves_native_integers_out() {
        use crate::ty::Primitive::{Byte, Double, Float, Int, Long, NInt, NUInt};
        assert!(crate::converts_implicitly(Byte, Int));
        assert!(crate::converts_implicitly(Int, Double));
        assert!(crate::converts_implicitly(Float, Double));
        assert!(!crate::converts_implicitly(Int, Byte));
        assert!(!crate::converts_implicitly(Long, Double));
        assert!(!crate::converts_implicitly(NInt, Int));
        assert!(!crate::converts_implicitly(Int, NInt));
        assert!(!crate::converts_implicitly(NUInt, Long));
    }

    use super::*;
    use Primitive::*;

    const NUMERIC: [Primitive; 12] = [
        Byte, SByte, Short, UShort, Int, UInt, Long, ULong, NInt, NUInt, Float, Double,
    ];

    #[test]
    fn integers_widen_within_their_signedness_or_to_a_wider_signed_type() {
        let widened: Vec<_> = NUMERIC
            .iter()
            .map(|&from| {
                let to: Vec<_> = NUMERIC
                    .iter()
                    .filter(|&&to| to != from && converts(from, to))
                    .map(|to| to.keyword())
                    .collect();
                (from.keyword(), to.join(" "))
            })
            .collect();
        let expected = [
            ("byte", "short ushort int uint long ulong float double"),
            ("sbyte", "short int long float double"),
            ("short", "int long float double"),
            ("ushort", "int uint long ulong float double"),
            ("int", "long double"),
            ("uint", "long ulong double"),
            ("long", ""),
            ("ulong", ""),
            ("nint", ""),
            ("nuint", ""),
            ("float", "double"),
            ("double", ""),
        ];
        let expected: Vec<_> = expected
            .iter()
            .map(|&(from, to)| (from, to.to_owned()))
            .collect();
        assert_eq!(widened, expected);
    }

    #[test]
    fn bool_and_char_convert_only_to_themselves() {
        for ty in NUMERIC {
            for other in [Bool, Char] {
                assert!(!converts(ty, other), "{ty:?} to {other:?}");
                assert!(!converts(other, ty), "{other:?} to {ty:?}");
            }
        }
        assert!(converts(Bool, Bool));
        assert!(converts(Char, Char));
        assert!(!converts(Bool, Char));
    }

    #[test]
    fn promotion_picks_the_first_common_type() {
        assert_eq!(promote(Byte, Byte), Some(Int));
        assert_eq!(promote(Short, UShort), Some(Int));
        assert_eq!(promote(Int, UInt), Some(Long));
        assert_eq!(promote(SByte, UInt), Some(Long));
        assert_eq!(promote(Int, Float), Some(Double));
        assert_eq!(promote(Byte, Float), Some(Float));
        assert_eq!(promote(Float, Float), Some(Float));
        assert_eq!(promote(UInt, UInt), Some(UInt));
        assert_eq!(promote(NInt, NInt), Some(NInt));
        assert_eq!(promote(Long, Int), Some(Long));
        assert_eq!(promote(ULong, Byte), Some(ULong));
        assert_eq!(promote(Long, ULong), None);
        assert_eq!(promote(Long, Float), None);
        assert_eq!(promote(Long, Double), None);
        assert_eq!(promote(NInt, Int), None);
        assert_eq!(promote(NUInt, NInt), None);
    }

    #[test]
    fn unary_operands_and_shifted_values_widen_to_int() {
        for small in [Byte, SByte, Short, UShort] {
            assert_eq!(widen_small(small), Int);
        }
        for ty in [Int, UInt, Long, ULong, NInt, NUInt, Float, Double] {
            assert_eq!(widen_small(ty), ty);
        }
    }

    #[test]
    fn literals_fit_by_range_or_exactness() {
        assert!(holds(Byte, 255));
        assert!(!holds(Byte, 256));
        assert!(!holds(Byte, -1));
        assert!(holds(SByte, -128));
        assert!(!holds(SByte, 128));
        assert!(holds(Long, i64::MIN.into()));
        assert!(!holds(ULong, -1));
        assert!(holds(ULong, u64::MAX.into()));
        assert!(holds(Float, 16_777_216));
        assert!(!holds(Float, 16_777_217));
        assert!(holds(Double, 9_007_199_254_740_992));
        assert!(!holds(Double, 9_007_199_254_740_993));
        assert!(!holds(Float, i128::MAX));
        assert!(!holds(Double, i128::MAX));
        assert!(holds(Float, i128::MIN));
        assert!(holds(Double, i128::MIN));
        assert!(!holds(Bool, 0));
        assert!(!holds(Char, 0));
        assert!(holds(NInt, i32::MAX.into()));
        assert!(!holds(NInt, i128::from(i32::MAX) + 1));
    }

    #[test]
    fn an_unsuffixed_integer_takes_the_first_type_that_holds_it() {
        assert_eq!(default_integer(1), Some(Int));
        assert_eq!(default_integer(-2_147_483_648), Some(Int));
        assert_eq!(default_integer(2_147_483_648), Some(Long));
        assert_eq!(default_integer(u64::MAX.into()), Some(ULong));
        assert_eq!(default_integer(-i128::from(u64::MAX)), None);
    }

    #[test]
    fn kinds_of_numeric_type() {
        assert!(is_integer(NInt));
        assert!(!is_integer(Float));
        assert!(!is_numeric(Bool));
        assert!(!is_numeric(Char));
        assert!(is_floating_point(Float));
        assert!(is_floating_point(Double));
        assert!(!is_floating_point(Long));
        assert!(!is_floating_point(Char));
        assert_eq!(as_numeric(&Ty::Primitive(Double)), Some(Double));
        assert_eq!(as_numeric(&Ty::Primitive(Bool)), None);
        assert_eq!(as_numeric(&Ty::String), None);
        assert!(is_signed(Long));
        assert!(!is_signed(NUInt));
        assert_eq!(bits(NInt), 64);
        assert_eq!(bits(UShort), 16);
        assert!(is_integer_only(BinaryOp::WrappingMul));
        assert!(is_integer_only(BinaryOp::BitXor));
        assert!(!is_integer_only(BinaryOp::Rem));
        assert!(!is_integer_only(BinaryOp::Shl));
    }
}
