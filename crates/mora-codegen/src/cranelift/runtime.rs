//! What generated code reaches outside its own functions: the runtime's
//! entry points, and the read-only data a panic names its location and
//! message with.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{
    AbiParam, ExternalName, FuncRef, Function, GlobalValue, GlobalValueData, Signature, Type,
    UserExternalName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use mora_span::{Db, LineIndex, SourceFile, Span, line_index};

use super::cranelift_error;
use crate::Profile;
use crate::error::CodegenError;
use crate::program::{Function as ProgramFunction, Program};

/// The message of a division or remainder by zero.
pub(super) const DIVISION_BY_ZERO: &str = "division by zero";

/// A runtime entry point generated code calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeFn {
    /// `mora_rt_panic`: stops with a message at a source location.
    Panic = 0,
    /// `mora_rt_panic_overflow`: stops with an arithmetic overflow.
    PanicOverflow = 1,
    /// `mora_rt_fmod`: `%` on `double`.
    Fmod = 2,
    /// `mora_rt_fmodf`: `%` on `float`.
    Fmodf = 3,
}

impl RuntimeFn {
    /// Every entry point, in the order of the discriminants.
    pub(crate) const ALL: &[RuntimeFn] = &[
        RuntimeFn::Panic,
        RuntimeFn::PanicOverflow,
        RuntimeFn::Fmod,
        RuntimeFn::Fmodf,
    ];

    /// The symbol the runtime defines.
    pub(crate) fn name(self) -> &'static str {
        match self {
            RuntimeFn::Panic => "mora_rt_panic",
            RuntimeFn::PanicOverflow => "mora_rt_panic_overflow",
            RuntimeFn::Fmod => "mora_rt_fmod",
            RuntimeFn::Fmodf => "mora_rt_fmodf",
        }
    }

    /// The Cranelift signature, in the C convention `call_conv` of a target
    /// whose pointers have the type `pointer`. The panics never return.
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
        };
        signature
            .params
            .extend(params.iter().copied().map(AbiParam::new));
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
        let sources: HashSet<SourceFile> = program
            .functions()
            .iter()
            .map(|function| function.key.file)
            .collect();
        for file in sources {
            let path = file.path(program.db());
            let data = FileData {
                path: define_bytes(module, path.as_bytes())?,
                path_len: path.len(),
                lines: line_index(program.db(), file).clone(),
            };
            files.insert(file, data);
        }
        Ok(Runtime {
            functions,
            zero_message,
            files,
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
        calls: Vec<Option<FuncRef>>,
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
fn data_symbol(func: &mut Function, id: DataId, colocated: bool) -> GlobalValue {
    // The namespace of data (cranelift-module).
    let name = func.declare_imported_user_function(UserExternalName::new(1, id.as_u32()));
    func.create_global_value(GlobalValueData::Symbol {
        name: ExternalName::user(name),
        offset: 0.into(),
        colocated,
        tls: false,
    })
}

/// What the lowering of one function reaches.
pub(super) struct Env<'a> {
    /// How the code is generated.
    pub(super) profile: Profile,
    /// The function each block calls, `None` for a block that ends in no
    /// call, or calls a function of the standard library or one outside the
    /// program.
    pub(super) calls: Vec<Option<FuncRef>>,
    /// The runtime entry points, in the order of [`RuntimeFn::ALL`].
    pub(super) functions: Vec<FuncRef>,
    /// The bytes of [`DIVISION_BY_ZERO`].
    pub(super) zero_message: GlobalValue,
    /// The bytes of the file's path.
    pub(super) path: GlobalValue,
    /// The length of the file's path.
    pub(super) path_len: usize,
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
        self.functions[function as usize]
    }

    /// The line and column, counting from 1, where `span` starts.
    pub(super) fn location(&self, span: Span) -> (u32, u32) {
        let start = u32::from(span.range.start()) as usize;
        mora_diag::line_column(self.file.text(self.db), self.lines, start).unwrap_or((0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_discriminants_are_the_positions() {
        for (index, function) in RuntimeFn::ALL.iter().enumerate() {
            assert_eq!(*function as usize, index);
        }
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
        let narrow = RuntimeFn::Panic.signature(types::I32, CallConv::SystemV);
        assert_eq!(narrow.params[0].value_type, types::I32);
    }
}
