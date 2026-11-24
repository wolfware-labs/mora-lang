//! What generated code reaches outside its own functions: the runtime's
//! entry points, and the read-only data a panic names its location and
//! message with. It includes the calls to the temporary output and formatting
//! stand-ins, which M1.15 (#43) removes.

use std::collections::HashMap;
use std::sync::Arc;

use cranelift_codegen::ir::Endianness;
use cranelift_codegen::ir::{
    AbiParam, ExternalName, FuncRef, Function, GlobalValue, GlobalValueData, Signature, Type,
    UserExternalName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use mora_span::{Db, LineIndex, SourceFile, Span, line_index};

use super::cranelift_error;
use super::strings::{define_literal, long_literals};
use crate::Profile;
use crate::error::CodegenError;
use crate::layout::Layout;
use crate::program::{Function as ProgramFunction, Program};

/// The message of a division or remainder by zero.
pub(super) const DIVISION_BY_ZERO: &str = "division by zero";

/// A runtime entry point generated code calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeFn {
    /// `mora_rt_panic`: stops with a message at a source location.
    Panic,
    /// `mora_rt_panic_overflow`: stops with an arithmetic overflow.
    PanicOverflow,
    /// `mora_rt_fmod`: `%` on `double`.
    Fmod,
    /// `mora_rt_fmodf`: `%` on `float`.
    Fmodf,
    /// `mora_rt_string_concat`: `a + b` on `string`.
    StringConcat,
    /// `mora_rt_string_eq`: `==` on `string`.
    StringEq,
    /// `__mora_standin_write_line_*`: `Console.WriteLine` of a value of the
    /// kind. A stand-in M1.15 (#43) removes.
    WriteLine(StandinKind),
    /// `__mora_standin_format_*`: the text of a value of the kind, for a
    /// hole of an interpolated string. A stand-in M1.15 (#43) removes; there
    /// is none for a string, which is a part as it is.
    Format(StandinKind),
}

/// The type of the value a stand-in takes. M1.15 (#43) removes it with the
/// stand-ins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StandinKind {
    /// A signed integer, extended to 64 bits.
    I64,
    /// An unsigned integer, extended to 64 bits.
    U64,
    /// A `float`.
    F32,
    /// A `double`.
    F64,
    /// A `bool`, extended to 32 bits.
    Bool,
    /// A `char`.
    Char,
    /// A `string`, by address.
    String,
}

impl StandinKind {
    /// The type the stand-in takes the value as, on a target whose pointers
    /// have the type `pointer`: a string crosses by address.
    fn param(self, pointer: Type) -> Type {
        match self {
            StandinKind::I64 | StandinKind::U64 => types::I64,
            StandinKind::F32 => types::F32,
            StandinKind::F64 => types::F64,
            StandinKind::Bool | StandinKind::Char => types::I32,
            StandinKind::String => pointer,
        }
    }
}

impl RuntimeFn {
    /// Every entry point, in the order of [`RuntimeFn::index`]: the fixed
    /// ones, the seven `WriteLine`s and the six `Format`s.
    pub(crate) const ALL: &[RuntimeFn] = &[
        RuntimeFn::Panic,
        RuntimeFn::PanicOverflow,
        RuntimeFn::Fmod,
        RuntimeFn::Fmodf,
        RuntimeFn::StringConcat,
        RuntimeFn::StringEq,
        RuntimeFn::WriteLine(StandinKind::I64),
        RuntimeFn::WriteLine(StandinKind::U64),
        RuntimeFn::WriteLine(StandinKind::F32),
        RuntimeFn::WriteLine(StandinKind::F64),
        RuntimeFn::WriteLine(StandinKind::Bool),
        RuntimeFn::WriteLine(StandinKind::Char),
        RuntimeFn::WriteLine(StandinKind::String),
        RuntimeFn::Format(StandinKind::I64),
        RuntimeFn::Format(StandinKind::U64),
        RuntimeFn::Format(StandinKind::F32),
        RuntimeFn::Format(StandinKind::F64),
        RuntimeFn::Format(StandinKind::Bool),
        RuntimeFn::Format(StandinKind::Char),
    ];

    /// The position of the entry point in [`RuntimeFn::ALL`].
    ///
    /// # Panics
    ///
    /// For `Format(String)`, which is no entry point: a string is a part of
    /// an interpolation as it is.
    pub(crate) fn index(self) -> usize {
        let known = Self::ALL.iter().position(|known| *known == self);
        known.unwrap_or_else(|| panic!("`{self:?}` is not a runtime entry point"))
    }

    /// The symbol the runtime defines.
    pub(crate) fn name(self) -> &'static str {
        match self {
            RuntimeFn::Panic => "mora_rt_panic",
            RuntimeFn::PanicOverflow => "mora_rt_panic_overflow",
            RuntimeFn::Fmod => "mora_rt_fmod",
            RuntimeFn::Fmodf => "mora_rt_fmodf",
            RuntimeFn::StringConcat => "mora_rt_string_concat",
            RuntimeFn::StringEq => "mora_rt_string_eq",
            RuntimeFn::WriteLine(StandinKind::I64) => "__mora_standin_write_line_i64",
            RuntimeFn::WriteLine(StandinKind::U64) => "__mora_standin_write_line_u64",
            RuntimeFn::WriteLine(StandinKind::F32) => "__mora_standin_write_line_f32",
            RuntimeFn::WriteLine(StandinKind::F64) => "__mora_standin_write_line_f64",
            RuntimeFn::WriteLine(StandinKind::Bool) => "__mora_standin_write_line_bool",
            RuntimeFn::WriteLine(StandinKind::Char) => "__mora_standin_write_line_char",
            RuntimeFn::WriteLine(StandinKind::String) => "__mora_standin_write_line_string",
            RuntimeFn::Format(StandinKind::I64) => "__mora_standin_format_i64",
            RuntimeFn::Format(StandinKind::U64) => "__mora_standin_format_u64",
            RuntimeFn::Format(StandinKind::F32) => "__mora_standin_format_f32",
            RuntimeFn::Format(StandinKind::F64) => "__mora_standin_format_f64",
            RuntimeFn::Format(StandinKind::Bool) => "__mora_standin_format_bool",
            RuntimeFn::Format(StandinKind::Char) => "__mora_standin_format_char",
            RuntimeFn::Format(StandinKind::String) => {
                panic!("a string is a part of an interpolation as it is")
            }
        }
    }

    /// The Cranelift signature, in the C convention `call_conv` of a target
    /// whose pointers have the type `pointer`. The panics never return. A
    /// 32-bit parameter is sign-extended, as riscv64's convention expects of
    /// every 32-bit value, signed or not, and the others ignore.
    pub(crate) fn signature(self, pointer: Type, call_conv: CallConv) -> Signature {
        let mut signature = Signature::new(call_conv);
        let (params, returns): (&[Type], &[Type]) = match self {
            // The file, its length, the line and column, the message and
            // its length.
            RuntimeFn::Panic => (
                &[pointer, pointer, types::I32, types::I32, pointer, pointer],
                &[],
            ),
            // The file, its length, the line and column, the operation,
            // whether it is signed, and the two operands.
            RuntimeFn::PanicOverflow => (
                &[
                    pointer,
                    pointer,
                    types::I32,
                    types::I32,
                    types::I32,
                    types::I32,
                    types::I64,
                    types::I64,
                ],
                &[],
            ),
            RuntimeFn::Fmod => (&[types::F64, types::F64], &[types::F64]),
            RuntimeFn::Fmodf => (&[types::F32, types::F32], &[types::F32]),
            // The address of the parts, how many there are, and the address
            // to write the result to. Strings always cross by address, so
            // the convention of a target for a value of 16 bytes does not
            // matter.
            RuntimeFn::StringConcat => (&[pointer, pointer, pointer], &[]),
            // The addresses of the two strings, and whether they are equal.
            RuntimeFn::StringEq => (&[pointer, pointer], &[types::I8]),
            // The value, which is extended as `StandinKind` says.
            RuntimeFn::WriteLine(kind) => (&[kind.param(pointer)], &[]),
            // The value, and the address to write the text to.
            RuntimeFn::Format(kind) => (&[kind.param(pointer), pointer], &[]),
        };
        signature.params.extend(params.iter().map(|&ty| match ty {
            types::I32 => AbiParam::new(ty).sext(),
            ty => AbiParam::new(ty),
        }));
        signature
            .returns
            .extend(returns.iter().copied().map(AbiParam::new));
        signature
    }
}

/// A source file's path in read-only data, and its line starts.
struct FileData {
    path: DataId,
    path_len: usize,
    lines: LineIndex,
}

/// What a module holds for the runtime: the imports, and the data of every
/// file of the program.
pub(super) struct Runtime {
    functions: Vec<FuncId>,
    zero_message: DataId,
    files: HashMap<SourceFile, FileData>,
    // The object of each string literal longer than an inline string holds.
    strings: HashMap<Arc<str>, DataId>,
    big_endian: bool,
}

impl Runtime {
    /// Declares the imports and defines the data `program` needs in `module`.
    ///
    /// # Errors
    ///
    /// [`CodegenError::Cranelift`] when the module rejects a declaration.
    pub(super) fn define<M: Module>(
        module: &mut M,
        program: &Program<'_>,
    ) -> Result<Runtime, CodegenError> {
        let pointer = module.isa().pointer_type();
        let call_conv = module.isa().default_call_conv();
        let functions = RuntimeFn::ALL
            .iter()
            .map(|function| {
                module
                    .declare_function(
                        function.name(),
                        Linkage::Import,
                        &function.signature(pointer, call_conv),
                    )
                    .map_err(cranelift_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let zero_message = define_bytes(module, DIVISION_BY_ZERO.as_bytes())?;
        let mut files = HashMap::new();
        // In the order of the functions, so that the object does not depend
        // on the order a hash table iterates in.
        for function in program.functions() {
            let file = function.key.file;
            if files.contains_key(&file) {
                continue;
            }
            let path = file.path(program.db());
            let data = FileData {
                path: define_bytes(module, path.as_bytes())?,
                path_len: path.len(),
                lines: line_index(program.db(), file).clone(),
            };
            files.insert(file, data);
        }
        let big_endian = module.isa().endianness() == Endianness::Big;
        let mut strings = HashMap::new();
        // In the order the literals first appear in, as the files are.
        for text in long_literals(program) {
            let id = define_literal(module, &text, big_endian)?;
            strings.insert(text, id);
        }
        Ok(Runtime {
            functions,
            zero_message,
            files,
            strings,
            big_endian,
        })
    }

    /// What the lowering of `function`, which `func` holds, reaches. `calls`
    /// is the function each block of it calls.
    ///
    /// # Panics
    ///
    /// When `function` is not one of the functions of the program given to
    /// [`Runtime::define`].
    pub(super) fn env<'a, M: Module>(
        &'a self,
        module: &mut M,
        program: &Program<'a>,
        function: &ProgramFunction<'_>,
        func: &mut Function,
        profile: Profile,
        calls: Vec<Option<Call>>,
    ) -> Env<'a> {
        let file = &self.files[&function.key.file];
        let pic = module.isa().flags().is_pic();
        Env {
            profile,
            calls,
            functions: self
                .functions
                .iter()
                .map(|id| {
                    let reference = module.declare_func_in_func(*id, func);
                    // See `data_symbol` for what `colocated` asks of the linker.
                    func.dfg.ext_funcs[reference].colocated = pic;
                    reference
                })
                .collect(),
            zero_message: data_symbol(func, self.zero_message, pic),
            path: data_symbol(func, file.path, pic),
            path_len: file.path_len,
            strings: &self.strings,
            colocated: pic,
            big_endian: self.big_endian,
            file: function.key.file,
            lines: &file.lines,
            db: program.db(),
        }
    }
}

/// Defines `bytes` as anonymous read-only data of `module`.
fn define_bytes<M: Module>(module: &mut M, bytes: &[u8]) -> Result<DataId, CodegenError> {
    let id = module
        .declare_anonymous_data(false, false)
        .map_err(cranelift_error)?;
    let mut data = DataDescription::new();
    data.define(bytes.into());
    module.define_data(id, &data).map_err(cranelift_error)?;
    Ok(id)
}

/// The address of the data `id`, as a global value of `func`.
///
/// Position-independent code, which objects for ELF and Mach-O hold, reaches
/// it as a colocated symbol: the linker resolves the reference, through a
/// stub when the target is far. Other code, which is the JIT's and COFF's
/// (the linker applies base relocations to an absolute address there), asks
/// for an absolute address: Cranelift's position-independent sequences on Arm
/// use relocations that its object writer has for ELF and Mach-O only. The
/// same holds for a call of the runtime, `colocated` on its import.
pub(super) fn data_symbol(func: &mut Function, id: DataId, colocated: bool) -> GlobalValue {
    // The namespace of data (cranelift-module).
    let name = func.declare_imported_user_function(UserExternalName::new(1, id.as_u32()));
    func.create_global_value(GlobalValueData::Symbol {
        name: ExternalName::user(name),
        offset: 0.into(),
        colocated,
        tls: false,
    })
}

/// A function a call terminator reaches.
pub(super) struct Call {
    /// The function, declared in the caller.
    pub(super) func: FuncRef,
    /// The layout of its result, which says how the result comes back even
    /// when the call drops it.
    pub(super) result: Layout,
}

/// What the lowering of one function reaches.
pub(super) struct Env<'a> {
    /// How the code is generated.
    pub(super) profile: Profile,
    /// The function each block calls, `None` for a block that ends in no
    /// call, or calls a function of the standard library or one outside the
    /// program.
    pub(super) calls: Vec<Option<Call>>,
    /// The runtime entry points, in the order of [`RuntimeFn::ALL`].
    pub(super) functions: Vec<FuncRef>,
    /// The bytes of [`DIVISION_BY_ZERO`].
    pub(super) zero_message: GlobalValue,
    /// The bytes of the file's path.
    pub(super) path: GlobalValue,
    /// The length of the file's path.
    pub(super) path_len: usize,
    /// The object of each string literal that does not fit in a string
    /// value, by its text.
    pub(super) strings: &'a HashMap<Arc<str>, DataId>,
    /// Whether the data symbols of the module are colocated (see
    /// [`data_symbol`]).
    pub(super) colocated: bool,
    /// Whether the target stores a multi-byte integer with its most
    /// significant byte first.
    pub(super) big_endian: bool,
    /// The file the function is in.
    pub(super) file: SourceFile,
    /// The line starts of the file.
    pub(super) lines: &'a LineIndex,
    /// The database of the file.
    pub(super) db: &'a dyn Db,
}

impl Env<'_> {
    /// The import of `function`.
    pub(super) fn function(&self, function: RuntimeFn) -> FuncRef {
        self.functions[function.index()]
    }

    /// The line and column, counting from 1, where `span` starts; `None`
    /// when it starts outside the file.
    pub(super) fn location(&self, span: Span) -> Option<(u32, u32)> {
        let start = usize::from(span.range.start());
        mora_diag::line_column(self.file.text(self.db), self.lines, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_of_each_file_is_defined_in_function_order() {
        use cranelift_jit::JITModule;
        use mora_hir::test_support::{TestDb, app};
        use mora_target::Target;

        use crate::cranelift::{isa, jit_builder};
        let text = [
            (
                "src/Program.mr",
                "Main(): int { return F() + G(); }\nG(): int => 2;\n",
            ),
            ("src/Lib.mr", "F(): int => 1;\n"),
            ("src/Other.mr", "H(): int => 3;\n"),
        ];
        // A hash table iterates in another order each time it is made.
        for _ in 0..20 {
            let db = TestDb::default();
            let project = app(&db, &text);
            let program = Program::collect(&db, project).unwrap();
            let target = Target::host().unwrap();
            let mut module =
                JITModule::new(jit_builder(isa(&target, Profile::Debug, false).unwrap()));
            let runtime = Runtime::define(&mut module, &program).unwrap();
            let mut seen: Vec<SourceFile> = Vec::new();
            for function in program.functions() {
                if !seen.contains(&function.key.file) {
                    seen.push(function.key.file);
                }
            }
            let ids: Vec<u32> = seen
                .iter()
                .map(|file| runtime.files[file].path.as_u32())
                .collect();
            // After the runtime's message, one object per file, in order.
            // Nothing calls `H`, so `Other.mr` has no function in the
            // program and no object.
            let first = runtime.zero_message.as_u32() + 1;
            assert_eq!(seen.len(), 2);
            assert!(
                program
                    .functions()
                    .iter()
                    .all(|function| function.key.file.path(&db) != "src/Other.mr")
            );
            assert_eq!(ids, [first, first + 1]);
        }
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
    fn the_index_is_the_position() {
        for (index, function) in RuntimeFn::ALL.iter().enumerate() {
            assert_eq!(function.index(), index);
        }
    }

    #[test]
    #[should_panic(expected = "is not a runtime entry point")]
    fn a_string_has_no_formatting_entry_point() {
        RuntimeFn::Format(StandinKind::String).index();
    }

    #[test]
    #[should_panic(expected = "a part of an interpolation as it is")]
    fn a_string_has_no_formatting_name() {
        RuntimeFn::Format(StandinKind::String).name();
    }

    #[test]
    fn the_stand_ins_are_named_for_their_kind() {
        let names: Vec<&str> = RuntimeFn::ALL[6..].iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "__mora_standin_write_line_i64",
                "__mora_standin_write_line_u64",
                "__mora_standin_write_line_f32",
                "__mora_standin_write_line_f64",
                "__mora_standin_write_line_bool",
                "__mora_standin_write_line_char",
                "__mora_standin_write_line_string",
                "__mora_standin_format_i64",
                "__mora_standin_format_u64",
                "__mora_standin_format_f32",
                "__mora_standin_format_f64",
                "__mora_standin_format_bool",
                "__mora_standin_format_char",
            ]
        );
    }

    #[test]
    fn signatures_follow_the_runtime() {
        let signature = |function: RuntimeFn| {
            let signature = function.signature(types::I64, CallConv::SystemV);
            let types = |params: &[AbiParam]| -> Vec<Type> {
                params.iter().map(|p| p.value_type).collect()
            };
            (types(&signature.params), types(&signature.returns))
        };
        let p = types::I64;
        let n = types::I32;
        assert_eq!(
            signature(RuntimeFn::Panic),
            (vec![p, p, n, n, p, p], vec![])
        );
        assert_eq!(
            signature(RuntimeFn::PanicOverflow),
            (vec![p, p, n, n, n, n, p, p], vec![])
        );
        assert_eq!(
            signature(RuntimeFn::Fmod),
            (vec![types::F64; 2], vec![types::F64])
        );
        assert_eq!(
            signature(RuntimeFn::Fmodf),
            (vec![types::F32; 2], vec![types::F32])
        );
        assert_eq!(signature(RuntimeFn::StringConcat), (vec![p, p, p], vec![]));
        assert_eq!(
            signature(RuntimeFn::StringEq),
            (vec![p, p], vec![types::I8])
        );
        use StandinKind::*;
        let (f32, f64) = (types::F32, types::F64);
        for (kind, value) in [
            (I64, p),
            (U64, p),
            (F32, f32),
            (F64, f64),
            (Bool, n),
            (Char, n),
        ] {
            assert_eq!(signature(RuntimeFn::WriteLine(kind)), (vec![value], vec![]));
            assert_eq!(signature(RuntimeFn::Format(kind)), (vec![value, p], vec![]));
        }
        // Every 32-bit parameter is sign-extended, and nothing else is.
        use cranelift_codegen::ir::ArgumentExtension;
        for function in [
            RuntimeFn::Panic,
            RuntimeFn::PanicOverflow,
            RuntimeFn::WriteLine(Bool),
            RuntimeFn::Format(Char),
            RuntimeFn::WriteLine(I64),
        ] {
            for param in function.signature(p, CallConv::SystemV).params {
                let expected = if param.value_type == n {
                    ArgumentExtension::Sext
                } else {
                    ArgumentExtension::None
                };
                assert_eq!(param.extension, expected, "{function:?}");
            }
        }
        // A string crosses by address, which is as wide as a pointer.
        let narrow = RuntimeFn::WriteLine(String).signature(types::I32, CallConv::SystemV);
        assert_eq!(narrow.params[0].value_type, types::I32);
        let narrow = RuntimeFn::Panic.signature(types::I32, CallConv::SystemV);
        assert_eq!(narrow.params[0].value_type, types::I32);
    }
}
