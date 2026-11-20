//! The lowering of values in memory: structs and tuples, their copies, and
//! the words a value of up to two words crosses a call in.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};
use mora_mir::{Const, Operand, Place};

use super::function::{Lowering, Source, broken, stack_slot};
use crate::layout::{Layout, Shape};

/// The flags of an access to memory that holds a value passed in words: it
/// cannot trap.
const WORDS: MemFlagsData = MemFlagsData::new().with_notrap();

/// Whether a value of `layout` is kept in memory and copied as bytes: a
/// string, or an aggregate that is not zero-sized.
pub(super) fn is_memory_value(layout: &Layout) -> bool {
    layout.size > 0 && matches!(layout.shape, Shape::Aggregate(_) | Shape::String)
}

impl Lowering<'_, '_> {
    /// The address of a new slot that holds a value of `layout`, a whole
    /// number of words in size and aligned to a word so that a load of words
    /// stays inside it and is aligned.
    pub(super) fn temporary(&mut self, layout: &Layout) -> Value {
        let size = self.layouts.word_size(layout);
        let align = self.layouts.word_align(layout);
        let slot = stack_slot(&mut self.builder, size, align);
        self.builder.ins().stack_addr(self.pointer, slot, 0)
    }

    /// Loads `count` words from `address`.
    pub(super) fn load_words(&mut self, address: Value, count: u8) -> Vec<Value> {
        (0..u32::from(count))
            .map(|word| {
                let offset = (word * self.layouts.pointer_bytes()) as i32;
                self.builder
                    .ins()
                    .load(self.pointer, WORDS, address, offset)
            })
            .collect()
    }

    /// Stores `words` at `address`.
    pub(super) fn store_words(&mut self, address: Value, words: &[Value]) {
        for (word, &value) in (0..).zip(words) {
            let offset = (word * self.layouts.pointer_bytes()) as i32;
            self.builder.ins().store(WORDS, value, address, offset);
        }
    }

    /// Copies a value of `layout` from `from` to `to`, which do not overlap.
    fn copy(&mut self, to: Value, from: Value, layout: &Layout) {
        let align = layout.align as u8;
        self.builder.emit_small_memory_copy(
            self.config,
            to,
            from,
            u64::from(layout.size),
            align,
            align,
            true,
            MemFlagsData::trusted(),
        );
    }

    /// Copies the value of `layout` at `from`, in memory no place holds,
    /// to `dest`.
    pub(super) fn copy_to_place(&mut self, dest: &Place, from: Value, layout: &Layout) {
        let to = self.place_address(dest);
        self.copy(to, from, layout);
    }

    /// Stores the value `source` reads at `address`, in memory no place
    /// holds: a scalar is stored, an aggregate or a string is copied, and a
    /// string literal is written.
    pub(super) fn store_source(&mut self, address: Value, source: Source<'_>) {
        let layout = self.source_layout(source);
        match source {
            Source::Place(place) if is_memory_value(&layout) => {
                let from = self.place_address(place);
                self.copy(address, from, &layout);
            }
            Source::Const(Const::String(text)) => self.store_literal(address, text),
            _ => {
                if let Some(value) = self.source_value(source) {
                    self.builder
                        .ins()
                        .store(MemFlagsData::trusted(), value, address, 0);
                }
            }
        }
    }

    /// Assigns the aggregate `source` reads to `place`. It is copied into a
    /// new slot first, so that a `source` that overlaps `place` is read
    /// before `place` is written.
    pub(super) fn assign_aggregate(&mut self, place: &Place, source: Source<'_>, layout: &Layout) {
        let copy = self.temporary(layout);
        self.store_source(copy, source);
        self.copy_to_place(place, copy, layout);
    }

    /// Builds the aggregate of `operands` in a new slot, and then assigns it
    /// to `place`, so that an operand that reads `place` is read before
    /// `place` is written.
    ///
    /// # Panics
    ///
    /// With an internal error when `place` does not hold an aggregate of as
    /// many fields as `operands`.
    pub(super) fn build_aggregate(&mut self, place: &Place, operands: &[Operand]) {
        let layout = self.place_layout(place);
        let Shape::Aggregate(offsets) = &layout.shape else {
            broken(self.body, &format!("an aggregate built in `{place:?}`"));
        };
        if offsets.len() != operands.len() {
            broken(
                self.body,
                &format!(
                    "an aggregate of {} fields built from {} operands",
                    offsets.len(),
                    operands.len()
                ),
            );
        }
        // The fields of a zero-sized aggregate are zero-sized too.
        if layout.size == 0 {
            return;
        }
        let built = self.temporary(&layout);
        for (operand, &offset) in operands.iter().zip(offsets) {
            let address = self.offset(built, offset);
            let source = self.source(operand);
            self.store_source(address, source);
        }
        self.copy_to_place(place, built, &layout);
    }
}

#[cfg(test)]
mod tests {
    use mora_mir::{LocalId, Operand, Place, Projection, Rvalue, StatementKind, TerminatorKind};

    use super::super::function::tests::{jit_one, jit_one_with, lowered_in};
    use crate::Profile;
    use crate::jit::JitProgram;
    use crate::test_util::{func, jit};

    /// The app of `text`, compiled by the JIT for a debug build. Its last
    /// line calls the functions the test runs.
    fn app(text: &str) -> JitProgram {
        jit(text, Profile::Debug)
    }

    /// The bytes of a value of 16 bytes and 8-byte alignment.
    #[repr(C, align(8))]
    struct Bytes([u8; 16]);

    /// A struct of three `long`s, as the host passes it.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Big {
        a: i64,
        b: i64,
        c: i64,
    }

    #[test]
    fn fields_live_at_their_layout_offsets() {
        let text = "struct S {\n    public A: byte;\n    public B: long;\n    public C: int;\n}\n\
                    GetC(s: S): int => s.C;\nGetA(s: S): byte => s.A;\n";
        // The fields go by decreasing alignment: B at 0, C at 8, A at 12.
        let mut bytes = Bytes([0; 16]);
        bytes.0[8..12].copy_from_slice(&7i32.to_ne_bytes());
        bytes.0[12] = 3;
        // The struct is plain and two words long, so `in` passes its words.
        let (low, high) = bytes.0.split_at(8);
        let words = [low, high].map(|word| u64::from_ne_bytes(word.try_into().unwrap()));
        let get_c = jit_one(text, "GetC");
        assert_eq!(
            func::<extern "C" fn(u64, u64) -> i32>(&get_c, "GetC")(words[0], words[1]),
            7
        );
        let get_a = jit_one(text, "GetA");
        assert_eq!(
            func::<extern "C" fn(u64, u64) -> u8>(&get_a, "GetA")(words[0], words[1]),
            3
        );
    }

    #[test]
    fn two_word_results_and_parameters_round_trip() {
        let program = app(
            "struct P {\n    public X: int;\n    public Y: int;\n    public Z: int;\n}\n\
             Make(a: int): P => new P { X = a, Y = a + 1, Z = a + 2 };\n\
             Take(move p: P): int => p.X + p.Y + p.Z;\n\
             Run(a: int): int => Take(Make(a));\nRun(1);\n",
        );
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "Run")(1), 6);
    }

    #[test]
    fn large_results_and_parameters_go_through_pointers() {
        let program = app(
            "struct Big {\n    public A: long;\n    public B: long;\n    public C: long;\n}\n\
             MakeBig(x: long): Big => new Big { A = x, B = x + 1, C = x + 2 };\n\
             TakeBig(move b: Big): long => b.A + b.B + b.C;\n\
             Run(x: long): long => TakeBig(MakeBig(x));\nRun(10);\n",
        );
        assert_eq!(func::<extern "C" fn(i64) -> i64>(&program, "Run")(10), 33);
        // The host reads the result as a struct returned by value, and
        // passes the address of a copy the callee owns.
        let make = func::<extern "C" fn(i64) -> Big>(&program, "MakeBig");
        let big = make(5);
        assert_eq!(big, Big { a: 5, b: 6, c: 7 });
        let take = func::<extern "C" fn(&Big) -> i64>(&program, "TakeBig");
        assert_eq!(take(&big), 18);
    }

    #[test]
    fn named_tuples_lay_out_like_structs() {
        // Reading an element is M3.12's, so the host reads the two words
        // the tuple comes back in and compares them with a struct's.
        let program = app("struct S {\n    public Id: int;\n    public Ok: bool;\n}\n\
             Pair(a: int, b: bool): (Id: int, Ok: bool) => (a, b);\n\
             Record(a: int, b: bool): S => new S { Id = a, Ok = b };\n\
             Pair(1, true);\nRecord(1, true);\n");
        // An `int` at 0, a `bool` at 4, and padding after them.
        let mut expected = [0u8; 5];
        expected[..4].copy_from_slice(&5i32.to_ne_bytes());
        expected[4] = 1;
        let pair = func::<extern "C" fn(i32, bool) -> u64>(&program, "Pair");
        let record = func::<extern "C" fn(i32, bool) -> u64>(&program, "Record");
        assert_eq!(pair(5, true).to_ne_bytes()[..5], expected);
        assert_eq!(record(5, true).to_ne_bytes()[..5], expected);
    }

    #[test]
    fn assignments_read_operands_before_writing_the_destination() {
        let program = app("struct P {\n    public X: int;\n    public Y: int;\n}\n\
             Flip(p: P): P => new P { X = p.Y, Y = p.X };\n\
             Swap(x: int, y: int): int {\n    var p = new P { X = x, Y = y };\n\
             \x20   p = new P { X = p.Y, Y = p.X };\n    p = Flip(p);\n    p = Flip(p);\n\
             \x20   return p.X * 10 + p.Y;\n}\nSwap(1, 2);\n");
        let swap = func::<extern "C" fn(i32, i32) -> i32>(&program, "Swap");
        assert_eq!(swap(1, 2), 21);
    }

    #[test]
    fn field_initializers_run_for_fields_creation_leaves_out() {
        let program = app(
            "struct P {\n    public X: int = 4;\n    public Y: int;\n}\n\
             GetX(y: int): int { let p = new P { Y = y }; return p.X + p.Y; }\nGetX(1);\n",
        );
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "GetX")(1), 5);
    }

    #[test]
    fn methods_take_this_by_borrow() {
        let program = app(
            "public struct Point {\n    public X: int;\n    public Y: int;\n\n\
             \x20   public LengthSquared(): int => X * X + Y * Y;\n\n\
             \x20   public Scaled(factor: int): Point => \
             new Point { X = X * factor, Y = Y * factor };\n}\n\
             Length(x: int, y: int): int { let p = new Point { X = x, Y = y }; \
             return p.LengthSquared(); }\n\
             Scaled(x: int, y: int): int { let p = new Point { X = x, Y = y }; \
             let q = p.Scaled(2); return q.X * 100 + q.Y; }\n\
             Length(3, 4);\nScaled(3, 4);\n",
        );
        let length = func::<extern "C" fn(i32, i32) -> i32>(&program, "Length");
        assert_eq!(length(3, 4), 25);
        let scaled = func::<extern "C" fn(i32, i32) -> i32>(&program, "Scaled");
        assert_eq!(scaled(3, 4), 608);
    }

    #[test]
    fn aggregates_are_copied_and_passed_whole() {
        let program = app("struct P {\n    public X: int;\n    public Y: int;\n}\n\
             struct Line {\n    public From: P;\n    public To: P;\n    public Tag: byte;\n}\n\
             Sum(move p: P): int => p.X + p.Y;\n\
             Copy(x: int, y: int): int {\n    var a = new P { X = x, Y = y };\n\
             \x20   var b = a;\n    a = new P { X = 0, Y = 0 };\n    return b.X * 10 + b.Y;\n}\n\
             Param(p: P): int {\n    let q = p;\n    return q.X * 10 + q.Y;\n}\n\
             Inner(x: int): int {\n    let l = new Line { From = new P { X = x, Y = 2 }, \
             To = new P { X = 3, Y = x }, Tag = 9 };\n    let t = l.To;\n\
             \x20   return Sum(l.From) * 100 + Sum(t) * 10 + l.To.Y;\n}\n\
             Copy(1, 2);\nParam(new P { X = 1, Y = 2 });\nInner(4);\n");
        let ints = func::<extern "C" fn(i32, i32) -> i32>(&program, "Copy");
        assert_eq!(ints(1, 2), 12);
        let mut point = [0u8; 8];
        point[..4].copy_from_slice(&5i32.to_ne_bytes());
        point[4..].copy_from_slice(&6i32.to_ne_bytes());
        let param = func::<extern "C" fn(u64) -> i32>(&program, "Param");
        assert_eq!(param(u64::from_ne_bytes(point)), 56);
        // From = (4, 2) sums to 6, To = (3, 4) to 7.
        let inner = func::<extern "C" fn(i32) -> i32>(&program, "Inner");
        assert_eq!(inner(4), 6 * 100 + 7 * 10 + 4);
    }

    #[test]
    fn values_of_every_size_cross_calls() {
        let program = app("struct One {\n    public A: byte;\n}\n\
             struct Pair {\n    public A: double;\n    public B: double;\n}\n\
             struct Wide {\n    public A: long;\n    public B: long;\n    public C: long;\n\
             \x20   public D: long;\n    public E: long;\n    public F: long;\n}\n\
             MakeOne(a: byte): One => new One { A = a };\n\
             MakePair(a: double): Pair => new Pair { A = a, B = a * 2.0 };\n\
             MakeWide(a: long): Wide => new Wide { A = a, B = a, C = a, D = a, E = a, F = a + 1 };\n\
             TakeOne(move o: One): int => o.A;\n\
             TakePair(move p: Pair): double => p.A + p.B;\n\
             TakeWide(move w: Wide): long => w.A + w.F;\n\
             Drop(a: long): long {\n    MakeWide(a);\n    MakePair(1.0);\n    return a;\n}\n\
             Run(): double {\n    let o = TakeOne(MakeOne(3));\n\
             \x20   return TakePair(MakePair(1.5)) + o;\n}\n\
             RunWide(a: long): long => TakeWide(MakeWide(a));\n\
             Drop(1);\nRun();\nRunWide(2);\n");
        assert_eq!(func::<extern "C" fn(i64) -> i64>(&program, "Drop")(7), 7);
        // 1.5 + 3.0, and 3 + 4.
        assert_eq!(func::<extern "C" fn() -> f64>(&program, "Run")(), 7.5);
        assert_eq!(func::<extern "C" fn(i64) -> i64>(&program, "RunWide")(3), 7);
    }

    #[test]
    fn a_borrowed_field_is_a_pointer_into_the_value() {
        let program = app("struct P {\n    public X: int;\n    public Y: int;\n}\n\
             Bump(ref x: int) { x = x + 1; }\n\
             Run(a: int): int {\n    var p = new P { X = a, Y = 10 };\n    Bump(ref p.Y);\n\
             \x20   Bump(ref p.Y);\n    return p.X * 100 + p.Y;\n}\n\
             Run(1);\n");
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "Run")(3), 312);
    }

    #[test]
    fn zero_sized_values_are_passed_and_stored_nowhere() {
        let program = app("Nothing() {}\nTake(move u: (), a: int): int => a;\n\
             Run(a: int): int {\n    let u = Nothing();\n    let t = (u, a);\n    return Take(u, a);\n}\n\
             Run(1);\n");
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "Run")(8), 8);
    }

    #[test]
    fn a_large_value_is_copied_by_the_runtime() {
        // Five words are more than the frontend copies in line.
        let program = app(
            "struct Wide {\n    public A: long;\n    public B: long;\n    public C: long;\n\
             \x20   public D: long;\n    public E: long;\n}\n\
             Sum(a: long): long {\n    let w = new Wide { A = a, B = 2, C = 3, D = 4, E = 5 };\n\
             \x20   var v = w;\n    return v.A + v.B + v.C + v.D + v.E;\n}\nSum(1);\n",
        );
        assert_eq!(func::<extern "C" fn(i64) -> i64>(&program, "Sum")(1), 15);
    }

    #[test]
    fn a_parameter_passed_by_pointer_has_no_slot_of_its_own() {
        let text = lowered_in(
            "struct Big {\n    public A: long;\n    public B: long;\n    public C: long;\n}\n\
             F(move b: Big): long => b.B;\n",
            "F",
            Profile::Debug,
            |_| {},
        );
        assert!(!text.contains("explicit_slot"), "{text}");
        // The result of a large value comes through an extra parameter.
        let text = lowered_in(
            "struct Big {\n    public A: long;\n    public B: long;\n    public C: long;\n}\n\
             F(a: long): Big => new Big { A = a, B = a, C = a };\n",
            "F",
            Profile::Debug,
            |_| {},
        );
        assert!(text.contains("sret"), "{text}");
    }

    /// The first assignment of the body.
    fn first_assignment(body: &mut mora_mir::Body) -> (&mut Place, &mut Rvalue) {
        body.blocks
            .iter_mut()
            .flat_map(|block| &mut block.statements)
            .find_map(|statement| match &mut statement.kind {
                StatementKind::Assign(place, rvalue) => Some((place, rvalue)),
                _ => None,
            })
            .expect("the body assigns something")
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: invalid MIR: an aggregate built in `Place { local: \
                               LocalId(0), projections: [] }`"
    )]
    fn an_aggregate_built_in_a_scalar_is_an_internal_error() {
        jit_one_with("F(): int { let t = (1, 2); return 1; }\n", "F", |body| {
            let (place, _) = first_assignment(body);
            *place = Place::local(LocalId::RETURN);
        });
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: invalid MIR: an aggregate of 2 fields built from 1 \
                               operands"
    )]
    fn an_aggregate_with_too_few_operands_is_an_internal_error() {
        jit_one_with("F(): int { let t = (1, 2); return 1; }\n", "F", |body| {
            if let (_, Rvalue::Aggregate(_, operands)) = first_assignment(body) {
                operands.pop();
            }
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a field that `Place { local: \
                               LocalId(0), projections: [Field(0)] }` does not have")]
    fn a_field_of_a_scalar_is_an_internal_error() {
        jit_one_with("F(): int { return 7; }\n", "F", |body| {
            let (place, _) = first_assignment(body);
            place.projections.push(Projection::Field(0));
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a field that `Place { local: \
                               LocalId(1), projections: [Field(0)] }` does not have")]
    fn a_field_of_a_borrow_is_an_internal_error() {
        jit_one_with("F(a: int): int => a;\n", "F", |body| {
            if let (_, Rvalue::Use(operand)) = first_assignment(body) {
                *operand = Operand::Copy(Place {
                    local: LocalId(1),
                    projections: vec![Projection::Field(0)],
                });
            }
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a field that `Place { local: \
                               LocalId(1), projections: [Field(3)] }` does not have")]
    fn a_field_past_the_end_is_an_internal_error() {
        jit_one_with("F(): int { let t = (1, 2); return 1; }\n", "F", |body| {
            let (place, _) = first_assignment(body);
            *place = Place {
                local: LocalId(1),
                projections: vec![Projection::Field(3)],
            };
        });
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: invalid MIR: a dereference inside `Place { local: \
                               LocalId(1), projections: [Field(0), Deref] }`"
    )]
    fn a_dereference_after_a_field_is_an_internal_error() {
        jit_one_with("F(): int { let t = (1, 2); return 1; }\n", "F", |body| {
            let (place, _) = first_assignment(body);
            *place = Place {
                local: LocalId(1),
                projections: vec![Projection::Field(0), Projection::Deref],
            };
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a local with no slot")]
    fn a_result_stored_in_a_zero_sized_local_is_an_internal_error() {
        // The call of `F` by itself returns a pair of `int`s in two words;
        // its result goes to `u`, which holds nothing.
        lowered_in(
            "F(a: int): (int, int) {\n    let u = ();\n    return F(a);\n}\n",
            "F",
            Profile::Debug,
            |body| {
                let unit = body
                    .locals
                    .iter()
                    .position(|local| local.source.as_ref().is_some_and(|s| s.name == "u"))
                    .expect("the local `u`");
                let unit = LocalId(unit as u32);
                for block in &mut body.blocks {
                    if let TerminatorKind::Call { dest, .. } = &mut block.terminator.kind {
                        *dest = Some(Place::local(unit));
                    }
                }
            },
        );
    }
}
