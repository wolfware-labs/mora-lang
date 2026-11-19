//! The lowering of the temporary stand-ins for `Console.WriteLine` and for
//! interpolated strings. M1.15 (#43) removes this module with the runtime's
//! `standin`: the standard library brings the real output and formatting,
//! and with it the formatting into a stack buffer (§13.4) that allocates once
//! for a whole interpolated string. The stand-in formats each part that is
//! longer than 15 bytes into an allocation of its own, then joins the parts
//! with one more; M1.15 brings the stack-buffer formatting.

use cranelift_codegen::ir::{InstBuilder, Value, types};
use mora_mir::{InterpPiece, Operand, Place};

use super::function::{Lowering, broken, fail};
use super::runtime::{RuntimeFn, StandinKind};
use crate::layout::{Scalar, Shape};

impl Lowering<'_, '_> {
    /// Lowers the call of the standard library function `path` with `args`:
    /// `Console.WriteLine` of one primitive or string is a call of the
    /// runtime's stand-in. M1.15 (#43) removes this stand-in.
    ///
    /// # Panics
    ///
    /// With an internal error for any other callee, argument count or
    /// argument type, which M1.15 brings.
    pub(super) fn standin_call(&mut self, path: &[String], args: &[Operand]) {
        let operand = match args {
            [operand] if path == ["Console", "WriteLine"] => operand,
            _ => self.not_a_standin(path),
        };
        let Some((kind, value)) = self.standin_operand(operand) else {
            self.not_a_standin(path)
        };
        let write_line = self.env.function(RuntimeFn::WriteLine(kind));
        self.builder.ins().call(write_line, &[value]);
    }

    /// Panics: the call of `path`, a function of the standard library, is
    /// not one a stand-in lowers.
    fn not_a_standin(&self, path: &[String]) -> ! {
        fail(
            self.body,
            &format!(
                "a call of `{}`, a standard library function,",
                path.join(".")
            ),
            "M1.15",
        )
    }

    /// The kind of the value `operand` reads, and the value as the stand-ins
    /// take it: an integer extended to 64 bits by its signedness, a `bool`
    /// extended to 32 bits, and a string as its address. `None` for any other
    /// type.
    fn standin_operand(&mut self, operand: &Operand) -> Option<(StandinKind, Value)> {
        let source = self.source(operand);
        let (kind, extend) = match self.source_layout(source).shape {
            Shape::String => return Some((StandinKind::String, self.string_address(operand))),
            Shape::Scalar(Scalar::Int { signed: true, .. }) => (StandinKind::I64, types::I64),
            Shape::Scalar(Scalar::Int { signed: false, .. }) => (StandinKind::U64, types::I64),
            Shape::Scalar(Scalar::Float) => (StandinKind::F32, types::F32),
            Shape::Scalar(Scalar::Double) => (StandinKind::F64, types::F64),
            Shape::Scalar(Scalar::Bool) => (StandinKind::Bool, types::I32),
            Shape::Scalar(Scalar::Char) => (StandinKind::Char, types::I32),
            Shape::Scalar(Scalar::Pointer) | Shape::Aggregate(_) => return None,
        };
        let (scalar, value) = self.scalar(operand);
        let signed = matches!(scalar, Scalar::Int { signed: true, .. });
        Some((kind, self.resize(value, extend, signed)))
    }

    /// Builds in `place` the string an interpolation describes: each text is
    /// a literal part, a string hole is a part as it is, and any other hole
    /// is formatted into a part. One call joins the parts. M1.15 (#43)
    /// removes this stand-in.
    ///
    /// # Panics
    ///
    /// With an internal error when `place` is not a string, for a hole with
    /// an alignment or a format, and for a hole of a type that is not a
    /// primitive or a string, which M1.15 brings.
    pub(super) fn interpolate(&mut self, place: &Place, pieces: &[InterpPiece]) {
        let layout = self.place_layout(place);
        if layout.shape != Shape::String {
            broken(
                self.body,
                &format!("an interpolation into `{place:?}`, which is not a string"),
            );
        }
        let (parts, offsets) = self.layouts.array(&layout, pieces.len() as u32);
        let array = self.temporary(&parts);
        for (piece, offset) in pieces.iter().zip(offsets) {
            let part = self.offset(array, offset);
            match piece {
                InterpPiece::Text(text) => self.store_literal(part, text),
                InterpPiece::Operand {
                    value,
                    alignment,
                    format,
                } => {
                    if alignment.is_some() || format.is_some() {
                        fail(
                            self.body,
                            "an alignment or a format in an interpolated string",
                            "M1.15",
                        );
                    }
                    self.interpolated_part(part, value);
                }
            }
        }
        // The parts are copies, so the result may go to a place a part
        // reads.
        let out = self.place_address(place);
        self.join_parts(array, pieces.len(), out);
    }

    /// Writes at `part` the text of the hole that reads `value`: a copy of a
    /// string, or the text a stand-in formats.
    fn interpolated_part(&mut self, part: Value, value: &Operand) {
        let source = self.source(value);
        if self.source_layout(source).shape == Shape::String {
            self.store_source(part, source);
            return;
        }
        match self.standin_operand(value) {
            Some((kind, value)) => {
                let format = self.env.function(RuntimeFn::Format(kind));
                self.builder.ins().call(format, &[value, part]);
            }
            None => fail(
                self.body,
                "a hole of a type that is not a primitive or a string",
                "M1.15",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use mora_rt::string::new_string;
    use mora_rt_core::string::MoraString;

    use mora_hir::{Primitive, Ty};
    use mora_mir::LocalTy;

    use super::super::function::tests::jit_one_with;
    use crate::Profile;
    use crate::test_util::{child, func, jit, only_in_child, string_bytes};

    /// What `F` writes to its `ref` string when it is `text`.
    fn built(text: &str) -> Vec<u8> {
        let program = jit(
            &format!("F(ref s: string) {{ s = {text}; }}\nvar t = \"\";\nF(ref t);\n"),
            Profile::Debug,
        );
        let mut out = MoraString::EMPTY;
        func::<extern "C" fn(&mut MoraString)>(&program, "F")(&mut out);
        string_bytes(&out)
    }

    #[test]
    fn interpolation_formats_every_primitive() {
        let program = jit(
            "Show(move n: int, move f: float, move b: bool, move c: char, s: string): bool => \
             $\"{n} {f} {b} {c} {s}!\" == \"-7 0.1 true é Ada!\";\n\
             Show(0, 0.0, false, 'a', \"\");\n",
            Profile::Debug,
        );
        let show =
            func::<extern "C" fn(i32, f32, bool, u32, &MoraString) -> bool>(&program, "Show");
        assert!(show(-7, 0.1, true, 'é' as u32, &new_string(b"Ada")));
        assert!(!show(-8, 0.1, true, 'é' as u32, &new_string(b"Ada")));
    }

    /// The text `F`, which formats its one `ty` parameter, makes of `bits`:
    /// the minimum of a signed type and the maximum of an unsigned one.
    macro_rules! shown {
        ($ty:literal, $rust:ty, $value:expr) => {{
            let text = format!(
                "F(move n: {}, ref s: string) {{ s = $\"{{n}}\"; }}\nvar t = \"\";\nF(0, ref t);\n",
                $ty
            );
            let program = jit(&text, Profile::Debug);
            let mut out = MoraString::EMPTY;
            func::<extern "C" fn($rust, &mut MoraString)>(&program, "F")($value, &mut out);
            String::from_utf8(string_bytes(&out)).unwrap()
        }};
    }

    #[test]
    fn every_integer_width_extends_by_its_signedness() {
        assert_eq!(shown!("sbyte", i8, i8::MIN), "-128");
        assert_eq!(shown!("short", i16, i16::MIN), "-32768");
        assert_eq!(shown!("int", i32, i32::MIN), "-2147483648");
        assert_eq!(shown!("nint", isize, isize::MIN), isize::MIN.to_string());
        assert_eq!(shown!("long", i64, i64::MIN), i64::MIN.to_string());
        assert_eq!(shown!("byte", u8, u8::MAX), "255");
        assert_eq!(shown!("ushort", u16, u16::MAX), "65535");
        assert_eq!(shown!("uint", u32, u32::MAX), "4294967295");
        assert_eq!(shown!("nuint", usize, usize::MAX), usize::MAX.to_string());
        assert_eq!(shown!("ulong", u64, u64::MAX), u64::MAX.to_string());
    }

    #[test]
    fn a_double_formats_in_its_shortest_form() {
        assert_eq!(
            built("$\"{2.5}|{1.0}|{0.1 + 0.2}\""),
            b"2.5|1|0.30000000000000004"
        );
    }

    #[test]
    fn interpolation_joins_text_and_long_literals() {
        assert_eq!(built("$\"\""), b"");
        assert_eq!(built("$\"plain\""), b"plain");
        let long = "a literal text of more than fifteen bytes";
        assert_eq!(
            built(&format!("$\"{long}, {{1 + 1}}\"")),
            format!("{long}, 2").as_bytes()
        );
        let name = "a string of more than fifteen bytes";
        // A string hole is a part as it is.
        let program = jit(
            "F(n: string, ref s: string) { s = $\"<{n}>\"; }\nvar t = \"\";\nF(\"\", ref t);\n",
            Profile::Debug,
        );
        let mut out = MoraString::EMPTY;
        let n = new_string(name.as_bytes());
        func::<extern "C" fn(&MoraString, &mut MoraString)>(&program, "F")(&n, &mut out);
        assert_eq!(string_bytes(&out), format!("<{name}>").as_bytes());
    }

    #[test]
    fn write_line_prints_through_the_stand_ins() {
        let out = child("cranelift::standin::tests::child_write_line");
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("1\n2.5\nHello, world!\n"), "{stdout}");
        let out = child("cranelift::standin::tests::child_write_line_every_type");
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines = "-7\n300\n18446744073709551615\n4294967295\n0.1\ntrue\nfalse\né\nAda!\n";
        assert!(stdout.contains(lines), "{stdout}");
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_write_line() {
        only_in_child(|| {
            let program = jit(
                "Console.WriteLine(1);\nConsole.WriteLine(2.5);\nConsole.WriteLine(\"Hello, world!\");\n",
                Profile::Debug,
            );
            assert_eq!(program.run_main(), 0);
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_write_line_every_type() {
        only_in_child(|| {
            let program = jit(
                "A(move v: sbyte) { Console.WriteLine(v); }\n\
                 B(move v: short) { Console.WriteLine(v); }\n\
                 C(move v: ulong) { Console.WriteLine(v); }\n\
                 D(move v: nuint) { Console.WriteLine(v); }\n\
                 E(move v: float) { Console.WriteLine(v); }\n\
                 G(move v: bool) { Console.WriteLine(v); }\n\
                 H(move v: char) { Console.WriteLine(v); }\n\
                 A(-7);\nB(300);\nC(18446744073709551615);\nD(4294967295);\nE(0.1);\n\
                 G(true);\nG(false);\nH('é');\nConsole.WriteLine($\"Ada{\"!\"}\");\n",
                Profile::Debug,
            );
            assert_eq!(program.run_main(), 0);
        });
    }

    #[test]
    #[should_panic(expected = "M1.15")]
    fn a_format_specifier_waits_for_the_standard_library() {
        jit("let n = 1; let s = $\"{n:X2}\";\n", Profile::Debug);
    }

    #[test]
    #[should_panic(expected = "an alignment or a format in an interpolated string")]
    fn an_alignment_waits_for_the_standard_library() {
        jit("let n = 1; let s = $\"{n,5}\";\n", Profile::Debug);
    }

    #[test]
    #[should_panic(expected = "M1.15")]
    fn write_line_of_a_struct_waits_for_the_standard_library() {
        jit(
            "struct P {\n    public X: int;\n}\nlet p = new P { X = 1 };\nConsole.WriteLine(p);\n",
            Profile::Debug,
        );
    }

    #[test]
    #[should_panic(
        expected = "a call of `Console.Write`, a standard library function, is not lowered \
                    until M1.15"
    )]
    fn other_callees_wait_for_the_standard_library() {
        jit("Console.Write(1);\n", Profile::Debug);
    }

    #[test]
    #[should_panic(
        expected = "a call of `Console.WriteLine`, a standard library function, is not \
                    lowered until M1.15"
    )]
    fn write_line_of_two_arguments_waits_for_the_standard_library() {
        jit("Console.WriteLine(1, 2);\n", Profile::Debug);
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: an interpolation into `")]
    fn an_interpolation_outside_a_string_is_an_internal_error() {
        jit_one_with("F(): string => $\"a{1}\";\n", "F", |body| {
            body.locals[0].ty = LocalTy::Value(Ty::Primitive(Primitive::Int));
        });
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: a hole of a type that is not a primitive or a string is not \
                    lowered until M1.15"
    )]
    fn a_hole_of_a_struct_waits_for_the_standard_library() {
        let text = "struct P {\n    public X: int;\n}\n\
                    F(move p: P): string => $\"{p}\";\n";
        jit_one_with(text, "F", |_| {});
    }

    #[test]
    fn a_child_body_does_not_run_in_the_parent() {
        only_in_child(|| panic!("ran outside a child process"));
    }
}
