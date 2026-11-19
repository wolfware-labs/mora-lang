//! The lowering of strings (§13.4): literals, `a + b`, and `==` and `!=`,
//! also on the string fields of a struct or tuple. A string is 16 bytes in
//! memory, never in a variable, and crosses every call to the runtime by
//! address; its encoding is `mora_rt_core::string`'s alone.

use std::collections::HashSet;
use std::sync::Arc;

use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_module::{DataDescription, DataId, Module};
use mora_mir::{Body, Const, InterpPiece, Operand, Place, Rvalue, StatementKind, TerminatorKind};
use mora_rt_core::string::{IMMORTAL, INLINE_MAX, MoraString, heap_template, object_header};

use super::cranelift_error;
use super::function::{Lowering, Source, broken};
use super::runtime::{RuntimeFn, data_symbol};
use crate::error::CodegenError;
use crate::layout::Shape;
use crate::program::Program;

/// The alignment of a string object in read-only data.
const OBJECT_ALIGN: u64 = mora_rt_core::string::OBJECT_ALIGN as u64;

/// What a walk of a body reports to.
struct Walker<'f> {
    /// Called for every operand the body reads.
    operand: &'f mut dyn FnMut(&Operand),
}

impl Walker<'_> {
    fn operand(&mut self, operand: &Operand) {
        (self.operand)(operand);
    }

    fn operands(&mut self, operands: &[Operand]) {
        for operand in operands {
            self.operand(operand);
        }
    }

    fn rvalue(&mut self, rvalue: &Rvalue) {
        match rvalue {
            Rvalue::Use(value) | Rvalue::Unary(_, value) | Rvalue::Cast(_, value, _) => {
                self.operand(value);
            }
            Rvalue::Binary(_, left, right) => {
                self.operand(left);
                self.operand(right);
            }
            Rvalue::Aggregate(_, values) => self.operands(values),
            Rvalue::Borrow(..) => {}
            Rvalue::Concat(values) => self.operands(values),
            Rvalue::Interpolate(pieces) => {
                for piece in pieces {
                    match piece {
                        // A text is a literal part, which needs its object
                        // when it is long.
                        InterpPiece::Text(text) => {
                            self.operand(&Operand::Const(Const::String(text.as_str().into())));
                        }
                        InterpPiece::Operand {
                            value, alignment, ..
                        } => {
                            self.operand(value);
                            alignment
                                .iter()
                                .for_each(|alignment| self.operand(alignment));
                        }
                    }
                }
            }
        }
    }

    /// Walks `body`. The cases of a switch are constants that are never
    /// strings, so they are not operands. Nothing here has a catch-all arm:
    /// a new kind of statement or terminator must say what it mentions.
    fn body(&mut self, body: &Body) {
        for block in &body.blocks {
            for statement in &block.statements {
                match &statement.kind {
                    StatementKind::Assign(_, rvalue) => self.rvalue(rvalue),
                    StatementKind::StorageLive(_)
                    | StatementKind::StorageDead(_)
                    | StatementKind::Nop => {}
                }
            }
            match &block.terminator.kind {
                TerminatorKind::Switch { discr, .. } => self.operand(discr),
                TerminatorKind::Call { args, .. } => self.operands(args),
                TerminatorKind::Goto(_) | TerminatorKind::Return | TerminatorKind::Unreachable => {}
            }
        }
    }
}

/// The text of every string literal of `program` that does not fit in a
/// string value, each once, in the order of first appearance: the functions
/// in order, and in each the blocks in order.
pub(super) fn long_literals(program: &Program<'_>) -> Vec<Arc<str>> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for function in program.functions() {
        Walker {
            operand: &mut |operand| {
                if let Operand::Const(Const::String(text)) = operand
                    && text.len() > INLINE_MAX
                    && seen.insert(Arc::clone(text))
                {
                    found.push(Arc::clone(text));
                }
            },
        }
        .body(function.body);
    }
    found
}

/// Defines the immortal heap object of the literal `text` as read-only data
/// of `module`: `{ IMMORTAL, 0, len, bytes }`.
///
/// # Errors
///
/// [`CodegenError::Cranelift`] when the module rejects the definition.
pub(super) fn define_literal<M: Module>(
    module: &mut M,
    text: &str,
    big_endian: bool,
) -> Result<DataId, CodegenError> {
    let id = module
        .declare_anonymous_data(false, false)
        .map_err(cranelift_error)?;
    // A literal is shorter than its source file, whose offsets are 32 bits.
    let mut bytes = object_header(IMMORTAL, 0, text.len() as u32, big_endian).to_vec();
    bytes.extend_from_slice(text.as_bytes());
    let mut data = DataDescription::new();
    data.define(bytes.into());
    data.set_align(OBJECT_ALIGN);
    module.define_data(id, &data).map_err(cranelift_error)?;
    Ok(id)
}

/// The integer whose bytes in memory are `bytes`, on a target of the byte
/// order `big_endian` says.
fn word_bits(bytes: &[u8], big_endian: bool) -> i64 {
    let step = |word: u64, byte: u8| (word << 8) | u64::from(byte);
    let bits = if big_endian {
        bytes.iter().copied().fold(0, step)
    } else {
        bytes.iter().rev().copied().fold(0, step)
    };
    bits as i64
}

impl Lowering<'_, '_> {
    /// Whether `operand` reads a string.
    pub(super) fn is_string(&self, operand: &Operand) -> bool {
        self.source_layout(self.source(operand)).shape == Shape::String
    }

    /// What `operand` reads, which is a string.
    ///
    /// # Panics
    ///
    /// With an internal error when it is not.
    fn string_source<'o>(&self, operand: &'o Operand) -> Source<'o> {
        if !self.is_string(operand) {
            broken(
                self.body,
                &format!("`{operand:?}`, which is not a string, used as one"),
            );
        }
        self.source(operand)
    }

    /// The address of the string `operand` reads: its place, or a new slot
    /// that holds a literal.
    pub(super) fn string_address(&mut self, operand: &Operand) -> Value {
        match self.string_source(operand) {
            Source::Place(place) => self.place_address(place),
            source @ Source::Const(_) => {
                let layout = self.source_layout(source);
                let slot = self.temporary(&layout);
                self.store_source(slot, source);
                slot
            }
        }
    }

    /// Writes the string literal `text` at `address`: an inline value as
    /// words of immediates, built in the byte order of the target, and a
    /// longer one as the address of its object, its length and its tag.
    pub(super) fn store_literal(&mut self, address: Value, text: &str) {
        let big_endian = self.env.big_endian;
        let (bytes, object) = match MoraString::inline(text.as_bytes()) {
            Some(inline) => (inline.raw(), None),
            None => {
                let id = *self.env.strings.get(text).unwrap_or_else(|| {
                    broken(
                        self.body,
                        &format!("a string literal `{text}` the scan missed"),
                    )
                });
                let symbol = data_symbol(self.builder.func, id, self.env.colocated);
                let object = self.builder.ins().symbol_value(self.pointer, symbol);
                // A literal is shorter than its source file, whose offsets
                // are 32 bits.
                (heap_template(text.len() as u32, big_endian), Some(object))
            }
        };
        // The object's address is the first word.
        let mut words: Vec<Value> = object.into_iter().collect();
        let width = self.layouts.pointer_bytes() as usize;
        for chunk in bytes.chunks(width).skip(words.len()) {
            let word = self
                .builder
                .ins()
                .iconst(self.pointer, word_bits(chunk, big_endian));
            words.push(word);
        }
        self.store_words(address, &words);
    }

    /// Builds in `place` the concatenation of the strings `operands` read,
    /// with one call that takes them in an array: a whole chain of `+` is
    /// one `Concat` (mora-mir), so it allocates at most once. A heap string
    /// is never released: counting references is M2's.
    ///
    /// # Panics
    ///
    /// With an internal error when `place` or an operand is not a string.
    pub(super) fn concatenate(&mut self, place: &Place, operands: &[Operand]) {
        let layout = self.place_layout(place);
        if layout.shape != Shape::String {
            broken(
                self.body,
                &format!("a concatenation into `{place:?}`, which is not a string"),
            );
        }
        let (parts, offsets) = self.layouts.array(&layout, operands.len() as u32);
        let array = self.temporary(&parts);
        for (operand, offset) in operands.iter().zip(offsets) {
            let source = self.string_source(operand);
            let part = self.offset(array, offset);
            self.store_source(part, source);
        }
        // The parts are copies, so the result may go to a place a part
        // reads.
        let out = self.place_address(place);
        self.join_parts(array, operands.len(), out);
    }

    /// Writes at `out` the concatenation of the `count` strings at `array`.
    pub(super) fn join_parts(&mut self, array: Value, count: usize, out: Value) {
        let count = self.builder.ins().iconst(self.pointer, count as i64);
        let concat = self.env.function(RuntimeFn::StringConcat);
        self.builder.ins().call(concat, &[array, count, out]);
    }

    /// The `bool` of `==` or `!=` on the strings `left` and `right` read.
    pub(super) fn strings_compare(
        &mut self,
        op: mora_mir::BinOp,
        left: &Operand,
        right: &Operand,
    ) -> Value {
        let left = self.string_address(left);
        let right = self.string_address(right);
        let equal = self.strings_equal(left, right);
        self.comparison(op, equal)
    }

    /// Whether the strings at `left` and `right` hold the same text.
    pub(super) fn strings_equal(&mut self, left: Value, right: Value) -> Value {
        let equal = self.env.function(RuntimeFn::StringEq);
        let call = self.builder.ins().call(equal, &[left, right]);
        self.builder.inst_results(call)[0]
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::{Primitive, Ty};
    use mora_mir::{BinOp, Const, LocalId, LocalTy, Operand, Place, Rvalue, StatementKind};
    use mora_rt::string::new_string;
    use mora_rt_core::string::{
        BYTES_OFFSET, CAPACITY_OFFSET, HEAP_TAG, IMMORTAL, INLINE_MAX, LEN_OFFSET, MoraString,
        STRONG_OFFSET, TAG_OFFSET, WEAK_OFFSET,
    };

    use super::super::function::tests::{jit_one_with, lowered_in};
    use super::super::runtime::RuntimeFn;

    use super::*;
    use crate::Profile;
    use crate::test_util::{func, jit, object_bytes, object_of, string_bytes};
    use crate::{JitProgram, Program};

    /// A string in place of the empty one, which a test overwrites.
    fn blank() -> MoraString {
        MoraString::EMPTY
    }

    /// The `u32` at `offset` of the object of the heap string `string`.
    fn object_word(string: &MoraString, offset: usize) -> u32 {
        let bytes = object_bytes(string, offset + 4);
        u32::from_ne_bytes(bytes[offset..].try_into().unwrap())
    }

    /// The text of `string`.
    fn text(string: &MoraString) -> Vec<u8> {
        string_bytes(string)
    }

    /// The value `Set` of `source` writes into its `ref` parameter, and the
    /// program, which holds the object of a long literal.
    fn set(source: &str) -> (MoraString, JitProgram) {
        let program = jit(
            &format!("Set(ref s: string) {{ s = {source}; }}\nvar t = \"\";\nSet(ref t);\n"),
            Profile::Debug,
        );
        let mut value = blank();
        func::<extern "C" fn(&mut MoraString)>(&program, "Set")(&mut value);
        (value, program)
    }

    #[test]
    fn short_literals_are_inline_values() {
        let (s, _s_program) = set("\"Ada\"");
        assert_eq!(s.raw()[..3], *b"Ada");
        assert_eq!(s.raw()[3..TAG_OFFSET], [0; 12]);
        assert_eq!(s.raw()[TAG_OFFSET], 3);
        let (empty, _empty_program) = set("\"\"");
        assert_eq!(empty.raw(), MoraString::EMPTY.raw());
    }

    #[test]
    fn the_inline_limit_of_a_literal_counts_bytes() {
        let (fits, _fits_program) = set("\"123456789012345\"");
        assert!(fits.is_inline());
        assert_eq!(fits.len(), INLINE_MAX);
        assert_eq!(text(&fits), b"123456789012345");
        let (spills, _spills_program) = set("\"1234567890123456\"");
        assert!(!spills.is_inline());
        assert_eq!(text(&spills), b"1234567890123456");
        // Sixteen bytes in eight characters.
        let (accents, _accents_program) = set("\"éééééééé\"");
        assert_eq!(accents.len(), 16);
        assert!(!accents.is_inline());
        assert_eq!(text(&accents), "éééééééé".as_bytes());
    }

    #[test]
    fn long_literals_are_immortal_objects() {
        let literal = "a literal of more than fifteen bytes";
        let (s, _s_program) = set(&format!("\"{literal}\""));
        assert_eq!(s.raw()[TAG_OFFSET], HEAP_TAG);
        assert_eq!(s.len(), 36);
        assert_eq!(s.raw()[12..TAG_OFFSET], [0, 0, 0]);
        assert_eq!(s.raw()[LEN_OFFSET..LEN_OFFSET + 4], 36u32.to_ne_bytes());
        assert_eq!(object_word(&s, STRONG_OFFSET), IMMORTAL);
        assert_eq!(object_word(&s, WEAK_OFFSET), 0);
        assert_eq!(object_word(&s, CAPACITY_OFFSET), 36);
        assert_eq!(
            object_bytes(&s, BYTES_OFFSET + 36)[BYTES_OFFSET..],
            *literal.as_bytes()
        );
    }

    #[test]
    fn a_text_has_one_object_in_a_module() {
        let program = jit(
            "A(ref s: string) { s = \"the same long literal, twice\"; }\n\
             B(ref s: string) { s = \"the same long literal, twice\"; }\n\
             C(ref s: string) { s = \"another long literal, once\"; }\n\
             var t = \"\";\nA(ref t);\nB(ref t);\nC(ref t);\n",
            Profile::Debug,
        );
        let run = |name: &str| {
            let mut value = blank();
            func::<extern "C" fn(&mut MoraString)>(&program, name)(&mut value);
            value
        };
        let (a, b, c) = (run("A"), run("B"), run("C"));
        assert_eq!(a.object(), b.object());
        assert_ne!(a.object(), c.object());
        assert_eq!(text(&c), b"another long literal, once");
    }

    #[test]
    fn strings_copy_and_cross_calls() {
        let program = jit(
            "Copy(s: string, ref t: string) { t = s; }\n\
             Id(move s: string): string => s;\n\
             Through(move s: string, ref t: string) { t = Id(s); }\n\
             var t = \"\";\nCopy(\"\", ref t);\nThrough(\"\", ref t);\n",
            Profile::Debug,
        );
        for text in [&b"Ada"[..], b"a string of more than fifteen bytes"] {
            let from = new_string(text);
            let mut to = blank();
            func::<extern "C" fn(&MoraString, &mut MoraString)>(&program, "Copy")(&from, &mut to);
            assert_eq!(to.raw(), from.raw());
            let mut to = blank();
            // A `move` string goes in two words, which hold its 16 bytes.
            let words: [usize; 2] = [0, 1].map(|word| {
                let bytes = &from.raw()[word * 8..word * 8 + 8];
                usize::from_ne_bytes(bytes.try_into().unwrap())
            });
            func::<extern "C" fn(usize, usize, &mut MoraString)>(&program, "Through")(
                words[0], words[1], &mut to,
            );
            assert_eq!(to.raw(), from.raw());
        }
    }

    #[test]
    fn concatenation_and_equality() {
        let program = jit(
            "Check(name: string): bool => \"Hello, \" + name + \"!\" == \"Hello, Ada!\";\n\
             Differs(name: string): bool => \"Hello, \" + name + \"!\" != \"Hello, Ada!\";\n\
             Long(name: string): bool => \"Hello, \" + name + \"!\" == \"Hello, Ada Lovelace!\";\n\
             Check(\"Ada\");\nDiffers(\"Ada\");\nLong(\"Ada\");\n",
            Profile::Debug,
        );
        let call = |name: &str, argument: &[u8]| {
            let argument = new_string(argument);
            func::<extern "C" fn(&MoraString) -> bool>(&program, name)(&argument)
        };
        assert!(call("Check", b"Ada"));
        assert!(!call("Check", b"Bob"));
        assert!(!call("Differs", b"Ada"));
        assert!(call("Differs", b"Bob"));
        assert!(call("Long", b"Ada Lovelace"));
        assert!(!call("Long", b"Ada Lovelacf"));
        assert!(!call("Long", b"Ada"));
    }

    #[test]
    fn a_concatenation_reads_its_parts_before_writing_its_result() {
        let program = jit(
            "Append(ref s: string) { s = s + \"!\"; }\n\
             Both(ref s: string) { s = s + s; }\n\
             var t = \"\";\nAppend(ref t);\nBoth(ref t);\n",
            Profile::Release,
        );
        for (text, appended, doubled) in [
            (&b"Ada"[..], &b"Ada!"[..], &b"Ada!Ada!"[..]),
            (
                b"fourteen bytes",
                b"fourteen bytes!",
                b"fourteen bytes!fourteen bytes!",
            ),
        ] {
            let mut s = new_string(text);
            func::<extern "C" fn(&mut MoraString)>(&program, "Append")(&mut s);
            assert_eq!(self::text(&s), appended);
            func::<extern "C" fn(&mut MoraString)>(&program, "Both")(&mut s);
            assert_eq!(self::text(&s), doubled);
        }
    }

    /// A pair of a string and an `int`, as the host passes it by pointer.
    #[repr(C)]
    struct Pair {
        text: MoraString,
        number: i32,
    }

    #[test]
    fn aggregates_compare_their_string_fields() {
        let program = jit(
            "Same(move a: (string, int), move b: (string, int)): bool => a == b;\n\
             Other(move a: (string, int), move b: (string, int)): bool => a != b;\n\
             Built(a: string, b: string): bool => (a, 1) == (b, 1);\n\
             Same((\"\", 1), (\"\", 1));\nOther((\"\", 1), (\"\", 1));\nBuilt(\"\", \"\");\n",
            Profile::Debug,
        );
        let pair = |text: &[u8], number| Pair {
            text: new_string(text),
            number,
        };
        let same = func::<extern "C" fn(&Pair, &Pair) -> bool>(&program, "Same");
        let other = func::<extern "C" fn(&Pair, &Pair) -> bool>(&program, "Other");
        let long = b"a string of more than fifteen bytes";
        assert!(same(&pair(b"Ada", 1), &pair(b"Ada", 1)));
        assert!(same(&pair(long, 1), &pair(long, 1)));
        assert!(!same(&pair(b"Ada", 1), &pair(b"Bob", 1)));
        assert!(!same(&pair(long, 1), &pair(b"Ada", 1)));
        assert!(!same(&pair(b"Ada", 1), &pair(b"Ada", 2)));
        assert!(other(&pair(b"Ada", 1), &pair(b"Bob", 1)));
        assert!(!other(&pair(b"Ada", 1), &pair(b"Ada", 1)));
        let built = func::<extern "C" fn(&MoraString, &MoraString) -> bool>(&program, "Built");
        assert!(built(&new_string(long), &new_string(long)));
        assert!(!built(&new_string(long), &new_string(b"Ada")));
    }

    #[test]
    fn objects_put_long_literals_in_read_only_data() {
        use object::{Object, ObjectSection, ObjectSymbol, SectionKind};
        let literal = "a literal of more than fifteen bytes";
        let target = mora_target::Target::builtin("x86_64-linux").unwrap();
        let text = format!(
            "F(a: string): string => a + \"{literal}\";\nG(a: string): bool => a == \"short\";\n\
             F(\"\");\nG(\"\");\n"
        );
        let bytes = object_of(&text, &target, Profile::Debug);
        let file = object::File::parse(&*bytes).unwrap();
        let held = file
            .sections()
            .filter(|section| section.kind() == SectionKind::ReadOnlyData)
            .any(|section| {
                let data = section.data().unwrap();
                data.windows(literal.len()).any(|w| w == literal.as_bytes())
            });
        assert!(held, "no read-only section holds the literal");
        // The short literal is an immediate in the code, not an object.
        let anywhere = file
            .sections()
            .filter(|section| section.kind() == SectionKind::ReadOnlyData)
            .any(|section| section.data().unwrap().windows(5).any(|w| w == b"short"));
        assert!(!anywhere);
        for wanted in ["mora_rt_string_concat", "mora_rt_string_eq"] {
            assert!(
                file.symbols()
                    .any(|s| s.is_undefined() && s.name() == Ok(wanted)),
                "{wanted}"
            );
        }
    }

    #[test]
    fn strings_compile_for_every_target() {
        let text = "Greet(name: string): string => \"Hello, \" + name + \"! A long literal.\";\n\
                    Same(name: string): bool => name == \"Ada\" || name != \"a long literal here\";\n\
                    Greet(\"\");\nSame(\"\");\n";
        for name in mora_target::builtin_names() {
            let target = mora_target::Target::builtin(name).unwrap();
            for profile in [Profile::Debug, Profile::Release] {
                assert!(!object_of(text, &target, profile).is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn words_hold_their_bytes_in_the_byte_order_of_the_target() {
        assert_eq!(word_bits(&[1, 2, 3, 4], false), 0x0403_0201);
        assert_eq!(word_bits(&[1, 2, 3, 4], true), 0x0102_0304);
        assert_eq!(word_bits(&[0xff; 8], false), -1);
        assert_eq!(word_bits(&[0, 0, 0, 0, 0, 0, 0, 0x80], false), i64::MIN);
        assert_eq!(word_bits(&[0x80, 0, 0, 0, 0, 0, 0, 0], true), i64::MIN);
    }

    #[test]
    fn long_literals_are_found_once_in_order_wherever_they_stand() {
        use mora_hir::test_support::TestDb;
        let db = TestDb::default();
        let project = crate::test_util::app_of(
            &db,
            "Take(a: string): int => 1;\n\
             F(a: string): bool => (a, 1) == (\"in an aggregate, longer than 15\", 1);\n\
             G(a: string): int => Take(\"as an argument, longer than 15\");\n\
             H(a: string): bool => a == \"in an aggregate, longer than 15\" || a == \"short\";\n\
             I(ref s: string) { s = \"in an assignment, longer than 15\"; }\n\
             J(ref s: string) { s = \"123456789012345\"; }\n\
             F(\"\");\nG(\"\");\nH(\"\");\nvar t = \"\";\nI(ref t);\nJ(ref t);\n",
        );
        let program = Program::collect(&db, project).unwrap();
        let literals = long_literals(&program);
        let found: Vec<&str> = literals.iter().map(|text| &**text).collect();
        let mut sorted = found.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(found.len(), sorted.len(), "{found:?}");
        assert_eq!(found.len(), 3, "{found:?}");
        // Fifteen bytes fit in a string value: no object.
        assert!(!found.contains(&"123456789012345"), "{found:?}");
        for wanted in [
            "in an aggregate, longer than 15",
            "as an argument, longer than 15",
            "in an assignment, longer than 15",
        ] {
            assert!(found.contains(&wanted), "{wanted}");
        }
    }

    #[test]
    fn the_objects_of_literals_do_not_depend_on_a_hash_table() {
        let target = mora_target::Target::builtin("x86_64-linux").unwrap();
        let text = "A(ref s: string) { s = \"the first long literal\"; }\n\
                    B(ref s: string) { s = \"the second long literal\"; }\n\
                    C(ref s: string) { s = \"the third long literal\"; }\n\
                    var t = \"\";\nA(ref t);\nB(ref t);\nC(ref t);\n";
        let first = object_of(text, &target, Profile::Debug);
        for _ in 0..20 {
            assert!(object_of(text, &target, Profile::Debug) == first);
        }
    }

    /// The text `F`, a function of `text` of three strings and a `ref`
    /// string, writes into the `ref` string.
    fn concatenated3(source: &str, args: [&[u8]; 3]) -> Vec<u8> {
        let program = jit(source, Profile::Debug);
        let [a, b, c] = args.map(new_string);
        let mut out = blank();
        func::<extern "C" fn(&MoraString, &MoraString, &MoraString, &mut MoraString)>(
            &program, "F",
        )(&a, &b, &c, &mut out);
        text(&out)
    }

    /// The same for four strings.
    fn concatenated4(source: &str, args: [&[u8]; 4]) -> Vec<u8> {
        let program = jit(source, Profile::Debug);
        let [a, b, c, d] = args.map(new_string);
        let mut out = blank();
        func::<extern "C" fn(&MoraString, &MoraString, &MoraString, &MoraString, &mut MoraString)>(
            &program, "F",
        )(&a, &b, &c, &d, &mut out);
        text(&out)
    }

    #[test]
    fn a_chain_gives_the_text_of_its_parts_in_order() {
        let three = "F(a: string, b: string, c: string, ref r: string) { r = a + b + c; }\n\
                     var t = \"\";\nF(\"\", \"\", \"\", ref t);\n";
        let long = b"fifteen bytes!!";
        assert_eq!(concatenated3(three, [b"ab", b"cd", b"ef"]), b"abcdef");
        assert_eq!(
            concatenated3(three, [long, b"x", long]),
            [&long[..], b"x", &long[..]].concat()
        );
        let four = "F(a: string, b: string, c: string, d: string, ref r: string) \
                    { r = a + b + c + d; }\nvar t = \"\";\nF(\"\", \"\", \"\", \"\", ref t);\n";
        assert_eq!(concatenated4(four, [b"1", b"2", b"3", b"4"]), b"1234");
        let right = "F(a: string, b: string, c: string, ref r: string) { r = a + (b + c); }\n\
                     var t = \"\";\nF(\"\", \"\", \"\", ref t);\n";
        assert_eq!(concatenated3(right, [b"a", b"b", b"c"]), b"abc");
    }

    #[test]
    fn a_chain_may_read_the_place_it_writes() {
        let program = jit(
            "G(ref s: string, t: string) { s = s + t + s; }\n\
             var s = \"\";\nG(ref s, \"\");\n",
            Profile::Debug,
        );
        let g = func::<extern "C" fn(&mut MoraString, &MoraString)>(&program, "G");
        for (start, middle, expected) in [
            (&b"ab"[..], &b"-"[..], &b"ab-ab"[..]),
            (b"twelve bytes", b"-and-", b"twelve bytes-and-twelve bytes"),
        ] {
            let mut s = new_string(start);
            g(&mut s, &new_string(middle));
            assert_eq!(text(&s), expected);
        }
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: invalid MIR: a string literal `a literal of more than fifteen bytes` the scan missed"
    )]
    fn a_literal_the_scan_missed_is_an_internal_error() {
        // The helper's module has no object for any literal.
        lowered_in(
            "F(ref s: string) { s = \"a literal of more than fifteen bytes\"; }\n",
            "F",
            Profile::Debug,
            |_| {},
        );
    }

    #[test]
    fn a_walk_reads_the_holes_of_an_interpolation() {
        use mora_hir::test_support::TestDb;
        let db = TestDb::default();
        let project = crate::test_util::app_of(
            &db,
            "F(a: int): int { let s = $`x{a,5}y{\"a hole of more than 15 bytes\"}`; return 1; }\nF(1);\n",
        );
        let program = Program::collect(&db, project).unwrap();
        let found = long_literals(&program);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(&*found[0], "a hole of more than 15 bytes");
    }

    /// The function the lowering of `F` makes, as text.
    fn ir(text: &str) -> String {
        lowered_in(text, "F", Profile::Debug, |_| {})
    }

    /// How many calls of `mora_rt_string_concat` the lowering of `text`
    /// makes: the calls of the import the runtime list numbers.
    fn concat_calls(text: &str) -> usize {
        let clif = ir(text);
        let name = format!("u0:{}", 1 + RuntimeFn::StringConcat.index());
        let imports: Vec<&str> = clif
            .lines()
            .filter(|line| line.contains(&name))
            .filter_map(|line| line.trim().split(' ').next())
            .collect();
        assert_eq!(imports.len(), 1, "{clif}");
        clif.matches(&format!("call {}(", imports[0])).count()
    }

    #[test]
    fn a_chain_of_additions_is_one_call() {
        let strings = |names: &str| {
            let params: Vec<String> = names.split(' ').map(|n| format!("{n}: string")).collect();
            params.join(", ")
        };
        let sources = [
            format!("F({}, ref r: string) {{ r = a + b; }}\n", strings("a b")),
            format!(
                "F({}, ref r: string) {{ r = a + b + c; }}\n",
                strings("a b c")
            ),
            format!(
                "F({}, ref r: string) {{ r = a + b + c + d; }}\n",
                strings("a b c d")
            ),
            format!(
                "F({}, ref r: string) {{ r = (a + b) + (c + d); }}\n",
                strings("a b c d")
            ),
            format!(
                "F({}, ref r: string) {{ r = a + (b + c); }}\n",
                strings("a b c")
            ),
            "F(a: string, ref r: string) { r = \"Hello, \" + a + \"!\"; }\n".to_owned(),
            format!(
                "Id(s: string): string => s;\nF({}, ref r: string) {{ r = a + b + Id(c); }}\n",
                strings("a b c")
            ),
            format!(
                "F({}, k: bool, ref r: string) {{ r = a + b + (k ? x : y); }}\n",
                strings("a b x y")
            ),
        ];
        for source in &sources {
            assert_eq!(concat_calls(source), 1, "{source}");
        }
    }

    #[test]
    fn a_chain_with_a_call_or_a_branch_gives_the_text_of_its_parts_in_order() {
        let program = jit(
            "Id(s: string): string => s;\n\
             Call(a: string, b: string, c: string, ref r: string) { r = a + b + Id(c) + a; }\n\
             Both(a: string, b: string, c: string, d: string, ref r: string) \
             { r = (a + b) + (c + d); }\n\
             Pick(a: string, b: string, x: string, y: string, k: bool, ref r: string) \
             { r = a + b + (k ? x : y); }\n\
             var t = \"\";\nCall(\"\", \"\", \"\", ref t);\nBoth(\"\", \"\", \"\", \"\", ref t);\n\
             Pick(\"\", \"\", \"\", \"\", true, ref t);\n",
            Profile::Debug,
        );
        let [a, b, c, d] = [&b"fifteen bytes!!"[..], b"-", b"cd", b"and some more"].map(new_string);
        let mut out = blank();
        func::<extern "C" fn(&MoraString, &MoraString, &MoraString, &mut MoraString)>(
            &program, "Call",
        )(&a, &b, &c, &mut out);
        assert_eq!(text(&out), b"fifteen bytes!!-cdfifteen bytes!!");
        func::<extern "C" fn(&MoraString, &MoraString, &MoraString, &MoraString, &mut MoraString)>(
            &program, "Both",
        )(&a, &b, &c, &d, &mut out);
        assert_eq!(text(&out), b"fifteen bytes!!-cdand some more");
        let pick = func::<
            extern "C" fn(
                &MoraString,
                &MoraString,
                &MoraString,
                &MoraString,
                bool,
                &mut MoraString,
            ),
        >(&program, "Pick");
        pick(&a, &b, &c, &d, true, &mut out);
        assert_eq!(text(&out), b"fifteen bytes!!-cd");
        pick(&a, &b, &c, &d, false, &mut out);
        assert_eq!(text(&out), b"fifteen bytes!!-and some more");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a `+` of strings that MIR has not turned")]
    fn an_addition_of_strings_is_an_internal_error() {
        first_assignment(
            "F(a: string, b: string, ref r: string) { r = a + b; }\n",
            |rvalue| {
                if let Rvalue::Concat(operands) = rvalue {
                    *rvalue = Rvalue::Binary(BinOp::Add, operands[0].clone(), operands[1].clone());
                }
            },
        );
    }

    #[test]
    fn one_call_builds_a_concatenation_of_two_parts() {
        let text = ir("F(a: string, b: string, ref c: string) { c = a + b; }\n");
        assert_eq!(text.matches("call fn").count(), 1, "{text}");
    }

    /// The function `F` of `text` after `edit` changes the value its first
    /// assignment computes.
    fn first_assignment(text: &str, edit: impl FnOnce(&mut Rvalue)) {
        jit_one_with(text, "F", |body| {
            if let StatementKind::Assign(_, rvalue) = &mut body.blocks[0].statements[0].kind {
                edit(rvalue);
            }
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a string constant in a scalar")]
    fn a_string_constant_in_a_scalar_is_an_internal_error() {
        first_assignment("F(): int { return 7; }\n", |rvalue| {
            *rvalue = Rvalue::Use(Operand::Const(Const::String("a".into())));
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a value of shape String read as a scalar")]
    fn a_string_read_as_a_scalar_is_an_internal_error() {
        jit_one_with("F(move s: string): int { return 7; }\n", "F", |body| {
            if let StatementKind::Assign(_, rvalue) = &mut body.blocks[0].statements[0].kind {
                *rvalue = Rvalue::Use(Operand::Copy(Place::local(LocalId(1))));
            }
        });
    }

    #[test]
    #[should_panic(expected = "which is not a string, used as one")]
    fn a_string_compared_with_a_scalar_is_an_internal_error() {
        first_assignment("F(a: string, b: string): bool => a == b;\n", |rvalue| {
            if let Rvalue::Binary(_, _, right) = rvalue {
                *right = Operand::Const(Const::Int(1, Primitive::Int));
            }
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: invalid MIR: a concatenation into `")]
    fn a_concatenation_outside_a_string_is_an_internal_error() {
        jit_one_with("F(a: string, b: string): string => a + b;\n", "F", |body| {
            body.locals[0].ty = LocalTy::Value(Ty::Primitive(Primitive::Int));
        });
    }
}
