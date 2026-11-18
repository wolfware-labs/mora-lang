//! The lowering of operators, comparisons and casts on scalars. Their
//! results match those of constant evaluation bit for bit (§14.6), except
//! that a checked operation wraps here.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{InstBuilder, Type, Value, types};
use mora_hir::Ty;
use mora_mir::{BinOp, Operand, UnOp};

use super::function::{Lowering, broken, fail, scalar_type};
use crate::layout::{Scalar, Shape};

/// The condition codes of a comparison.
struct Conditions {
    /// The code for signed integers.
    signed: IntCC,
    /// The code for unsigned integers, `bool`, `char` and pointers.
    unsigned: IntCC,
    /// The code for floating-point numbers, where NaN equals and orders
    /// with nothing.
    float: FloatCC,
}

impl Conditions {
    /// The codes of `op`; `None` when it is not a comparison.
    fn of(op: BinOp) -> Option<Conditions> {
        let (signed, unsigned, float) = match op {
            BinOp::Eq => (IntCC::Equal, IntCC::Equal, FloatCC::Equal),
            // Not equal is true for NaN.
            BinOp::Ne => (IntCC::NotEqual, IntCC::NotEqual, FloatCC::NotEqual),
            BinOp::Lt => (
                IntCC::SignedLessThan,
                IntCC::UnsignedLessThan,
                FloatCC::LessThan,
            ),
            BinOp::Le => (
                IntCC::SignedLessThanOrEqual,
                IntCC::UnsignedLessThanOrEqual,
                FloatCC::LessThanOrEqual,
            ),
            BinOp::Gt => (
                IntCC::SignedGreaterThan,
                IntCC::UnsignedGreaterThan,
                FloatCC::GreaterThan,
            ),
            BinOp::Ge => (
                IntCC::SignedGreaterThanOrEqual,
                IntCC::UnsignedGreaterThanOrEqual,
                FloatCC::GreaterThanOrEqual,
            ),
            _ => return None,
        };
        Some(Conditions {
            signed,
            unsigned,
            float,
        })
    }
}

impl Lowering<'_, '_> {
    /// The scalar `operand` reads and its value.
    ///
    /// # Panics
    ///
    /// With an internal error when it is not a scalar: strings and
    /// aggregates are not lowered until M1.12.
    fn scalar(&mut self, operand: &Operand) -> (Scalar, Value) {
        let source = self.source(operand);
        let Shape::Scalar(scalar) = self.source_layout(source).shape else {
            fail(
                self.body,
                &format!("an operator on `{operand:?}`, which is not a scalar"),
                "M1.12",
            )
        };
        // A scalar is never zero-sized.
        let value = self
            .source_value(source)
            .unwrap_or_else(|| broken(self.body, &format!("a zero-sized scalar `{operand:?}`")));
        (scalar, value)
    }

    /// The value of the binary operation `op`. A checked `+`, `-` and `*`
    /// wrap.
    ///
    /// # Panics
    ///
    /// With an internal error for `/` and `%`, which M1.12 lowers with the
    /// checks of a debug build, or for operands `op` does not take.
    pub(super) fn binary(&mut self, op: BinOp, left: &Operand, right: &Operand) -> Value {
        if matches!(op, BinOp::Div | BinOp::Rem) {
            fail(self.body, &format!("the operator `{}`", op.text()), "M1.12");
        }
        let (scalar, left) = self.scalar(left);
        let (count, right) = self.scalar(right);
        if let Some(conditions) = Conditions::of(op) {
            return self.compare(&conditions, scalar, left, right);
        }
        match (op, scalar) {
            (BinOp::Shl | BinOp::Shr | BinOp::UShr, _) => {
                self.shift(op, scalar, left, count, right)
            }
            (BinOp::Add | BinOp::WrappingAdd, Scalar::Int { .. }) => {
                self.builder.ins().iadd(left, right)
            }
            (BinOp::Sub | BinOp::WrappingSub, Scalar::Int { .. }) => {
                self.builder.ins().isub(left, right)
            }
            (BinOp::Mul | BinOp::WrappingMul, Scalar::Int { .. }) => {
                self.builder.ins().imul(left, right)
            }
            (BinOp::Add, Scalar::Float | Scalar::Double) => self.builder.ins().fadd(left, right),
            (BinOp::Sub, Scalar::Float | Scalar::Double) => self.builder.ins().fsub(left, right),
            (BinOp::Mul, Scalar::Float | Scalar::Double) => self.builder.ins().fmul(left, right),
            (BinOp::BitAnd, Scalar::Int { .. } | Scalar::Bool) => {
                self.builder.ins().band(left, right)
            }
            (BinOp::BitOr, Scalar::Int { .. } | Scalar::Bool) => {
                self.builder.ins().bor(left, right)
            }
            (BinOp::BitXor, Scalar::Int { .. } | Scalar::Bool) => {
                self.builder.ins().bxor(left, right)
            }
            _ => broken(self.body, &format!("`{}` on a {scalar:?}", op.text())),
        }
    }

    /// The `bool` of a comparison of two `scalar`s.
    fn compare(
        &mut self,
        conditions: &Conditions,
        scalar: Scalar,
        left: Value,
        right: Value,
    ) -> Value {
        match scalar {
            Scalar::Int { signed, .. } => {
                let condition = if signed {
                    conditions.signed
                } else {
                    conditions.unsigned
                };
                self.builder.ins().icmp(condition, left, right)
            }
            Scalar::Bool | Scalar::Char | Scalar::Pointer => {
                self.builder.ins().icmp(conditions.unsigned, left, right)
            }
            Scalar::Float | Scalar::Double => {
                self.builder.ins().fcmp(conditions.float, left, right)
            }
        }
    }

    /// `value` shifted by `count`, which masks the count to the larger of
    /// the width of `value` and 32 bits. A value narrower than 32 bits is
    /// extended to 32, shifted and reduced: `>>` extends a signed value by
    /// its sign, every other shift by zeros.
    ///
    /// # Panics
    ///
    /// With an internal error when `value` or `count` is not an integer.
    fn shift(
        &mut self,
        op: BinOp,
        scalar: Scalar,
        value: Value,
        count_scalar: Scalar,
        count: Value,
    ) -> Value {
        let (Scalar::Int { signed, .. }, Scalar::Int { .. }) = (scalar, count_scalar) else {
            broken(
                self.body,
                &format!("`{}` of a {scalar:?} by a {count_scalar:?}", op.text()),
            )
        };
        let ty = self.builder.func.dfg.value_type(value);
        let wide = ty.bits().max(32);
        let wide = if wide == 64 { types::I64 } else { types::I32 };
        let value = self.resize(value, wide, signed && op == BinOp::Shr);
        let shifted = match op {
            BinOp::Shl => self.builder.ins().ishl(value, count),
            BinOp::Shr if signed => self.builder.ins().sshr(value, count),
            _ => self.builder.ins().ushr(value, count),
        };
        self.resize(shifted, ty, false)
    }

    /// The value of the unary operation `op`. A checked `-` wraps.
    ///
    /// # Panics
    ///
    /// With an internal error for an operand `op` does not take.
    pub(super) fn unary(&mut self, op: UnOp, operand: &Operand) -> Value {
        let (scalar, value) = self.scalar(operand);
        match (op, scalar) {
            (UnOp::Neg, Scalar::Int { .. }) => self.builder.ins().ineg(value),
            (UnOp::Neg, Scalar::Float | Scalar::Double) => self.builder.ins().fneg(value),
            (UnOp::BitNot, Scalar::Int { .. }) => self.builder.ins().bnot(value),
            (UnOp::Not, Scalar::Bool) => self.builder.ins().icmp_imm_u(IntCC::Equal, value, 0),
            _ => broken(self.body, &format!("`{}` on a {scalar:?}", op.text())),
        }
    }

    /// `operand` converted to the type `to`, as constant evaluation does: an
    /// integer wraps, a floating-point number rounds to nearest, and a
    /// floating-point number converted to an integer saturates, with NaN
    /// giving 0.
    ///
    /// # Panics
    ///
    /// With an internal error for a conversion MIR validation rejects: to a
    /// type that is not a scalar, or between types that are not both
    /// numeric and not one.
    pub(super) fn cast(&mut self, operand: &Operand, to: &Ty) -> Value {
        let (from, value) = self.scalar(operand);
        let Shape::Scalar(to_scalar) = self.layouts.of(to).shape else {
            broken(
                self.body,
                &format!("a cast to `{to:?}`, which is not a scalar"),
            )
        };
        let to_type = scalar_type(to_scalar, self.pointer);
        match (from, to_scalar) {
            (from, to) if from == to => value,
            (Scalar::Int { signed, .. }, Scalar::Int { .. }) => self.resize(value, to_type, signed),
            // Extended to 64 bits, so that the conversion rounds once.
            (Scalar::Int { signed, .. }, Scalar::Float | Scalar::Double) => {
                let wide = self.resize(value, types::I64, signed);
                if signed {
                    self.builder.ins().fcvt_from_sint(to_type, wide)
                } else {
                    self.builder.ins().fcvt_from_uint(to_type, wide)
                }
            }
            (Scalar::Float, Scalar::Double) => self.builder.ins().fpromote(to_type, value),
            (Scalar::Double, Scalar::Float) => self.builder.ins().fdemote(to_type, value),
            (Scalar::Float | Scalar::Double, Scalar::Int { signed, .. }) => {
                self.float_to_int(value, to_type, signed)
            }
            _ => broken(
                self.body,
                &format!("a cast from a {from:?} to a {to_scalar:?}"),
            ),
        }
    }

    /// The integer `value` as an integer of type `to`: extended by its
    /// signedness `signed` when `to` is wider, reduced when narrower.
    fn resize(&mut self, value: Value, to: Type, signed: bool) -> Value {
        let from = self.builder.func.dfg.value_type(value);
        match from.bits().cmp(&to.bits()) {
            std::cmp::Ordering::Less if signed => self.builder.ins().sextend(to, value),
            std::cmp::Ordering::Less => self.builder.ins().uextend(to, value),
            std::cmp::Ordering::Equal => value,
            std::cmp::Ordering::Greater => self.builder.ins().ireduce(to, value),
        }
    }

    /// The floating-point `value` as an integer of type `to` that is signed
    /// when `signed`, truncated toward zero and clamped to the range of
    /// `to`, with NaN giving 0.
    fn float_to_int(&mut self, value: Value, to: Type, signed: bool) -> Value {
        if to.bits() == 64 {
            return if signed {
                self.builder.ins().fcvt_to_sint_sat(types::I64, value)
            } else {
                self.builder.ins().fcvt_to_uint_sat(types::I64, value)
            };
        }
        // Every value of a narrower type is in the range of an `i64`, and
        // the conversion saturates there.
        let wide = self.builder.ins().fcvt_to_sint_sat(types::I64, value);
        let (min, max) = if signed {
            (-(1i64 << (to.bits() - 1)), (1i64 << (to.bits() - 1)) - 1)
        } else {
            (0, (1i64 << to.bits()) - 1)
        };
        let low = self.builder.ins().iconst(types::I64, min);
        let high = self.builder.ins().iconst(types::I64, max);
        let clamped = self.builder.ins().smax(wide, low);
        let clamped = self.builder.ins().smin(clamped, high);
        self.builder.ins().ireduce(to, clamped)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use mora_hir::{Primitive, Ty};
    use mora_mir::{BinOp, Const, LocalTy, Operand, Rvalue, StatementKind, UnOp};

    use super::super::function::tests::{jit_one, jit_one_with};
    use crate::Profile;
    use crate::jit::JitProgram;
    use crate::test_util::{func, jit};

    /// The functions of a program, each compiled alone for a debug build,
    /// since the program of an app holds only what its entry reaches.
    struct Funcs(Vec<(String, JitProgram)>);

    impl Funcs {
        /// The functions of `text`, one on each line.
        fn of(text: &str) -> Funcs {
            Funcs(
                text.lines()
                    .map(|line| {
                        let name = line.split('(').next().unwrap_or(line);
                        (name.to_owned(), jit_one(line, name))
                    })
                    .collect(),
            )
        }

        /// The function `name`, as a function pointer of type `F`.
        fn get<F: Copy>(&self, name: &str) -> F {
            let (_, program) = self.0.iter().find(|(known, _)| known == name).unwrap();
            func(program, name)
        }
    }

    fn debug(text: &str) -> Funcs {
        Funcs::of(text)
    }

    #[test]
    fn arithmetic_in_range() {
        let program = debug(
            "Add(move a: int, move b: int): int => a + b;\n\
             Sub(move a: int, move b: int): int => a - b;\n\
             Mul(move a: int, move b: int): int => a * b;\n\
             AddL(move a: long, move b: long): long => a + b;\n\
             SubL(move a: long, move b: long): long => a - b;\n\
             MulL(move a: long, move b: long): long => a * b;\n\
             MulU(move a: ulong, move b: ulong): ulong => a * b;\n\
             AddU(move a: uint, move b: uint): uint => a + b;\n\
             DivD(move a: double, move b: double): double => a - b;\n",
        );
        assert_eq!(
            program.get::<extern "C" fn(i32, i32) -> i32>("Add")(40, 2),
            42
        );
        assert_eq!(
            program.get::<extern "C" fn(i32, i32) -> i32>("Sub")(40, 42),
            -2
        );
        assert_eq!(
            program.get::<extern "C" fn(i32, i32) -> i32>("Mul")(-6, 7),
            -42
        );
        let big = 5_000_000_000;
        assert_eq!(
            program.get::<extern "C" fn(i64, i64) -> i64>("AddL")(big, 1),
            big + 1
        );
        assert_eq!(
            program.get::<extern "C" fn(i64, i64) -> i64>("SubL")(big, 1),
            big - 1
        );
        assert_eq!(
            program.get::<extern "C" fn(i64, i64) -> i64>("MulL")(big, -3),
            -3 * big
        );
        assert_eq!(
            program.get::<extern "C" fn(u64, u64) -> u64>("MulU")(3, 5),
            15
        );
        assert_eq!(
            program.get::<extern "C" fn(u32, u32) -> u32>("AddU")(4_000_000_000, 5),
            4_000_000_005
        );
        assert_eq!(
            program.get::<extern "C" fn(f64, f64) -> f64>("DivD")(5.0, 2.5),
            2.5
        );
    }

    #[test]
    fn wrapping_operators_wrap() {
        let program = debug(
            "Add(move a: int, move b: int): int => a +% b;\n\
             Sub(move a: int, move b: int): int => a -% b;\n\
             Mul(move a: int, move b: int): int => a *% b;\n\
             AddL(move a: long, move b: long): long => a +% b;\n\
             SubU(move a: uint, move b: uint): uint => a -% b;\n\
             MulU(move a: ulong, move b: ulong): ulong => a *% b;\n",
        );
        let int = |name| program.get::<extern "C" fn(i32, i32) -> i32>(name);
        assert_eq!(int("Add")(i32::MAX, 1), i32::MIN);
        assert_eq!(int("Sub")(i32::MIN, 1), i32::MAX);
        assert_eq!(int("Mul")(1 << 30, 4), 0);
        assert_eq!(int("Mul")(i32::MAX, 2), -2);
        assert_eq!(
            program.get::<extern "C" fn(i64, i64) -> i64>("AddL")(i64::MAX, 1),
            i64::MIN
        );
        assert_eq!(
            program.get::<extern "C" fn(u32, u32) -> u32>("SubU")(0, 1),
            u32::MAX
        );
        assert_eq!(
            program.get::<extern "C" fn(u64, u64) -> u64>("MulU")(u64::MAX, 2),
            u64::MAX - 1
        );
    }

    #[test]
    fn release_wraps_checked_operators() {
        // The program of an app holds the functions its entry calls.
        let program = jit(
            "Add(move a: int, move b: int): int => a + b;\n\
             Sub(move a: int, move b: int): int => a - b;\n\
             Mul(move a: int, move b: int): int => a * b;\n\
             Neg(move a: int): int => -a;\n\
             Add(1, 1);\nSub(1, 1);\nMul(1, 1);\nNeg(1);\n",
            Profile::Release,
        );
        let int = |name| func::<extern "C" fn(i32, i32) -> i32>(&program, name);
        assert_eq!(int("Add")(i32::MAX, 1), i32::MIN);
        assert_eq!(int("Sub")(i32::MIN, 1), i32::MAX);
        assert_eq!(int("Mul")(1 << 20, 1 << 20), 0);
        let neg = func::<extern "C" fn(i32) -> i32>(&program, "Neg");
        assert_eq!((neg(i32::MIN), neg(5)), (i32::MIN, -5));
    }

    #[test]
    fn float_arithmetic_is_single_precision() {
        let program = debug(
            "Add(move a: float, move b: float): float => a + b;\n\
             Sub(move a: float, move b: float): float => a - b;\n\
             Mul(move a: float, move b: float): float => a * b;\n\
             Neg(move a: float): float => -a;\n\
             NegD(move a: double): double => -a;\n\
             MulD(move a: double, move b: double): double => a * b;\n\
             AddD(move a: double, move b: double): double => a + b;\n",
        );
        let float = |name| program.get::<extern "C" fn(f32, f32) -> f32>(name);
        assert_eq!(
            float("Add")(0.1, 0.2).to_bits(),
            (0.1f32 + 0.2f32).to_bits()
        );
        assert_eq!(
            float("Sub")(0.3, 0.1).to_bits(),
            (0.3f32 - 0.1f32).to_bits()
        );
        assert_eq!(
            float("Mul")(0.1, 0.3).to_bits(),
            (0.1f32 * 0.3f32).to_bits()
        );
        assert_eq!(program.get::<extern "C" fn(f32) -> f32>("Neg")(1.5), -1.5);
        assert_eq!(
            program.get::<extern "C" fn(f64) -> f64>("NegD")(0.0).to_bits(),
            (-0.0f64).to_bits()
        );
        let double = |name| program.get::<extern "C" fn(f64, f64) -> f64>(name);
        assert_eq!(
            double("MulD")(0.1, 0.3).to_bits(),
            (0.1f64 * 0.3f64).to_bits()
        );
        assert_eq!(
            double("AddD")(0.1, 0.2).to_bits(),
            (0.1f64 + 0.2f64).to_bits()
        );
    }

    /// A Rust comparison of two values.
    type Compare<T> = fn(&T, &T) -> bool;

    /// The comparison operators with the Rust function that gives each one's
    /// result.
    fn comparisons<T: PartialOrd>() -> [(&'static str, Compare<T>); 6] {
        [
            ("==", PartialEq::eq),
            ("!=", PartialEq::ne),
            ("<", PartialOrd::lt),
            ("<=", PartialOrd::le),
            (">", PartialOrd::gt),
            (">=", PartialOrd::ge),
        ]
    }

    /// Compares each pair with every operator of `ops` on the Mora type `ty`,
    /// whose values pass as `T`, and checks the result against Rust's.
    fn check_comparisons<T: Copy + PartialOrd + Debug>(ty: &str, ops: &[&str], pairs: &[(T, T)]) {
        let mut text = String::new();
        for (index, (op, _)) in comparisons::<T>().iter().enumerate() {
            if !ops.contains(op) {
                continue;
            }
            text += &format!("C{index}(move a: {ty}, move b: {ty}): bool => a {op} b;\n");
        }
        let program = debug(&text);
        for (index, (op, expected)) in comparisons::<T>().iter().enumerate() {
            if !ops.contains(op) {
                continue;
            }
            let f = program.get::<extern "C" fn(T, T) -> bool>(&format!("C{index}"));
            for &(a, b) in pairs {
                assert_eq!(f(a, b), expected(&a, &b), "{a:?} {op} {b:?} on {ty}");
            }
        }
    }

    const ALL: [&str; 6] = ["==", "!=", "<", "<=", ">", ">="];

    #[test]
    fn comparisons_respect_signedness_and_nan() {
        check_comparisons::<i32>(
            "int",
            &ALL,
            &[
                (0, 0),
                (1, 2),
                (2, 1),
                (-1, 0),
                (0, -1),
                (i32::MIN, i32::MAX),
                (i32::MAX, i32::MIN),
            ],
        );
        check_comparisons::<i64>(
            "long",
            &ALL,
            &[
                (0, 0),
                (1, 2),
                (-1, 0),
                (0, -1),
                (i64::MIN, i64::MAX),
                (i64::MAX, i64::MIN),
            ],
        );
        check_comparisons::<isize>("nint", &ALL, &[(0, 0), (1, 2), (-1, 0), (0, -1)]);
        check_comparisons::<u32>(
            "uint",
            &ALL,
            &[
                (0, 0),
                (1, 2),
                (2, 1),
                (4_000_000_000, 1),
                (1, 4_000_000_000),
                (u32::MAX, 0),
            ],
        );
        check_comparisons::<u64>(
            "ulong",
            &ALL,
            &[(0, 0), (1, 2), (u64::MAX, 1), (1, u64::MAX), (1 << 63, 5)],
        );
        check_comparisons::<usize>(
            "nuint",
            &ALL,
            &[(0, 0), (1, 2), (usize::MAX, 1), (1, usize::MAX)],
        );
        // `char` is unsigned, and `bool` only compares for equality.
        check_comparisons::<u32>(
            "char",
            &ALL,
            &[
                (0x61, 0x61),
                (0x61, 0x62),
                (0x62, 0x61),
                (0x10FFFF, 0x61),
                (0x61, 0x10FFFF),
            ],
        );
        check_comparisons::<bool>(
            "bool",
            &["==", "!="],
            &[(true, true), (true, false), (false, true), (false, false)],
        );
        let reals = [
            (0.0, 0.0),
            (1.5, 2.5),
            (2.5, 1.5),
            (-0.0, 0.0),
            (-1.0, 1.0),
            (f64::NAN, f64::NAN),
            (f64::NAN, 1.0),
            (1.0, f64::NAN),
            (f64::INFINITY, 1.0),
        ];
        check_comparisons::<f64>("double", &ALL, &reals);
        let singles = reals.map(|(a, b)| (a as f32, b as f32));
        check_comparisons::<f32>("float", &ALL, &singles);
    }

    #[test]
    fn shift_counts_are_masked() {
        let program = debug(
            "Shl(move a: int, move n: int): int => a << n;\n\
             ShlL(move a: long, move n: int): long => a << n;\n\
             Shr(move a: int, move n: int): int => a >> n;\n\
             UShr(move a: int, move n: int): int => a >>> n;\n",
        );
        let int = |name| program.get::<extern "C" fn(i32, i32) -> i32>(name);
        assert_eq!(int("Shl")(1, 33), 2);
        assert_eq!(
            program.get::<extern "C" fn(i64, i32) -> i64>("ShlL")(1, 65),
            2
        );
        assert_eq!(int("Shr")(-8, 1), -4);
        assert_eq!(int("UShr")(-8, 1), 0x7FFF_FFFC);
    }

    /// `value` shifted by `count` in an integer type of `bits` bits, as
    /// constant evaluation does it (mora-mir `eval/arith.rs`).
    fn shifted(op: BinOp, signed: bool, bits: u32, value: i128, count: i128) -> i128 {
        let mask = (1i128 << bits) - 1;
        let wrap = |value: i128| {
            let low = value & mask;
            if signed && low >= 1 << (bits - 1) {
                low - (1 << bits)
            } else {
                low
            }
        };
        let count = (count & i128::from(bits.max(32) - 1)) as u32;
        match op {
            BinOp::Shl => wrap(value << count),
            BinOp::Shr => value >> count,
            _ => wrap((value & mask) >> count),
        }
    }

    /// Shifts every value by every count with each shift on `ty`, whose
    /// values pass as `T`; `wide` widens one to `i128`.
    fn check_shifts<T: Copy + Debug>(
        ty: Primitive,
        signed: bool,
        values: &[T],
        wide: impl Fn(T) -> i128,
    ) {
        let bits = (size_of::<T>() * 8) as u32;
        // No source keeps the type of a shift of a value narrower than
        // `int` (it promotes), so the MIR of an `int` shift is retyped.
        let retype = bits < 32;
        let name = if retype { "int" } else { ty.keyword() };
        for (op, text) in [(BinOp::Shl, "<<"), (BinOp::Shr, ">>"), (BinOp::UShr, ">>>")] {
            let source = format!("F(move a: {name}, move n: int): {name} => a {text} n;\n");
            let program = jit_one_with(&source, "F", |body| {
                for (index, local) in body.locals.iter_mut().enumerate() {
                    let value = local.source.as_ref().is_some_and(|s| s.name == "a");
                    if retype && (index == 0 || value) {
                        local.ty = LocalTy::Value(Ty::Primitive(ty));
                    }
                }
            });
            let f = func::<extern "C" fn(T, i32) -> T>(&program, "F");
            for &value in values {
                for count in [0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, -1] {
                    let expected = shifted(op, signed, bits, wide(value), count.into());
                    let found = wide(f(value, count));
                    assert_eq!(found, expected, "{value:?} {text} {count} on {name}");
                }
            }
        }
    }

    #[test]
    fn shifts_follow_constant_evaluation() {
        check_shifts::<i32>(
            Primitive::Int,
            true,
            &[0, 1, -1, -8, i32::MAX, i32::MIN, 0x1234_5678],
            |v| v.into(),
        );
        check_shifts::<u32>(
            Primitive::UInt,
            false,
            &[0, 1, 8, u32::MAX, 0x8000_0000, 0x1234_5678],
            |v| v.into(),
        );
        check_shifts::<i64>(
            Primitive::Long,
            true,
            &[0, 1, -1, -8, i64::MAX, i64::MIN, 0x1234_5678_9ABC_DEF0],
            |v| v.into(),
        );
        check_shifts::<u64>(
            Primitive::ULong,
            false,
            &[0, 1, u64::MAX, 1 << 63, 0x1234_5678_9ABC_DEF0],
            |v| v.into(),
        );
        check_shifts::<i8>(
            Primitive::SByte,
            true,
            &[0, 1, -1, -8, 127, -128, 0x55],
            |v| v.into(),
        );
        check_shifts::<u8>(
            Primitive::Byte,
            false,
            &[0, 1, 8, 127, 128, 200, 255],
            |v| v.into(),
        );
        check_shifts::<i16>(
            Primitive::Short,
            true,
            &[0, 1, -1, -8, 32767, -32768, 0x5555],
            |v| v.into(),
        );
        check_shifts::<u16>(
            Primitive::UShort,
            false,
            &[0, 1, 8, 32768, 65535, 0xAAAA],
            |v| v.into(),
        );
    }

    #[test]
    fn nint_is_pointer_sized() {
        let program = debug(
            "Add(move a: nint, move b: nint): nint => a + b;\n\
             Shift(move a: nint, move n: int): nint => a << n;\n\
             Up(move a: nuint, move n: int): nuint => a >> n;\n",
        );
        let a = 1isize << 40;
        assert_eq!(
            program.get::<extern "C" fn(isize, isize) -> isize>("Add")(a, a),
            1 << 41
        );
        assert_eq!(
            program.get::<extern "C" fn(isize, i32) -> isize>("Shift")(1, 40),
            a
        );
        assert_eq!(
            program.get::<extern "C" fn(usize, i32) -> usize>("Up")(1 << 63, 63),
            1
        );
    }

    #[test]
    fn bitwise_and_logical_not() {
        let program = debug(
            "Not(move a: int): int => ~a;\n\
             NotU(move a: uint): uint => ~a;\n\
             Neg(move a: long): long => -a;\n\
             Bang(move a: bool): bool => !a;\n\
             And(move a: int, move b: int): int => a & b;\n\
             Or(move a: int, move b: int): int => a | b;\n\
             Xor(move a: int, move b: int): int => a ^ b;\n\
             AndB(move a: bool, move b: bool): bool => a & b;\n\
             OrB(move a: bool, move b: bool): bool => a | b;\n\
             XorB(move a: bool, move b: bool): bool => a ^ b;\n",
        );
        assert_eq!(program.get::<extern "C" fn(i32) -> i32>("Not")(0), -1);
        assert_eq!(program.get::<extern "C" fn(u32) -> u32>("NotU")(5), !5u32);
        assert_eq!(
            program.get::<extern "C" fn(i64) -> i64>("Neg")(i64::MIN),
            i64::MIN
        );
        let bang = program.get::<extern "C" fn(bool) -> bool>("Bang");
        assert_eq!((bang(true), bang(false)), (false, true));
        let int = |name| program.get::<extern "C" fn(i32, i32) -> i32>(name);
        assert_eq!(
            (int("And")(6, 3), int("Or")(6, 3), int("Xor")(6, 3)),
            (2, 7, 5)
        );
        let logic = |name| program.get::<extern "C" fn(bool, bool) -> bool>(name);
        for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(logic("AndB")(a, b), a & b);
            assert_eq!(logic("OrB")(a, b), a | b);
            assert_eq!(logic("XorB")(a, b), a ^ b);
        }
    }

    /// The Mora integer types, with their Rust twins in the macros below.
    const INTEGERS: [&str; 10] = [
        "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "nint", "nuint",
    ];

    /// A program with a function `Cast_<from>_<to>` that casts its parameter,
    /// for each pair of `from` and `to`.
    fn cast_program(from: &[&str], to: &[&str]) -> Funcs {
        let mut text = String::new();
        for from in from {
            for to in to {
                text += &format!("Cast_{from}_{to}(move a: {from}): {to} => ({to})a;\n");
            }
        }
        debug(&text)
    }

    /// Checks `$value as $to` for each integer `$to` against the cast
    /// function the program has.
    macro_rules! casts_to {
        ($program:ident, $from:ty, $name:literal, $values:expr, [$($to:ty => $to_name:literal),*]) => {
            $(
                let f = $program.get::<extern "C" fn($from) -> $to>(concat!("Cast_", $name, "_", $to_name));
                for value in $values {
                    assert_eq!(f(value), value as $to, "({}){:?}", $to_name, value);
                }
            )*
        };
    }

    #[test]
    fn integer_casts_extend_by_the_source_and_reduce() {
        let program = cast_program(&INTEGERS, &INTEGERS);
        macro_rules! from {
            ($from:ty, $name:literal, $values:expr) => {
                casts_to!(program, $from, $name, $values, [
                    u8 => "byte", i8 => "sbyte", i16 => "short", u16 => "ushort", i32 => "int",
                    u32 => "uint", i64 => "long", u64 => "ulong", isize => "nint", usize => "nuint"
                ]);
            };
        }
        from!(u8, "byte", [0u8, 1, 127, 128, 255]);
        from!(i8, "sbyte", [0i8, 1, -1, 127, -128]);
        from!(i16, "short", [0i16, 1, -1, 300, i16::MAX, i16::MIN]);
        from!(u16, "ushort", [0u16, 1, 300, 0x8000, u16::MAX]);
        from!(i32, "int", [0i32, 1, -1, 300, i32::MAX, i32::MIN, -129]);
        from!(u32, "uint", [0u32, 1, 300, 0x8000_0000, u32::MAX]);
        from!(
            i64,
            "long",
            [0i64, 1, -1, 300, i64::MAX, i64::MIN, 0x1_0000_0001]
        );
        from!(
            u64,
            "ulong",
            [0u64, 1, 300, 1 << 63, u64::MAX, 0x1_0000_0001]
        );
        from!(isize, "nint", [0isize, -1, 300, isize::MAX, isize::MIN]);
        from!(usize, "nuint", [0usize, 300, 1 << 63, usize::MAX]);
    }

    /// Checks `value as $to` for a float `$from` against the cast function
    /// the program has, comparing the bits of a float and NaN as NaN.
    macro_rules! float_casts {
        ($program:ident, $from:ty, $name:literal, $values:expr, [$($to:ty => $to_name:literal),*]) => {
            $(
                let f = $program.get::<extern "C" fn($from) -> $to>(concat!("Cast_", $name, "_", $to_name));
                for value in $values {
                    assert_eq!(f(value), value as $to, "({}){:?}", $to_name, value);
                }
            )*
        };
    }

    #[test]
    fn float_to_integer_saturates_and_nan_is_zero() {
        let program = cast_program(&["float", "double"], &INTEGERS);
        let doubles = [
            f64::NAN,
            0.0,
            -0.0,
            0.5,
            1.5,
            -1.5,
            255.5,
            256.0,
            300.7,
            -1.0,
            -129.0,
            127.9,
            32767.9,
            32768.0,
            65535.9,
            65536.0,
            -32769.0,
            2147483647.5,
            2147483648.0,
            -2147483648.5,
            3e9,
            4294967295.5,
            4294967296.0,
            1e10,
            -1e10,
            9.2e18,
            9.3e18,
            -9.3e18,
            1.0e19,
            1.8e19,
            1.9e19,
            1e30,
            -1e30,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        let singles = doubles.map(|v| v as f32);
        macro_rules! targets {
            ($from:ty, $name:literal, $values:expr) => {
                float_casts!(program, $from, $name, $values, [
                    u8 => "byte", i8 => "sbyte", i16 => "short", u16 => "ushort", i32 => "int",
                    u32 => "uint", i64 => "long", u64 => "ulong", isize => "nint", usize => "nuint"
                ]);
            };
        }
        targets!(f64, "double", doubles);
        targets!(f32, "float", singles);
    }

    #[test]
    fn integer_to_float_rounds_once() {
        let program = cast_program(&INTEGERS, &["float", "double"]);
        macro_rules! to_float {
            ($from:ty, $name:literal, $values:expr) => {
                casts_to!(program, $from, $name, $values, [f32 => "float", f64 => "double"]);
            };
        }
        to_float!(u8, "byte", [0u8, 1, 255]);
        to_float!(i8, "sbyte", [0i8, -1, -128, 127]);
        to_float!(i16, "short", [0i16, -1, i16::MIN, i16::MAX]);
        to_float!(u16, "ushort", [0u16, 1, u16::MAX]);
        to_float!(
            i32,
            "int",
            [0i32, -1, 16_777_217, -16_777_217, i32::MAX, i32::MIN]
        );
        to_float!(u32, "uint", [0u32, 16_777_217, 0x8000_0000, u32::MAX]);
        to_float!(
            i64,
            "long",
            [
                0i64,
                -1,
                i64::MAX,
                i64::MIN,
                0x4000_0040_0000_0001,
                -0x4000_0040_0000_0001,
                0x0020_0000_0000_0001
            ]
        );
        to_float!(
            u64,
            "ulong",
            [
                0u64,
                1,
                u64::MAX,
                1 << 63,
                0x8000_0000_0000_0001,
                0x8000_0080_0000_0001,
                0x8000_0000_0000_0401,
                0xFFFF_FFFF_FFFF_F400
            ]
        );
        to_float!(
            isize,
            "nint",
            [0isize, -1, isize::MAX, 0x4000_0040_0000_0001]
        );
        to_float!(
            usize,
            "nuint",
            [0usize, 1, usize::MAX, 0x8000_0080_0000_0001]
        );
    }

    #[test]
    fn float_and_double_convert_to_each_other() {
        let program = cast_program(&["float", "double"], &["float", "double"]);
        let promote = program.get::<extern "C" fn(f32) -> f64>("Cast_float_double");
        for value in [0.1f32, -0.0, 1e30, f32::INFINITY, f32::MIN_POSITIVE] {
            assert_eq!(promote(value).to_bits(), f64::from(value).to_bits());
        }
        assert!(promote(f32::NAN).is_nan());
        let demote = program.get::<extern "C" fn(f64) -> f32>("Cast_double_float");
        for value in [
            0.1f64,
            -0.0,
            1e40,
            -1e40,
            1e-50,
            16_777_217.0,
            f64::INFINITY,
        ] {
            assert_eq!(demote(value).to_bits(), (value as f32).to_bits());
        }
        assert!(demote(f64::NAN).is_nan());
        let same = program.get::<extern "C" fn(f32) -> f32>("Cast_float_float");
        assert_eq!(same(0.1).to_bits(), 0.1f32.to_bits());
        let same = program.get::<extern "C" fn(f64) -> f64>("Cast_double_double");
        assert_eq!(same(0.1).to_bits(), 0.1f64.to_bits());
    }

    #[test]
    fn casts_follow_constant_evaluation() {
        let program = debug(
            "Byte(move a: int): byte => (byte)a;\n\
             FromNan(move a: double): int => (int)a;\n\
             Big(move a: double): int => (int)a;\n\
             Uint(move a: double): uint => (uint)a;\n\
             Rounded(move a: int): float => (float)a;\n\
             Widen(move a: uint): long => a;\n\
             WidenSigned(move a: int): long => a;\n\
             Single(move a: double): double => (double)(float)a;\n\
             Huge(move a: double): ulong => (ulong)a;\n\
             Promote(move a: float): double => a;\n\
             ToDouble(move a: int): double => a;\n",
        );
        assert_eq!(program.get::<extern "C" fn(i32) -> u8>("Byte")(300), 44);
        assert_eq!(
            program.get::<extern "C" fn(f64) -> i32>("FromNan")(f64::NAN),
            0
        );
        assert_eq!(
            program.get::<extern "C" fn(f64) -> i32>("Big")(1e10),
            i32::MAX
        );
        assert_eq!(
            program.get::<extern "C" fn(f64) -> i32>("Big")(-1e10),
            i32::MIN
        );
        assert_eq!(program.get::<extern "C" fn(f64) -> u32>("Uint")(-1.0), 0);
        assert_eq!(
            program.get::<extern "C" fn(i32) -> f32>("Rounded")(16_777_217),
            16_777_216.0
        );
        assert_eq!(
            program.get::<extern "C" fn(u32) -> i64>("Widen")(4_294_967_295),
            4_294_967_295
        );
        assert_eq!(
            program.get::<extern "C" fn(i32) -> i64>("WidenSigned")(-1),
            -1
        );
        assert_eq!(
            program.get::<extern "C" fn(f64) -> f64>("Single")(0.1),
            0.100_000_001_490_116_12
        );
        assert_eq!(
            program.get::<extern "C" fn(f64) -> u64>("Huge")(1e30),
            u64::MAX
        );
        assert_eq!(
            program.get::<extern "C" fn(f32) -> f64>("Promote")(0.1),
            0.100_000_001_490_116_12
        );
        assert_eq!(
            program.get::<extern "C" fn(i32) -> f64>("ToDouble")(-7),
            -7.0
        );
    }

    /// The program of `text`'s function `F` alone, after `edit` changes the
    /// rvalue of its first statement, which is its operator.
    fn with_rvalue(text: &str, edit: impl FnOnce(&mut Rvalue)) -> JitProgram {
        jit_one_with(text, "F", |body| {
            if let StatementKind::Assign(_, rvalue) = &mut body.blocks[0].statements[0].kind {
                edit(rvalue);
            }
        })
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: the operator `/` is not lowered until M1.12"
    )]
    fn division_waits_for_its_checks() {
        jit_one("F(move a: int, move b: int): int => a / b;\n", "F");
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: the operator `%` is not lowered until M1.12"
    )]
    fn remainder_waits_for_its_checks() {
        jit_one("F(move a: int, move b: int): int => a % b;\n", "F");
    }

    #[test]
    #[should_panic(
        expected = "an operator on `Const(Unit)`, which is not a scalar is not lowered until M1.12"
    )]
    fn operators_on_aggregates_wait() {
        with_rvalue("F(move a: int, move b: int): bool => a == b;\n", |rvalue| {
            if let Rvalue::Binary(_, _, right) = rvalue {
                *right = Operand::Const(Const::Unit);
            }
        });
    }

    #[test]
    #[should_panic(expected = "invalid MIR: `+%` on a Float")]
    fn wrapping_operators_need_integers() {
        with_rvalue(
            "F(move a: float, move b: float): float => a + b;\n",
            |rvalue| {
                if let Rvalue::Binary(op, ..) = rvalue {
                    *op = BinOp::WrappingAdd;
                }
            },
        );
    }

    #[test]
    #[should_panic(expected = "invalid MIR: `<<` of a Double by a Double")]
    fn shifts_need_integers() {
        jit_one_with(
            "F(move a: int, move n: int): int => a << n;\n",
            "F",
            |body| {
                for local in &mut body.locals {
                    local.ty = LocalTy::Value(Ty::Primitive(Primitive::Double));
                }
            },
        );
    }

    #[test]
    #[should_panic(expected = "invalid MIR: `-` on a Bool")]
    fn negation_needs_a_number() {
        with_rvalue("F(move a: bool): bool => !a;\n", |rvalue| {
            if let Rvalue::Unary(op, _) = rvalue {
                *op = UnOp::Neg;
            }
        });
    }

    #[test]
    #[should_panic(expected = "invalid MIR: a cast to `String`, which is not a scalar")]
    fn casts_go_to_scalars() {
        with_rvalue("F(move a: int): long => (long)a;\n", |rvalue| {
            if let Rvalue::Cast(_, _, ty) = rvalue {
                *ty = Ty::String;
            }
        });
    }

    #[test]
    #[should_panic(
        expected = "invalid MIR: a cast from a Bool to a Int { bytes: 8, signed: true }"
    )]
    fn casts_go_between_numbers() {
        jit_one_with("F(move a: int): long => (long)a;\n", "F", |body| {
            body.locals[1].ty = LocalTy::Value(Ty::Primitive(Primitive::Bool));
        });
    }

    #[test]
    fn a_cast_to_the_same_type_keeps_the_value() {
        let program = jit_one_with("F(move a: int): long => (long)a;\n", "F", |body| {
            body.locals[0].ty = LocalTy::Value(Ty::Primitive(Primitive::Int));
            for block in &mut body.blocks {
                for statement in &mut block.statements {
                    if let StatementKind::Assign(_, Rvalue::Cast(_, _, ty)) = &mut statement.kind {
                        *ty = Ty::Primitive(Primitive::Int);
                    }
                }
            }
        });
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "F")(-5), -5);
    }
}
