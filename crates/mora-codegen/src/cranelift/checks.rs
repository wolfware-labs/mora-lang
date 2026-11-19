//! Checked arithmetic and division: the overflow checks of a debug build,
//! the zero divisor of both profiles, and the panics they call (§11.4).

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{GlobalValue, InstBuilder, Type, Value, types};
use mora_mir::BinOp;
use mora_rt::panic::OverflowOp;

use super::function::{Lowering, UNREACHABLE, broken};
use super::runtime::{DIVISION_BY_ZERO, RuntimeFn};
use crate::Profile;
use crate::layout::Scalar;

/// An integer operation whose overflow a debug build checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arith {
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// Unary `-`.
    Neg,
}

impl Arith {
    /// The checked operation `op` is, if it is one.
    pub(super) fn of(op: BinOp) -> Option<Arith> {
        match op {
            BinOp::Add => Some(Arith::Add),
            BinOp::Sub => Some(Arith::Sub),
            BinOp::Mul => Some(Arith::Mul),
            _ => None,
        }
    }

    /// How the panic names it.
    fn overflow(self) -> OverflowOp {
        match self {
            Arith::Add => OverflowOp::Add,
            Arith::Sub => OverflowOp::Sub,
            Arith::Mul => OverflowOp::Mul,
            Arith::Neg => OverflowOp::Neg,
        }
    }
}

impl Lowering<'_, '_> {
    /// The result of `arith` on the integers `left` and `right`, which are
    /// signed when `signed`; panics when it overflows. The negation of
    /// `left` is the subtraction of `left` from `right`, which is 0.
    pub(super) fn checked(
        &mut self,
        arith: Arith,
        signed: bool,
        left: Value,
        right: Value,
    ) -> Value {
        let (value, overflow) = match arith {
            Arith::Add => self.add_overflowing(signed, left, right),
            Arith::Sub => self.sub_overflowing(signed, left, right),
            Arith::Mul => self.mul_overflowing(signed, left, right),
            Arith::Neg => self.sub_overflowing(signed, right, left),
        };
        self.panic_if(overflow, |lowering| {
            lowering.panic_overflow(arith.overflow(), signed, left, right);
        });
        value
    }

    // The overflow flags come from comparisons, not from Cranelift's
    // `*_overflow` instructions, which not every instruction set implements:
    // riscv64 has `uadd_overflow` only.

    /// `left + right` wrapped, and whether it overflowed.
    fn add_overflowing(&mut self, signed: bool, left: Value, right: Value) -> (Value, Value) {
        let sum = self.builder.ins().iadd(left, right);
        let overflow = if signed {
            // The operands have the same sign, and the sum another.
            let left_differs = self.builder.ins().bxor(left, sum);
            let right_differs = self.builder.ins().bxor(right, sum);
            let both = self.builder.ins().band(left_differs, right_differs);
            self.builder
                .ins()
                .icmp_imm_s(IntCC::SignedLessThan, both, 0)
        } else {
            // The sum wrapped past zero.
            self.builder.ins().icmp(IntCC::UnsignedLessThan, sum, left)
        };
        (sum, overflow)
    }

    /// `left - right` wrapped, and whether it overflowed.
    fn sub_overflowing(&mut self, signed: bool, left: Value, right: Value) -> (Value, Value) {
        let difference = self.builder.ins().isub(left, right);
        let overflow = if signed {
            // The operands have different signs, and the difference has the
            // sign of the right one.
            let operands_differ = self.builder.ins().bxor(left, right);
            let result_differs = self.builder.ins().bxor(left, difference);
            let both = self.builder.ins().band(operands_differ, result_differs);
            self.builder
                .ins()
                .icmp_imm_s(IntCC::SignedLessThan, both, 0)
        } else {
            self.builder
                .ins()
                .icmp(IntCC::UnsignedLessThan, left, right)
        };
        (difference, overflow)
    }

    /// `left * right` wrapped, and whether it overflowed.
    fn mul_overflowing(&mut self, signed: bool, left: Value, right: Value) -> (Value, Value) {
        let product = self.builder.ins().imul(left, right);
        let ty = self.builder.func.dfg.value_type(left);
        let overflow = if ty.bits() == 64 {
            // The high half of the product is the extension of the low one
            // when it did not overflow: its sign for a signed product, 0
            // for an unsigned one.
            if signed {
                let high = self.builder.ins().smulhi(left, right);
                let sign = self.builder.ins().sshr_imm_s(product, 63);
                self.builder.ins().icmp(IntCC::NotEqual, high, sign)
            } else {
                let high = self.builder.ins().umulhi(left, right);
                self.builder.ins().icmp_imm_s(IntCC::NotEqual, high, 0)
            }
        } else {
            // The exact product fits in 64 bits; it overflowed when the
            // wrapped one extends to something else.
            let wide_left = self.resize(left, types::I64, signed);
            let wide_right = self.resize(right, types::I64, signed);
            let exact = self.builder.ins().imul(wide_left, wide_right);
            let extended = self.resize(product, types::I64, signed);
            self.builder.ins().icmp(IntCC::NotEqual, extended, exact)
        };
        (product, overflow)
    }

    /// The quotient or remainder of the numbers `left` and `right` of the
    /// type `scalar`. An integer divisor of zero panics; so does, in a debug
    /// build, the minimum of a signed type divided by -1, which a release
    /// build wraps.
    ///
    /// # Panics
    ///
    /// With an internal error for operands that are not numbers.
    pub(super) fn divide(&mut self, op: BinOp, scalar: Scalar, left: Value, right: Value) -> Value {
        let div = op == BinOp::Div;
        match scalar {
            Scalar::Int { signed, .. } => self.divide_int(div, signed, left, right),
            Scalar::Float | Scalar::Double if div => self.builder.ins().fdiv(left, right),
            Scalar::Float | Scalar::Double => {
                let function = if scalar == Scalar::Float {
                    RuntimeFn::Fmodf
                } else {
                    RuntimeFn::Fmod
                };
                let call = self
                    .builder
                    .ins()
                    .call(self.env.function(function), &[left, right]);
                self.builder.inst_results(call)[0]
            }
            _ => broken(self.body, &format!("`{}` on a {scalar:?}", op.text())),
        }
    }

    /// `left / right` when `div`, else `left % right`, on integers.
    fn divide_int(&mut self, div: bool, signed: bool, left: Value, right: Value) -> Value {
        let ty = self.builder.func.dfg.value_type(left);
        let zero = self.builder.ins().iconst(ty, 0);
        let by_zero = self.builder.ins().icmp(IntCC::Equal, right, zero);
        let message = self.env.zero_message;
        self.panic_if(by_zero, |lowering| {
            lowering.panic_message(message, DIVISION_BY_ZERO.len());
        });
        if !signed {
            return if div {
                self.builder.ins().udiv(left, right)
            } else {
                self.builder.ins().urem(left, right)
            };
        }
        let minus_one = self.builder.ins().iconst(ty, -1);
        let by_minus_one = self.builder.ins().icmp(IntCC::Equal, right, minus_one);
        if self.env.profile == Profile::Debug {
            let min = self.builder.ins().iconst(ty, i64::MIN >> (64 - ty.bits()));
            let is_min = self.builder.ins().icmp(IntCC::Equal, left, min);
            let overflows = self.builder.ins().band(is_min, by_minus_one);
            let op = if div {
                OverflowOp::Div
            } else {
                OverflowOp::Rem
            };
            self.panic_if(overflows, |lowering| {
                lowering.panic_overflow(op, true, left, right);
            });
            return if div {
                self.builder.ins().sdiv(left, right)
            } else {
                self.builder.ins().srem(left, right)
            };
        }
        // A divisor of -1 becomes 1, so that no instruction traps; the
        // result of `x / -1` is `-x` and of `x % -1` is 0, both wrapping.
        let one = self.builder.ins().iconst(ty, 1);
        let divisor = self.builder.ins().select(by_minus_one, one, right);
        if div {
            let quotient = self.builder.ins().sdiv(left, divisor);
            let negated = self.builder.ins().ineg(left);
            self.builder.ins().select(by_minus_one, negated, quotient)
        } else {
            let remainder = self.builder.ins().srem(left, divisor);
            self.builder.ins().select(by_minus_one, zero, remainder)
        }
    }

    /// Runs `panic` in a block that is rarely reached and ends the program,
    /// when `condition` holds; the code that follows is in a new block that
    /// runs otherwise.
    fn panic_if(&mut self, condition: Value, panic: impl FnOnce(&mut Self)) {
        let cold = self.builder.create_block();
        let next = self.builder.create_block();
        self.builder.set_cold_block(cold);
        self.builder.ins().brif(condition, cold, &[], next, &[]);
        self.builder.switch_to_block(cold);
        panic(self);
        // A panic does not return.
        self.builder.ins().trap(UNREACHABLE);
        self.builder.switch_to_block(next);
    }

    /// The file, its length, and the line and column of the statement being
    /// lowered, the leading arguments of every panic.
    ///
    /// # Panics
    ///
    /// With an internal error when the statement's source starts outside its
    /// file, which no MIR lowering gives.
    fn location_args(&mut self) -> [Value; 4] {
        let Some((line, column)) = self.env.location(self.span) else {
            broken(self.body, "a source location outside its file");
        };
        let path = self.builder.ins().symbol_value(self.pointer, self.env.path);
        let length = self.constant_of(self.pointer, self.env.path_len as i64);
        let line = self.constant_of(types::I32, i64::from(line));
        let column = self.constant_of(types::I32, i64::from(column));
        [path, length, line, column]
    }

    /// The integer constant `value` of the type `ty`.
    fn constant_of(&mut self, ty: Type, value: i64) -> Value {
        self.builder.ins().iconst(ty, value)
    }

    /// Calls `mora_rt_panic` with the `length` bytes of `message`.
    fn panic_message(&mut self, message: GlobalValue, length: usize) {
        let [path, path_len, line, column] = self.location_args();
        let message = self.builder.ins().symbol_value(self.pointer, message);
        let length = self.constant_of(self.pointer, length as i64);
        let function = self.env.function(RuntimeFn::Panic);
        self.builder
            .ins()
            .call(function, &[path, path_len, line, column, message, length]);
    }

    /// Calls `mora_rt_panic_overflow` for `op` on `left` and `right`, which
    /// are signed when `signed`.
    fn panic_overflow(&mut self, op: OverflowOp, signed: bool, left: Value, right: Value) {
        let [path, path_len, line, column] = self.location_args();
        let left = self.resize(left, types::I64, signed);
        let right = self.resize(right, types::I64, signed);
        let op = self.constant_of(types::I32, i64::from(op as u32));
        let signed = self.constant_of(types::I32, i64::from(signed));
        let function = self.env.function(RuntimeFn::PanicOverflow);
        self.builder.ins().call(
            function,
            &[path, path_len, line, column, op, signed, left, right],
        );
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::{Primitive, Ty};
    use mora_mir::LocalTy;

    use super::super::function::tests::{jit_one_in, jit_one_with_in, lowered_in};
    use super::super::runtime::RuntimeFn;
    use crate::Profile;
    use crate::test_util::{child, func, object_of, only_in_child};

    /// `text`, a function `F` of two `int`s, and a call of it.
    fn entry(text: &str) -> String {
        format!("{text}F(1, 2);\n")
    }

    /// What a program with a panic that `test` runs in a child process
    /// prints, and how it ends.
    fn panics(test: &str, stderr: &str) {
        let out = child(&format!("cranelift::checks::tests::{test}"));
        assert_eq!(out.status.code(), Some(101), "{test}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{test}");
    }

    /// Calls the function `F` of `text` with `a` and `b`.
    fn call<T: Copy, R>(text: &str, profile: Profile, a: T, b: T) -> R {
        let program = jit_one_in(text, "F", profile);
        func::<extern "C" fn(T, T) -> R>(&program, "F")(a, b)
    }

    /// Calls the function `F` of `text` with `a` and `b` after its locals are
    /// retyped as `ty`, which source code cannot do for a type narrower than
    /// `int`: `a + b` of two `sbyte`s is an `int`.
    fn call_as<T: Copy, R>(text: &str, ty: Primitive, profile: Profile, a: T, b: T) -> R {
        let program = jit_one_with_in(text, "F", profile, |body| {
            for local in &mut body.locals {
                local.ty = LocalTy::Value(Ty::Primitive(ty));
            }
        });
        func::<extern "C" fn(T, T) -> R>(&program, "F")(a, b)
    }

    const INT_ADD: &str = "F(move a: int, move b: int): int => a + b;\n";
    /// The same with `in` parameters, which pass an `int` as its value, as the
    /// column of a panic is counted in it.
    const REF_ADD: &str = "F(a: int, b: int): int => a + b;\n";
    const INT_DIV: &str = "F(move a: int, move b: int): int => a / b;\n";
    const INT_REM: &str = "F(move a: int, move b: int): int => a % b;\n";
    const LONG_DIV: &str = "F(move a: long, move b: long): long => a / b;\n";

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: invalid MIR: a source \
                    location outside its file"
    )]
    fn a_check_at_a_span_outside_its_file_is_an_internal_error() {
        jit_one_with_in(INT_ADD, "F", Profile::Debug, |body| {
            let past = mora_span::TextRange::empty(100_000.into());
            for statement in body
                .blocks
                .iter_mut()
                .flat_map(|block| &mut block.statements)
            {
                statement.span.range = past;
            }
        });
    }

    #[test]
    fn debug_add_overflow_panics_with_the_operands() {
        panics(
            "child_add",
            "panic at src/Program.mr:1:27: arithmetic overflow: 2147483647 + 1\n",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_add() {
        only_in_child(|| {
            call::<i32, i32>(REF_ADD, Profile::Debug, i32::MAX, 1);
        });
    }

    #[test]
    fn debug_sub_mul_and_neg_overflow_panic() {
        panics(
            "child_sub",
            "panic at src/Program.mr:1:40: arithmetic overflow: 0 - 1\n",
        );
        panics(
            "child_mul",
            "panic at src/Program.mr:1:40: arithmetic overflow: 9223372036854775807 * 2\n",
        );
        panics(
            "child_neg",
            "panic at src/Program.mr:1:24: arithmetic overflow: -(-2147483648)\n",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_sub() {
        only_in_child(|| {
            let text = "F(move a: uint, move b: uint): uint => a - b;\n";
            call::<u32, u32>(text, Profile::Debug, 0, 1);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_mul() {
        only_in_child(|| {
            let text = "F(move a: long, move b: long): long => a * b;\n";
            call::<i64, i64>(text, Profile::Debug, i64::MAX, 2);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_neg() {
        only_in_child(|| {
            let program = jit_one_in("F(move a: int): int => -a;\n", "F", Profile::Debug);
            func::<extern "C" fn(i32) -> i32>(&program, "F")(i32::MIN);
        });
    }

    /// The function `F` of two `ty`s that applies `op`.
    fn operator(ty: &str, op: &str) -> String {
        format!("F(move a: {ty}, move b: {ty}): {ty} => a {op} b;\n")
    }

    /// Child tests that each overflow one operation, and the parent test
    /// that checks each one's message: the type, the Rust type, the
    /// operator, the operands and how the panic prints them.
    macro_rules! overflows {
        ($($name:ident: $ty:literal $rust:ty, $op:literal, $a:expr, $b:expr => $shown:literal;)*) => {
            $(
                #[test]
                #[ignore = "runs in a child process"]
                fn $name() {
                    only_in_child(|| {
                        call::<$rust, $rust>(&operator($ty, $op), Profile::Debug, $a, $b);
                    });
                }
            )*

            #[test]
            fn every_overflowing_operation_panics() {
                $(
                    let column = operator($ty, $op).find("=> ").unwrap() + 4;
                    panics(
                        stringify!($name),
                        &format!(
                            "panic at src/Program.mr:1:{column}: arithmetic overflow: {}\n",
                            $shown
                        ),
                    );
                )*
            }
        };
    }

    overflows! {
        child_int_add_min: "int" i32, "+", i32::MIN, -1 => "-2147483648 + -1";
        child_int_sub_min: "int" i32, "-", i32::MIN, 1 => "-2147483648 - 1";
        child_int_sub_max: "int" i32, "-", i32::MAX, -1 => "2147483647 - -1";
        child_int_sub_zero: "int" i32, "-", 0, i32::MIN => "0 - -2147483648";
        child_int_mul_min: "int" i32, "*", i32::MIN, -1 => "-2147483648 * -1";
        child_int_mul_carry: "int" i32, "*", 65536, 32768 => "65536 * 32768";
        child_int_mul_negative: "int" i32, "*", -65536, 32769 => "-65536 * 32769";
        child_uint_add: "uint" u32, "+", u32::MAX, 1 => "4294967295 + 1";
        child_uint_add_left: "uint" u32, "+", 1, u32::MAX => "1 + 4294967295";
        child_uint_sub: "uint" u32, "-", 1, 2 => "1 - 2";
        child_uint_mul: "uint" u32, "*", 65536, 65536 => "65536 * 65536";
        child_long_add: "long" i64, "+", i64::MAX, 1 => "9223372036854775807 + 1";
        child_long_add_min: "long" i64, "+", i64::MIN, -1 => "-9223372036854775808 + -1";
        child_long_sub: "long" i64, "-", i64::MIN, 1 => "-9223372036854775808 - 1";
        child_long_sub_zero: "long" i64, "-", 0, i64::MIN => "0 - -9223372036854775808";
        child_long_mul_min: "long" i64, "*", i64::MIN, -1 => "-9223372036854775808 * -1";
        child_long_mul_carry: "long" i64, "*", 1 << 32, 1 << 31 => "4294967296 * 2147483648";
        child_long_mul_negative: "long" i64, "*", -(1 << 32), (1 << 31) + 1 => "-4294967296 * 2147483649";
        child_ulong_add: "ulong" u64, "+", u64::MAX, 1 => "18446744073709551615 + 1";
        child_ulong_sub: "ulong" u64, "-", 0, 1 => "0 - 1";
        child_ulong_mul: "ulong" u64, "*", 1 << 32, 1 << 32 => "4294967296 * 4294967296";
        child_ulong_mul_max: "ulong" u64, "*", u64::MAX, 2 => "18446744073709551615 * 2";
    }

    #[test]
    fn narrow_integers_overflow_at_their_own_width() {
        for (test, column, shown) in [
            ("child_sbyte_add", 37, "127 + 1"),
            ("child_byte_add", 37, "255 + 1"),
            ("child_short_mul", 37, "256 * 128"),
            ("child_sbyte_neg", 24, "-(-128)"),
        ] {
            panics(
                test,
                &format!("panic at src/Program.mr:1:{column}: arithmetic overflow: {shown}\n"),
            );
        }
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_sbyte_add() {
        only_in_child(|| {
            call_as::<i8, i8>(INT_ADD, Primitive::SByte, Profile::Debug, 127, 1);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_byte_add() {
        only_in_child(|| {
            call_as::<u8, u8>(INT_ADD, Primitive::Byte, Profile::Debug, 255, 1);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_short_mul() {
        only_in_child(|| {
            let text = operator("int", "*");
            call_as::<i16, i16>(&text, Primitive::Short, Profile::Debug, 256, 128);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_sbyte_neg() {
        only_in_child(|| {
            let program = jit_one_with_in(
                "F(move a: int): int => -a;\n",
                "F",
                Profile::Debug,
                |body| {
                    for local in &mut body.locals {
                        local.ty = LocalTy::Value(Ty::Primitive(Primitive::SByte));
                    }
                },
            );
            func::<extern "C" fn(i8) -> i8>(&program, "F")(i8::MIN);
        });
    }

    #[test]
    fn division_by_zero_panics_in_both_profiles() {
        for test in [
            "child_div_zero_debug",
            "child_div_zero_release",
            "child_rem_zero_debug",
            "child_rem_zero_release",
        ] {
            panics(test, "panic at src/Program.mr:1:37: division by zero\n");
        }
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_div_zero_debug() {
        only_in_child(|| {
            call::<i32, i32>(INT_DIV, Profile::Debug, 1, 0);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_div_zero_release() {
        only_in_child(|| {
            call::<i32, i32>(INT_DIV, Profile::Release, 1, 0);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_rem_zero_debug() {
        only_in_child(|| {
            call::<i32, i32>(INT_REM, Profile::Debug, 1, 0);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_rem_zero_release() {
        only_in_child(|| {
            call::<i32, i32>(INT_REM, Profile::Release, 1, 0);
        });
    }

    #[test]
    fn min_divided_by_minus_one_overflows_in_debug() {
        panics(
            "child_min_div",
            "panic at src/Program.mr:1:37: arithmetic overflow: -2147483648 / -1\n",
        );
        panics(
            "child_min_rem",
            "panic at src/Program.mr:1:37: arithmetic overflow: -2147483648 % -1\n",
        );
        panics(
            "child_long_min_div",
            "panic at src/Program.mr:1:40: arithmetic overflow: -9223372036854775808 / -1\n",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_min_div() {
        only_in_child(|| {
            call::<i32, i32>(INT_DIV, Profile::Debug, i32::MIN, -1);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_min_rem() {
        only_in_child(|| {
            call::<i32, i32>(INT_REM, Profile::Debug, i32::MIN, -1);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_long_min_div() {
        only_in_child(|| {
            call::<i64, i64>(LONG_DIV, Profile::Debug, i64::MIN, -1);
        });
    }

    #[test]
    fn release_division_wraps_min_by_minus_one() {
        assert_eq!(
            call::<i32, i32>(INT_DIV, Profile::Release, i32::MIN, -1),
            i32::MIN
        );
        assert_eq!(call::<i32, i32>(INT_REM, Profile::Release, i32::MIN, -1), 0);
        assert_eq!(
            call::<i64, i64>(LONG_DIV, Profile::Release, i64::MIN, -1),
            i64::MIN
        );
        let args = (Primitive::SByte, Profile::Release);
        assert_eq!(
            call_as::<i8, i8>(INT_DIV, args.0, args.1, i8::MIN, -1),
            i8::MIN
        );
        assert_eq!(call_as::<i8, i8>(INT_REM, args.0, args.1, i8::MIN, -1), 0);
        assert_eq!(call::<i32, i32>(INT_DIV, Profile::Release, 7, -1), -7);
        assert_eq!(call::<i32, i32>(INT_REM, Profile::Release, 7, -1), 0);
    }

    #[test]
    fn division_truncates_toward_zero() {
        for profile in [Profile::Debug, Profile::Release] {
            assert_eq!(call::<i32, i32>(INT_DIV, profile, -7, 2), -3);
            assert_eq!(call::<i32, i32>(INT_REM, profile, -7, 2), -1);
            assert_eq!(call::<i32, i32>(INT_DIV, profile, 7, -2), -3);
            assert_eq!(call::<i32, i32>(INT_REM, profile, 7, -2), 1);
            let div = "F(move a: uint, move b: uint): uint => a / b;\n";
            let rem = "F(move a: uint, move b: uint): uint => a % b;\n";
            assert_eq!(
                call::<u32, u32>(div, profile, 4_000_000_000, 3),
                1_333_333_333
            );
            assert_eq!(call::<u32, u32>(rem, profile, 4_000_000_000, 3), 1);
            assert_eq!(
                call_as::<i8, i8>(INT_REM, Primitive::SByte, profile, -7, 2),
                -1
            );
            assert_eq!(
                call_as::<i16, i16>(INT_DIV, Primitive::Short, profile, -7, 2),
                -3
            );
            assert_eq!(
                call_as::<u8, u8>(INT_DIV, Primitive::Byte, profile, 250, 7),
                35
            );
        }
    }

    #[test]
    fn float_remainder_and_division_by_zero() {
        let rem = "F(move a: double, move b: double): double => a % b;\n";
        assert_eq!(call::<f64, f64>(rem, Profile::Debug, 5.5, 2.0), 1.5);
        let rem = "F(move a: float, move b: float): float => a % b;\n";
        assert_eq!(call::<f32, f32>(rem, Profile::Release, -5.5, 2.0), -1.5);
        let div = "F(move a: double, move b: double): double => a / b;\n";
        assert_eq!(
            call::<f64, f64>(div, Profile::Debug, 1.0, 0.0),
            f64::INFINITY
        );
        let div = "F(move a: float, move b: float): float => a / b;\n";
        assert_eq!(call::<f32, f32>(div, Profile::Debug, 1.0, 4.0), 0.25);
    }

    #[test]
    fn operations_at_the_edge_of_their_range_do_not_panic() {
        let debug = Profile::Debug;
        let run = |ty: &str, op: &str| operator(ty, op);
        let int = |op: &str, a: i32, b: i32| call::<i32, i32>(&run("int", op), debug, a, b);
        assert_eq!(int("+", i32::MAX, 0), i32::MAX);
        assert_eq!(int("+", i32::MIN, 0), i32::MIN);
        assert_eq!(int("+", i32::MAX, i32::MIN), -1);
        assert_eq!(int("+", i32::MIN, i32::MAX), -1);
        assert_eq!(int("+", -1, i32::MIN + 1), i32::MIN);
        assert_eq!(int("-", i32::MIN, 0), i32::MIN);
        assert_eq!(int("-", i32::MAX, i32::MAX), 0);
        assert_eq!(int("-", -1, i32::MAX), i32::MIN);
        assert_eq!(int("-", i32::MIN, i32::MIN), 0);
        assert_eq!(int("-", 0, i32::MAX), -i32::MAX);
        assert_eq!(int("*", i32::MIN, 1), i32::MIN);
        assert_eq!(int("*", 46341, 46340), 2_147_441_940);
        assert_eq!(int("*", -46341, 46340), -2_147_441_940);
        assert_eq!(int("*", 65536, -32768), i32::MIN);
        let uint = |op: &str, a: u32, b: u32| call::<u32, u32>(&run("uint", op), debug, a, b);
        assert_eq!(uint("+", u32::MAX, 0), u32::MAX);
        assert_eq!(uint("+", 1, u32::MAX - 1), u32::MAX);
        assert_eq!(uint("-", u32::MAX, u32::MAX), 0);
        assert_eq!(uint("-", 5, 5), 0);
        assert_eq!(uint("*", 65535, 65537), u32::MAX);
        assert_eq!(uint("*", u32::MAX, 1), u32::MAX);
        let long = |op: &str, a: i64, b: i64| call::<i64, i64>(&run("long", op), debug, a, b);
        assert_eq!(long("+", i64::MAX, 0), i64::MAX);
        assert_eq!(long("+", i64::MAX, i64::MIN), -1);
        assert_eq!(long("+", -1, i64::MIN + 1), i64::MIN);
        assert_eq!(long("-", i64::MIN, 0), i64::MIN);
        assert_eq!(long("-", -1, i64::MAX), i64::MIN);
        assert_eq!(long("-", i64::MAX, i64::MAX), 0);
        assert_eq!(long("*", i64::MIN, 1), i64::MIN);
        assert_eq!(
            long("*", 3_037_000_499, 3_037_000_499),
            9_223_372_030_926_249_001
        );
        assert_eq!(
            long("*", -3_037_000_499, 3_037_000_499),
            -9_223_372_030_926_249_001
        );
        assert_eq!(long("*", 1 << 32, -(1 << 31)), i64::MIN);
        assert_eq!(long("*", i64::MAX, -1), -i64::MAX);
        let ulong = |op: &str, a: u64, b: u64| call::<u64, u64>(&run("ulong", op), debug, a, b);
        assert_eq!(ulong("+", u64::MAX, 0), u64::MAX);
        assert_eq!(ulong("-", u64::MAX, u64::MAX), 0);
        assert_eq!(ulong("*", u64::MAX, 1), u64::MAX);
        assert_eq!(ulong("*", (1 << 32) - 1, (1 << 32) + 1), u64::MAX);
        assert_eq!(ulong("*", 1 << 63, 1), 1 << 63);
        let neg = |ty: &str| format!("F(move a: {ty}): {ty} => -a;\n");
        let neg_int = jit_one_in(&neg("int"), "F", debug);
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&neg_int, "F")(0), 0);
        let neg_long = jit_one_in(&neg("long"), "F", debug);
        assert_eq!(
            func::<extern "C" fn(i64) -> i64>(&neg_long, "F")(i64::MAX),
            -i64::MAX
        );
        assert_eq!(
            func::<extern "C" fn(i64) -> i64>(&neg_long, "F")(i64::MIN + 1),
            i64::MAX
        );
    }

    #[test]
    fn in_range_checked_operations_give_their_results() {
        let long = "F(move a: long, move b: long): long => a - b;\n";
        assert_eq!(
            call::<i64, i64>(long, Profile::Debug, i64::MIN + 1, 1),
            i64::MIN
        );
        let debug = Profile::Debug;
        let mul = "F(move a: int, move b: int): int => a * b;\n";
        assert_eq!(call_as::<u8, u8>(mul, Primitive::Byte, debug, 15, 17), 255);
        assert_eq!(
            call_as::<i16, i16>(mul, Primitive::Short, debug, -128, 256),
            -32768
        );
        assert_eq!(
            call_as::<i8, i8>(INT_ADD, Primitive::SByte, debug, -1, -127),
            -128
        );
        let neg = jit_one_in("F(move a: int): int => -a;\n", "F", Profile::Debug);
        assert_eq!(
            func::<extern "C" fn(i32) -> i32>(&neg, "F")(i32::MAX),
            -i32::MAX
        );
    }

    #[test]
    #[should_panic(expected = "invalid MIR: `/` on a Bool")]
    fn division_needs_numbers() {
        call_as::<bool, bool>(INT_DIV, Primitive::Bool, Profile::Debug, true, true);
    }

    #[test]
    fn every_runtime_function_is_in_the_jit_symbol_table() {
        let symbols = mora_rt::jit_symbols();
        let names: Vec<&str> = symbols.iter().map(|(name, _)| *name).collect();
        let missing: Vec<&str> = RuntimeFn::ALL
            .iter()
            .map(|function| function.name())
            .filter(|name| !names.contains(name))
            .collect();
        assert_eq!(missing, Vec::<&str>::new());
    }

    #[test]
    fn objects_import_the_panic_entry_points() {
        use object::{Object, ObjectSymbol};
        let target = mora_target::Target::builtin("x86_64-linux").unwrap();
        let bytes = object_of(&entry(INT_ADD), &target, Profile::Debug);
        let file = object::File::parse(&*bytes).unwrap();
        let undefined = |wanted: &str| {
            file.symbols()
                .any(|s| s.is_undefined() && s.name() == Ok(wanted))
        };
        assert!(undefined("mora_rt_panic_overflow"));
        let bytes = object_of(&entry(INT_DIV), &target, Profile::Debug);
        let file = object::File::parse(&*bytes).unwrap();
        assert!(
            file.symbols()
                .any(|s| s.is_undefined() && s.name() == Ok("mora_rt_panic"))
        );
    }

    #[test]
    fn the_checks_are_lowered_for_every_target() {
        // Every built-in target compiles the checks.
        let text = "F(move a: int, move b: int): int => a + b - a * b;\n\
                    G(move a: uint, move b: uint): uint => a * b - a / b + a % b;\n\
                    H(move a: long, move b: long): long => a * b - -a + a / b - a % b;\n\
                    K(move a: ulong, move b: ulong): ulong => a - b + a / b + a % b;\n\
                    L(move a: double, move b: double): double => a % b / a;\n\
                    M(move a: float, move b: float): float => a % b / a;\n\
                    F(1, 2);\nG(1, 2);\nH(1, 2);\nK(1, 2);\nL(1.0, 2.0);\nM(1.0, 2.0);\n";
        for name in mora_target::builtin_names() {
            let target = mora_target::Target::builtin(name).unwrap();
            for profile in [Profile::Debug, Profile::Release] {
                assert!(!object_of(text, &target, profile).is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn a_debug_check_branches_to_a_cold_block_and_a_release_one_does_not() {
        let debug = lowered_in(INT_ADD, "F", Profile::Debug, |_| {});
        assert!(debug.contains("cold"), "{debug}");
        assert!(debug.contains("brif"), "{debug}");
        let release = lowered_in(INT_ADD, "F", Profile::Release, |_| {});
        assert!(
            !release.contains("overflow") && !release.contains("cold"),
            "{release}"
        );
        // A divisor of zero is checked in both.
        let release = lowered_in(INT_DIV, "F", Profile::Release, |_| {});
        assert!(release.contains("cold"), "{release}");
        assert!(!release.contains("overflow"), "{release}");
    }
}
