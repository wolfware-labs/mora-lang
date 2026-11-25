//! The Cranelift backend (§14.13): object files for every built-in target,
//! and the JIT on the host.

mod aggregates;
mod checks;
mod function;
mod ops;
mod runtime;
mod standin;
mod strings;

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName, types};
use cranelift_codegen::isa::{self, CallConv as IsaCallConv, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, ModuleError, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use mora_hir::{Primitive, Ty};
use mora_mir::{Body, LocalId, TerminatorKind};
use mora_target::{CallConv, Endian, ObjectFormat, Target};
use target_lexicon::{
    Aarch64Architecture, Architecture, BinaryFormat, Endianness, OperatingSystem,
    Riscv64Architecture, Triple,
};

use self::runtime::{Call, Runtime};
use crate::abi::FnAbi;
use crate::error::CodegenError;
use crate::jit::JitProgram;
use crate::layout::Layouts;
use crate::program::{FunctionId, Program};
use crate::{Backend, JitBackend, Profile};

/// The symbol of the function the runtime's `main` calls (§13.8).
pub(crate) const MAIN: &str = "__mora_main";

/// The Cranelift code generator.
pub struct Cranelift;

impl Backend for Cranelift {
    fn object(
        &self,
        program: &Program<'_>,
        target: &Target,
        profile: Profile,
    ) -> Result<Vec<u8>, CodegenError> {
        // Cranelift's object writer lacks the COFF relocations of its
        // position-independent sequences on aarch64 (see `data_symbol`), so
        // a COFF object holds absolute addresses, which the linker's base
        // relocations adjust.
        let isa = isa(target, profile, target.object_format != ObjectFormat::Coff)?;
        let builder =
            ObjectBuilder::new(isa, "mora", default_libcall_names()).map_err(cranelift_error)?;
        let mut module = ObjectModule::new(builder);
        let layouts = Layouts::new(program.db(), target);
        define_program(&mut module, program, &layouts, profile)?;
        module.finish().emit().map_err(cranelift_error)
    }
}

impl JitBackend for Cranelift {
    fn jit(&self, program: &Program<'_>, profile: Profile) -> Result<JitProgram, CodegenError> {
        let target = Target::host().map_err(unsupported_host)?;
        let isa = isa(&target, profile, false)?;
        let mut jit = JitProgram::new(JITModule::new(jit_builder(isa)));
        let layouts = Layouts::new(program.db(), &target);
        define_program(jit.module_mut(), program, &layouts, profile)?;
        jit.module_mut()
            .finalize_definitions()
            .map_err(cranelift_error)?;
        Ok(jit)
    }
}

/// A JIT that resolves the runtime's entry points to the ones linked into this
/// process.
fn jit_builder(isa: OwnedTargetIsa) -> JITBuilder {
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    for (name, address) in mora_rt::jit_symbols() {
        builder.symbol(name, address);
    }
    builder
}

/// The Cranelift calling convention of `target`'s C convention (§13.7).
///
/// # Errors
///
/// [`CodegenError::UnsupportedTarget`] for a target Cranelift cannot
/// generate for: WebAssembly objects, pointers other than 64 bits wide, and
/// 32-bit Arm.
fn call_conv(target: &Target) -> Result<IsaCallConv, CodegenError> {
    if target.object_format == ObjectFormat::Wasm {
        return Err(unsupported(
            target,
            "Cranelift writes no WebAssembly objects",
        ));
    }
    if target.pointer_width != 64 {
        return Err(unsupported(target, "pointers must be 64 bits wide"));
    }
    match target.call_conv {
        CallConv::Sysv64 | CallConv::Aapcs64 | CallConv::Lp64d => Ok(IsaCallConv::SystemV),
        CallConv::AppleAapcs64 => Ok(IsaCallConv::AppleAarch64),
        // Cranelift's `WindowsFastcall` is Microsoft's convention on both x64
        // and arm64.
        CallConv::Win64 | CallConv::WinArm64 => Ok(IsaCallConv::WindowsFastcall),
        CallConv::Aapcs => Err(unsupported(
            target,
            "Cranelift generates no 32-bit Arm code",
        )),
    }
}

/// Checks, before Cranelift sees `triple`, that it has the pointer width and
/// the byte order of `target`, names the instruction set of its `arch`, is
/// one Cranelift generates for (see [`cranelift_refuses`]), and gives the
/// object format and the operating system of its `object-format` and `os`:
/// Cranelift generates code and writes objects from the triple, while
/// layouts and `@Cfg` follow the description (§19.4 rule 2).
///
/// # Errors
///
/// [`CodegenError::UnsupportedTarget`] for the first check that fails,
/// naming the triple and, where the description has the value, the
/// description's. A triple whose architecture has no known pointer width or
/// byte order is left to the instruction-set check.
fn check_triple(target: &Target, triple: &Triple) -> Result<(), CodegenError> {
    if let Ok(width) = triple.pointer_width()
        && u32::from(width.bits()) != target.pointer_width
    {
        return Err(unsupported(
            target,
            format!(
                "the triple `{}` has {}-bit pointers, but `pointer-width` is {}",
                target.triple,
                width.bits(),
                target.pointer_width
            ),
        ));
    }
    if let Ok(order) = triple.endianness() {
        let endian = match order {
            Endianness::Little => Endian::Little,
            Endianness::Big => Endian::Big,
        };
        if endian != target.endian {
            return Err(unsupported(
                target,
                format!(
                    "the triple `{}` is {}-endian, but `endian` is {}",
                    target.triple,
                    endian.name(),
                    target.endian.name()
                ),
            ));
        }
    }
    let set = instruction_set(triple.architecture);
    if set != target.arch {
        return Err(unsupported(
            target,
            format!(
                "the triple `{}` names the instruction set {set}, but `arch` is {}",
                target.triple, target.arch
            ),
        ));
    }
    if let Some(reason) = cranelift_refuses(triple.architecture, &target.triple) {
        return Err(unsupported(target, reason));
    }
    if object_format(triple.binary_format) != Some(target.object_format) {
        return Err(unsupported(
            target,
            format!(
                "the triple `{}` gives the object format {}, but `object-format` is {}",
                target.triple,
                triple.binary_format,
                target.object_format.name()
            ),
        ));
    }
    let os = operating_system(triple.operating_system);
    if os != target.os {
        return Err(unsupported(
            target,
            format!(
                "the triple `{}` names the operating system {os}, but `os` is {}",
                target.triple, target.os
            ),
        ));
    }
    Ok(())
}

/// Why Cranelift cannot generate for `architecture`, the architecture of
/// `triple`: an instruction set that `isa::lookup` accepts and then asserts
/// on, or one Cranelift generates another variant of than the one named.
fn cranelift_refuses(architecture: Architecture, triple: &str) -> Option<String> {
    match architecture {
        // The aarch64 backend asserts that the triple is little-endian.
        Architecture::Aarch64(Aarch64Architecture::Aarch64be) => Some(format!(
            "Cranelift generates no big-endian aarch64 code, which the triple `{triple}` asks for"
        )),
        // The riscv64 backend needs G and passes floats in registers (LP64D),
        // whatever the triple says.
        Architecture::Riscv64(Riscv64Architecture::Riscv64 | Riscv64Architecture::Riscv64imac) => {
            Some(format!(
                "Cranelift generates riscv64 code only for the G extensions (IMAFD) with \
                 hardware floats, which the triple `{triple}` does not name"
            ))
        }
        _ => None,
    }
}

/// The description's `object-format` of the objects Cranelift writes for
/// `format`, or `None` for a format it writes no objects in.
fn object_format(format: BinaryFormat) -> Option<ObjectFormat> {
    match format {
        BinaryFormat::Elf => Some(ObjectFormat::Elf),
        BinaryFormat::Macho => Some(ObjectFormat::Macho),
        BinaryFormat::Coff => Some(ObjectFormat::Coff),
        _ => None,
    }
}

/// The operating system `os` spelled as a description's `os`: `macos` for
/// both Darwin and macOS, which a triple may name either way, and the
/// triple's own spelling for any other.
fn operating_system(os: OperatingSystem) -> Cow<'static, str> {
    match os {
        OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_) => Cow::Borrowed("macos"),
        other => other.into_str(),
    }
}

/// The instruction set `architecture` belongs to, spelled as a description's
/// `arch`: the architecture's own name, except that a 64-bit RISC-V triple
/// such as `riscv64gc` names its extensions on top of `riscv64`.
fn instruction_set(architecture: Architecture) -> Cow<'static, str> {
    match architecture {
        Architecture::Riscv64(_) => Cow::Borrowed("riscv64"),
        other => other.into_str(),
    }
}

/// Cranelift's `opt_level` setting for `profile`.
fn opt_level(profile: Profile) -> &'static str {
    match profile {
        Profile::Debug => "none",
        Profile::Release => "speed",
    }
}

/// The instruction set of `target`; `pic` asks for position-independent code,
/// which object files need and the JIT does not.
///
/// # Errors
///
/// [`CodegenError::UnsupportedTarget`] when Cranelift cannot generate for
/// the target or its triple, when the triple does not match the
/// description (see [`check_triple`]), or when the convention the triple
/// implies is not the one the description names.
fn isa(target: &Target, profile: Profile, pic: bool) -> Result<OwnedTargetIsa, CodegenError> {
    let call_conv = call_conv(target)?;
    let mut flags = settings::builder();
    flags
        .set("opt_level", opt_level(profile))
        .map_err(cranelift_error)?;
    flags
        .set("is_pic", if pic { "true" } else { "false" })
        .map_err(cranelift_error)?;
    // A result passed in two words is returned as two values.
    flags
        .enable("enable_multi_ret_implicit_sret")
        .map_err(cranelift_error)?;
    // A frame larger than a page touches its pages, inline, before it is
    // used (§13.10): from the top down on x86-64 and aarch64, so that a
    // stack overflow reaches the guard page instead of jumping past it; on
    // riscv64 a frame of four pages or more is probed from the bottom up.
    flags.enable("enable_probestack").map_err(cranelift_error)?;
    flags
        .set("probestack_strategy", "inline")
        .map_err(cranelift_error)?;
    let triple = Triple::from_str(&target.triple).map_err(|error| {
        unsupported(
            target,
            format!("`{}` is not a target triple: {error}", target.triple),
        )
    })?;
    check_triple(target, &triple)?;
    let isa = isa::lookup(triple)
        .map_err(|error| unsupported(target, error))?
        .finish(settings::Flags::new(flags))
        .map_err(cranelift_error)?;
    if isa.default_call_conv() != call_conv {
        return Err(unsupported(
            target,
            format!(
                "the triple implies the {} convention, not {call_conv}",
                isa.default_call_conv()
            ),
        ));
    }
    Ok(isa)
}

/// Declares and defines every function of `program` in `module`, then
/// `__mora_main`, which calls the entry point.
///
/// # Errors
///
/// [`CodegenError::TypeTooLarge`] when a struct of the project or the type
/// of a local takes more than 2^31 bytes, checked before anything is
/// generated; [`CodegenError::FrameTooLarge`] when the stack frame of a
/// function would pass 2^30 bytes, by its stack slots or once Cranelift
/// adds what it needs; and [`CodegenError::Cranelift`] when Cranelift
/// rejects a function otherwise.
///
/// # Panics
///
/// With an internal error for MIR that is not lowered yet, and for an entry
/// point with parameters or with a result other than `int` or a zero-sized
/// one (see [`Exit::of`]).
fn define_program<M: Module>(
    module: &mut M,
    program: &Program<'_>,
    layouts: &Layouts<'_>,
    profile: Profile,
) -> Result<(), CodegenError> {
    let entry = program.function(program.entry());
    // Before the sizes, which need a layout of the result.
    let exit = Exit::of(entry.body, layouts);
    crate::size::check_structs(layouts, entry.key.project)?;
    crate::size::check_locals(layouts, program)?;
    let ids = define_functions(module, program, layouts, profile)?;
    let call_conv = module.isa().default_call_conv();
    let mut signature = Signature::new(call_conv);
    signature.returns.push(AbiParam::new(types::I32));
    let main = module
        .declare_function(MAIN, Linkage::Export, &signature)
        .map_err(cranelift_error)?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, main.as_u32());
    let entry_ref = module.declare_func_in_func(ids[program.entry().index()], &mut context.func);
    let mut function_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);
    let call = builder.ins().call(entry_ref, &[]);
    let code = match exit {
        Exit::Int => builder.inst_results(call)[0],
        Exit::Zero => builder.ins().iconst(types::I32, 0),
    };
    builder.ins().return_(&[code]);
    builder.finalize(module.isa().frontend_config());
    module
        .define_function(main, &mut context)
        .map_err(cranelift_error)
}

/// Declares every function of `program` in `module`, then defines them.
/// Returns their ids, in the order of [`Program::functions`].
fn define_functions<M: Module>(
    module: &mut M,
    program: &Program<'_>,
    layouts: &Layouts<'_>,
    profile: Profile,
) -> Result<Vec<FuncId>, CodegenError> {
    let runtime = Runtime::define(module, program)?;
    let call_conv = module.isa().default_call_conv();
    let pointer = module.isa().pointer_type();
    let mut declared = Vec::with_capacity(program.functions().len());
    for function in program.functions() {
        let abi = FnAbi::of(function.body, layouts);
        let signature = function::signature(&abi, call_conv, pointer);
        let id = module
            .declare_function(&function.symbol, Linkage::Local, &signature)
            .map_err(cranelift_error)?;
        declared.push((id, signature, abi));
    }
    let mut context = module.make_context();
    let mut function_context = FunctionBuilderContext::new();
    let ids: Vec<FuncId> = declared.iter().map(|(id, ..)| *id).collect();
    for (caller, (function, (id, signature, abi))) in
        (0u32..).zip(program.functions().iter().zip(&declared))
    {
        context.func.signature = signature.clone();
        context.func.name = UserFuncName::user(0, id.as_u32());
        let calls = call_targets(
            module,
            program,
            FunctionId(caller),
            &ids,
            layouts,
            &mut context.func,
        );
        let env = runtime.env(module, program, function, &mut context.func, profile, calls);
        crate::size::check_temporaries(layouts, function)?;
        function::lower(
            &mut context.func,
            &mut function_context,
            function.body,
            layouts,
            abi,
            module.isa().frontend_config(),
            &env,
        );
        crate::size::check_frame(program.db(), function, &context.func)?;
        module
            .define_function(*id, &mut context)
            .map_err(|error| match error {
                // The stack slots fit, but with the spilled values, saved
                // registers and call arguments Cranelift adds, the frame
                // does not.
                ModuleError::Compilation(cranelift_codegen::CodegenError::ImplLimitExceeded) => {
                    crate::size::frame_too_large(program.db(), function)
                }
                error => cranelift_error(error),
            })?;
        module.clear_context(&mut context);
    }
    Ok(ids)
}

/// The function each block of `caller`, a function of `program`, calls,
/// declared in `func`, with the layout of its result: `None` for a block that
/// ends in no call, or calls a function of the standard library or one
/// outside `program`. `ids` holds the id of each function of `program`.
fn call_targets<M: Module>(
    module: &mut M,
    program: &Program<'_>,
    caller: FunctionId,
    ids: &[FuncId],
    layouts: &Layouts<'_>,
    func: &mut Function,
) -> Vec<Option<Call>> {
    program
        .function(caller)
        .body
        .blocks
        .iter()
        .map(|block| match &block.terminator.kind {
            TerminatorKind::Call { callee, .. } => {
                program.callee(caller, callee).map(|target| Call {
                    func: module.declare_func_in_func(ids[target.index()], func),
                    result: layouts.of_local(
                        &program.function(target).body.locals[LocalId::RETURN.index()].ty,
                    ),
                })
            }
            _ => None,
        })
        .collect()
}

/// What `__mora_main` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// The entry point's `int` result.
    Int,
    /// 0, for an entry point with a zero-sized result.
    Zero,
}

impl Exit {
    /// The exit of the entry point `entry`.
    ///
    /// # Panics
    ///
    /// With an internal error for an entry point with parameters, which
    /// M1.13 brings, one that returns a `Result`, whose exit codes M3.7
    /// brings, and one with another result that is neither `int` nor
    /// zero-sized, which the front end reports (`MR0120`): another type a
    /// later milestone gives meaning to among them.
    fn of(entry: &Body, layouts: &Layouts<'_>) -> Exit {
        let result = &entry.locals[LocalId::RETURN.index()].ty;
        if entry.params != 0 {
            function::fail(entry, "an entry point with parameters", "M1.13")
        }
        match result.ty() {
            Ty::Primitive(Primitive::Int) => Exit::Int,
            // While the standard library is not loaded, a `Result` is one
            // of its types: those the front end takes for a `Result` here.
            ty if ty.as_external().is_some() && mora_hir::is_entry_return(ty) => {
                function::fail(entry, "an entry point that returns a `Result`", "M3.7")
            }
            ty if !matches!(ty, Ty::Unsupported(_)) && layouts.of_local(result).size == 0 => {
                Exit::Zero
            }
            _ => function::broken(
                entry,
                "an entry point that returns a type other than `()`, `int` or a `Result`",
            ),
        }
    }
}

/// An error Cranelift reported.
fn cranelift_error(error: impl fmt::Display) -> CodegenError {
    CodegenError::Cranelift(error.to_string())
}

/// `target` is not supported, for `reason`.
fn unsupported(target: &Target, reason: impl fmt::Display) -> CodegenError {
    CodegenError::UnsupportedTarget {
        target: target.name.clone(),
        reason: reason.to_string(),
    }
}

/// The host has no built-in target.
fn unsupported_host(error: mora_target::Error) -> CodegenError {
    CodegenError::UnsupportedTarget {
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use cranelift_codegen::settings::{OptLevel, ProbestackStrategy};
    use mora_hir::Construct;
    use mora_hir::test_support::{TestDb, unsupported_of};
    use mora_mir::LocalTy;
    use mora_target::builtin_names;
    use object::{Object, ObjectSection, ObjectSymbol};

    use super::*;
    use crate::test_util::{app_of, build_object, doubling, func, jit, object_of, symbol, x86_64};

    /// Asserts that `isa`, and an object of a program with a string literal
    /// on the heap, reject the built-in target `name`, once `change` has
    /// edited it, for `reason`. The literal's data symbol reaches the
    /// relocations that differ by object format (see `data_symbol`).
    fn rejects_changed(name: &str, change: impl FnOnce(&mut Target), reason: &str) {
        let mut target = Target::builtin(name).unwrap();
        change(&mut target);
        let error = CodegenError::UnsupportedTarget {
            target: name.into(),
            reason: reason.into(),
        };
        assert_eq!(isa(&target, Profile::Debug, true).err().unwrap(), error);
        let text =
            "F(a: string): string => a + \"a literal of more than fifteen bytes\";\nF(\"\");\n";
        assert_eq!(
            build_object(text, &target, Profile::Debug).err().unwrap(),
            error
        );
    }

    /// Memory from the system that records when it is freed, and is then
    /// left allocated, so that the test frees nothing itself.
    struct Recording {
        memory: cranelift_jit::SystemMemoryProvider,
        freed: Arc<AtomicBool>,
    }

    impl cranelift_jit::JITMemoryProvider for Recording {
        fn allocate(
            &mut self,
            size: usize,
            align: u64,
            kind: cranelift_jit::JITMemoryKind,
        ) -> std::io::Result<*mut u8> {
            self.memory.allocate(size, align, kind)
        }

        unsafe fn free_memory(&mut self) {
            self.freed.store(true, Ordering::SeqCst);
        }

        fn finalize(
            &mut self,
            branch_protection: cranelift_jit::BranchProtection,
        ) -> cranelift_module::ModuleResult<()> {
            self.memory.finalize(branch_protection)
        }
    }

    #[test]
    fn dropping_a_program_frees_its_memory() {
        let freed = Arc::new(AtomicBool::new(false));
        let target = Target::host().unwrap();
        let mut builder = jit_builder(isa(&target, Profile::Debug, false).unwrap());
        builder.memory_provider(Box::new(Recording {
            memory: cranelift_jit::SystemMemoryProvider::new(),
            freed: Arc::clone(&freed),
        }));
        let mut jit = JitProgram::new(JITModule::new(builder));
        let db = TestDb::default();
        let project = app_of(&db, "Main(): int { return 7; }\n");
        let program = Program::collect(&db, project).unwrap();
        let layouts = Layouts::new(&db, &target);
        define_program(jit.module_mut(), &program, &layouts, Profile::Debug).unwrap();
        jit.module_mut().finalize_definitions().unwrap();
        assert_eq!(jit.run_main(), 7);
        assert!(!freed.load(Ordering::SeqCst));
        drop(jit);
        assert!(freed.load(Ordering::SeqCst));
    }

    #[test]
    fn main_returning_int_is_the_exit_code() {
        assert_eq!(
            jit("Main(): int { return 7; }\n", Profile::Debug).run_main(),
            7
        );
    }

    #[test]
    fn a_negative_exit_code_keeps_its_value() {
        assert_eq!(
            jit("Main(): int { return -1; }\n", Profile::Debug).run_main(),
            -1
        );
    }

    #[test]
    fn top_level_statements_exit_with_zero() {
        assert_eq!(jit("let x = 1;\n", Profile::Debug).run_main(), 0);
    }

    #[test]
    fn the_release_profile_returns_the_exit_code() {
        assert_eq!(
            jit("Main(): int { return 3; }\n", Profile::Release).run_main(),
            3
        );
    }

    #[test]
    fn the_entry_is_callable_under_its_own_symbol() {
        let program = jit("Main(): int { return 7; }\n", Profile::Debug);
        let main: extern "C" fn() -> i32 = func(&program, "Main");
        assert_eq!(main(), 7);
    }

    #[test]
    fn objects_define_mora_main_and_the_entry_for_every_builtin_target() {
        use object::{Architecture, BinaryFormat};
        let expected = [
            ("x86_64-linux", BinaryFormat::Elf, Architecture::X86_64),
            ("aarch64-linux", BinaryFormat::Elf, Architecture::Aarch64),
            ("aarch64-macos", BinaryFormat::MachO, Architecture::Aarch64),
            ("x86_64-macos", BinaryFormat::MachO, Architecture::X86_64),
            ("x86_64-windows", BinaryFormat::Coff, Architecture::X86_64),
            ("aarch64-windows", BinaryFormat::Coff, Architecture::Aarch64),
            ("riscv64gc-linux", BinaryFormat::Elf, Architecture::Riscv64),
        ];
        assert_eq!(builtin_names().count(), expected.len());
        for (name, format, architecture) in expected {
            let target = Target::builtin(name).unwrap();
            let bytes = object_of("Main(): int { return 7; }\n", &target, Profile::Debug);
            // A pair comes back in two words and a triple of `long`s
            // through a pointer, on every target.
            let aggregates = object_of(
                "struct P {\n    public X: long;\n    public Y: int;\n}\n\
                 struct Big {\n    public A: long;\n    public B: long;\n    public C: long;\n}\n\
                 MakeP(a: long): P => new P { X = a, Y = 1 };\n\
                 MakeBig(a: long): Big => new Big { A = a, B = a, C = a };\n\
                 Take(move p: P, move b: Big): long => p.X + b.C;\n\
                 Main(): int { return Take(MakeP(1), MakeBig(2)) == 3 ? 0 : 1; }\n",
                &target,
                Profile::Debug,
            );
            // Two overloads, each under a symbol of its own.
            let overloads = object_of(
                "F(x: int): int => 1;\nF(x: uint): int => 2;\n\
                 Main(): int { return F(1) + F(1u); }\n",
                &target,
                Profile::Debug,
            );
            let defined = |file: &object::File<'_>, wanted: &str, global: bool| {
                file.symbols().any(|s| {
                    let name = s.name().unwrap();
                    let name = if format == BinaryFormat::MachO {
                        name.strip_prefix('_').unwrap_or(name)
                    } else {
                        name
                    };
                    name == wanted && s.is_definition() && (!global || s.is_global())
                })
            };
            let wide = object::File::parse(&*aggregates).unwrap();
            assert_eq!(wide.architecture(), architecture, "{name}");
            for function in [
                "MakeP(long)",
                "MakeBig(long)",
                "Take(UsersApi.P,UsersApi.Big)",
            ] {
                assert!(
                    defined(&wide, &symbol(function), false),
                    "{name}: no {function}"
                );
            }
            let overloads = object::File::parse(&*overloads).unwrap();
            for function in ["F(int)", "F(uint)"] {
                assert!(
                    defined(&overloads, &symbol(function), false),
                    "{name}: no {function}"
                );
            }
            let file = object::File::parse(&*bytes).unwrap();
            assert_eq!(file.format(), format, "{name}");
            assert_eq!(file.architecture(), architecture, "{name}");
            assert!(defined(&file, MAIN, true), "{name}: no global {MAIN}");
            assert!(defined(&file, &symbol("Main"), false), "{name}: no Main");
            assert!(
                !defined(&file, &symbol("Main"), true),
                "{name}: Main is global"
            );
        }
    }

    #[test]
    fn overloads_run_side_by_side() {
        let text = "struct P {\n    public X: int;\n}\n\
                    F(x: int): int => 1;\nF(x: uint): int => 2;\nF(x: string): int => 3;\n\
                    F(p: P): int => p.X;\nF(t: (int, long)): int => 5;\n\
                    Main(): int { return F(1) * 10000 + F(1u) * 1000 + F(\"s\") * 100 + F(new P { X = 4 }) * 10 + F((1, 2L)); }\n";
        let program = jit(text, Profile::Debug);
        assert_eq!(program.run_main(), 12345);
        let int: extern "C" fn(i32) -> i32 = func(&program, "F(int)");
        let uint: extern "C" fn(u32) -> i32 = func(&program, "F(uint)");
        assert_eq!((int(0), uint(0)), (1, 2));
    }

    #[test]
    fn objects_are_position_independent_and_the_jit_is_not() {
        let target = x86_64();
        assert!(isa(&target, Profile::Debug, true).unwrap().flags().is_pic());
        assert!(
            !isa(&target, Profile::Debug, false)
                .unwrap()
                .flags()
                .is_pic()
        );
        assert!(
            isa(&target, Profile::Debug, false)
                .unwrap()
                .flags()
                .enable_multi_ret_implicit_sret()
        );
    }

    #[test]
    fn unsupported_targets_are_errors() {
        rejects_changed(
            "x86_64-linux",
            |target| target.object_format = ObjectFormat::Wasm,
            "Cranelift writes no WebAssembly objects",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.call_conv = CallConv::Aapcs,
            "Cranelift generates no 32-bit Arm code",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.pointer_width = 32,
            "pointers must be 64 bits wide",
        );
        rejects_changed(
            "x86_64-linux",
            |target| {
                target.triple = "s390x-unknown-linux-gnu".into();
                target.arch = "s390x".into();
                target.endian = Endian::Big;
            },
            "Support for this target is disabled",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.triple = "not a triple".into(),
            "`not a triple` is not a target triple: Unrecognized architecture: not a triple",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.call_conv = CallConv::Win64,
            "the triple implies the system_v convention, not windows_fastcall",
        );
    }

    #[test]
    fn a_triple_with_other_pointers_is_rejected() {
        rejects_changed(
            "aarch64-linux",
            |target| target.triple = "aarch64-unknown-linux-gnu_ilp32".into(),
            "the triple `aarch64-unknown-linux-gnu_ilp32` has 32-bit pointers, \
             but `pointer-width` is 64",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.triple = "x86_64-unknown-linux-gnux32".into(),
            "the triple `x86_64-unknown-linux-gnux32` has 32-bit pointers, \
             but `pointer-width` is 64",
        );
    }

    #[test]
    fn a_triple_of_the_other_byte_order_is_rejected() {
        // Cranelift asserts, rather than reports, that an aarch64 triple is
        // little-endian.
        rejects_changed(
            "aarch64-linux",
            |target| target.triple = "aarch64_be-unknown-linux-gnu".into(),
            "the triple `aarch64_be-unknown-linux-gnu` is big-endian, but `endian` is little",
        );
        rejects_changed(
            "x86_64-linux",
            |target| target.endian = Endian::Big,
            "the triple `x86_64-unknown-linux-musl` is little-endian, but `endian` is big",
        );
    }

    #[test]
    fn a_triple_of_another_instruction_set_is_rejected() {
        rejects_changed(
            "x86_64-linux",
            |target| target.triple = "aarch64-unknown-linux-musl".into(),
            "the triple `aarch64-unknown-linux-musl` names the instruction set aarch64, \
             but `arch` is x86_64",
        );
        // An architecture with no known pointer width or byte order.
        rejects_changed(
            "x86_64-linux",
            |target| target.triple = "unknown-unknown-linux-musl".into(),
            "the triple `unknown-unknown-linux-musl` names the instruction set unknown, \
             but `arch` is x86_64",
        );
        // Big-endian on both sides, with `arch` left at `aarch64`, fails this
        // check; with `arch` changed too, see the test below.
        rejects_changed(
            "aarch64-linux",
            |target| {
                target.triple = "aarch64_be-unknown-linux-gnu".into();
                target.endian = Endian::Big;
            },
            "the triple `aarch64_be-unknown-linux-gnu` names the instruction set aarch64_be, \
             but `arch` is aarch64",
        );
    }

    #[test]
    fn a_big_endian_aarch64_triple_is_rejected_whatever_the_arch() {
        // Cranelift asserts, rather than reports, that an aarch64 triple is
        // little-endian.
        rejects_changed(
            "aarch64-linux",
            |target| {
                target.triple = "aarch64_be-unknown-linux-gnu".into();
                target.arch = "aarch64_be".into();
                target.endian = Endian::Big;
            },
            "Cranelift generates no big-endian aarch64 code, which the triple \
             `aarch64_be-unknown-linux-gnu` asks for",
        );
    }

    #[test]
    fn a_riscv64_triple_without_the_g_extensions_is_rejected() {
        for triple in [
            "riscv64imac-unknown-linux-musl",
            "riscv64-unknown-linux-musl",
        ] {
            rejects_changed(
                "riscv64gc-linux",
                |target| target.triple = triple.into(),
                &format!(
                    "Cranelift generates riscv64 code only for the G extensions (IMAFD) with \
                     hardware floats, which the triple `{triple}` does not name"
                ),
            );
        }
        let mut a23 = Target::builtin("riscv64gc-linux").unwrap();
        a23.triple = "riscv64a23-unknown-linux-musl".into();
        assert!(isa(&a23, Profile::Debug, true).is_ok());
    }

    #[test]
    fn a_triple_of_another_object_format_is_rejected() {
        let cases = [
            (
                "aarch64-linux",
                "aarch64-unknown-linux-musl-coff",
                "coff",
                "elf",
            ),
            ("x86_64-linux", "x86_64-apple-darwin", "macho", "elf"),
            ("x86_64-macos", "x86_64-unknown-linux-musl", "elf", "macho"),
            (
                "x86_64-windows",
                "x86_64-pc-windows-gnullvm-elf",
                "elf",
                "coff",
            ),
            (
                "x86_64-linux",
                "x86_64-unknown-linux-musl-xcoff",
                "xcoff",
                "elf",
            ),
            (
                "x86_64-linux",
                "x86_64-unknown-linux-musl-unknown",
                "unknown",
                "elf",
            ),
        ];
        for (name, triple, format, object_format) in cases {
            rejects_changed(
                name,
                |target| target.triple = triple.into(),
                &format!(
                    "the triple `{triple}` gives the object format {format}, \
                     but `object-format` is {object_format}"
                ),
            );
        }
    }

    #[test]
    fn a_triple_of_another_operating_system_is_rejected() {
        let cases = [
            ("x86_64-linux", "x86_64-unknown-freebsd", "freebsd", "linux"),
            ("x86_64-linux", "x86_64-unknown-uefi", "uefi", "linux"),
            ("aarch64-macos", "aarch64-apple-ios", "ios", "macos"),
            (
                "x86_64-windows",
                "x86_64-pc-cygwin-coff",
                "cygwin",
                "windows",
            ),
        ];
        for (name, triple, os, expected) in cases {
            rejects_changed(
                name,
                |target| target.triple = triple.into(),
                &format!(
                    "the triple `{triple}` names the operating system {os}, \
                     but `os` is {expected}"
                ),
            );
        }
        // Darwin and macOS both spell `macos`.
        let mut macos = Target::builtin("x86_64-macos").unwrap();
        macos.triple = "x86_64-apple-macosx10.12.0".into();
        assert!(isa(&macos, Profile::Debug, true).is_ok());
    }

    #[test]
    fn an_unknown_host_is_unsupported() {
        let unknown = || mora_target::Error::Unknown("pdp11-unix".into());
        assert_eq!(
            unsupported_host(unknown()),
            CodegenError::UnsupportedTarget {
                target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
                reason: unknown().to_string(),
            }
        );
    }

    #[test]
    fn cranelift_errors_keep_the_message() {
        assert_eq!(
            cranelift_error("boom"),
            CodegenError::Cranelift("boom".into())
        );
    }

    #[test]
    fn profiles_choose_the_optimization_level() {
        assert_eq!(opt_level(Profile::Debug), "none");
        assert_eq!(opt_level(Profile::Release), "speed");
        let target = x86_64();
        let debug = isa(&target, Profile::Debug, false).unwrap();
        assert_eq!(debug.flags().opt_level(), OptLevel::None);
        let release = isa(&target, Profile::Release, false).unwrap();
        assert_eq!(release.flags().opt_level(), OptLevel::Speed);
    }

    #[test]
    fn calling_conventions_follow_the_target() {
        let expected = [
            ("x86_64-linux", IsaCallConv::SystemV),
            ("aarch64-linux", IsaCallConv::SystemV),
            ("aarch64-macos", IsaCallConv::AppleAarch64),
            ("x86_64-macos", IsaCallConv::SystemV),
            ("x86_64-windows", IsaCallConv::WindowsFastcall),
            ("aarch64-windows", IsaCallConv::WindowsFastcall),
            ("riscv64gc-linux", IsaCallConv::SystemV),
        ];
        assert_eq!(builtin_names().count(), expected.len());
        for (name, conv) in expected {
            let target = Target::builtin(name).unwrap();
            assert_eq!(call_conv(&target).unwrap(), conv, "{name}");
            assert_eq!(
                isa(&target, Profile::Debug, true)
                    .unwrap()
                    .default_call_conv(),
                conv,
                "{name}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "internal error generating code for UsersApi.Main: \
                               invalid MIR: an entry point that returns a type other than \
                               `()`, `int` or a `Result`")]
    fn an_entry_returning_another_type_is_an_internal_error() {
        // The front end reports it (`MR0120`); code generation still checks.
        let db = TestDb::default();
        let project =
            mora_hir::test_support::app(&db, &[("src/Program.mr", "Main(): long { return 7; }\n")]);
        let program = Program::collect(&db, project).unwrap();
        let _ = Cranelift.jit(&program, Profile::Debug);
    }

    /// The panic message of compiling an entry point whose result is
    /// `ty`. Lowering stops earlier on such a result, so the body of
    /// `Main() {}` is given it here.
    fn panic_of_an_entry_returning(ty: Ty) -> String {
        let db = TestDb::default();
        let project = app_of(&db, "Main() {}\n");
        let file = mora_hir::test_support::file_named(&db, project, "src/Program.mr");
        let item = mora_hir::test_support::item_named(&db, project, file, "Main");
        let mir = mora_mir::mir(&db, project, file, mora_hir::BodyOwner::Item(item.item));
        let mut body = mir.bodies[0].clone();
        body.locals[LocalId::RETURN.index()].ty = LocalTy::Value(ty);
        let program = Program::from_body(&db, project, file, &body);
        let payload = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = Cranelift.jit(&program, Profile::Debug);
        }))
        .expect_err("the entry point compiled");
        payload.downcast::<String>().map(|text| *text).unwrap()
    }

    #[test]
    fn an_entry_returning_a_result_waits_for_results() {
        // A `Result` is a type of the standard library, with its two type
        // arguments or, written without them, none.
        let int = Ty::Primitive(Primitive::Int);
        for ty in [
            mora_hir::test_support::result_of(Ty::UNIT, Ty::String),
            mora_hir::test_support::result_of(int, Ty::String),
            unsupported_of(Construct::External, Vec::new()),
        ] {
            assert_eq!(
                panic_of_an_entry_returning(ty),
                "internal error generating code for UsersApi.Main: an entry point that \
                 returns a `Result` is not lowered until M3.7"
            );
        }
    }

    #[test]
    fn an_entry_returning_another_unsupported_type_is_broken_mir() {
        // The front end reports these (`MR0120`). An external type with two
        // arguments is a `Result` only when the first is `()` or `int`; its
        // name is not kept, so another such type looks the same.
        let int = || Ty::Primitive(Primitive::Int);
        let result_of = mora_hir::test_support::result_of;
        for ty in [
            result_of(Ty::String, Ty::String),
            result_of(Ty::Primitive(Primitive::Bool), int()),
            result_of(result_of(int(), Ty::String), Ty::String),
            unsupported_of(Construct::External, vec![int()]),
            unsupported_of(Construct::External, vec![int(), int(), int()]),
            unsupported_of(Construct::Optional, vec![int()]),
            unsupported_of(Construct::Function, vec![int()]),
            unsupported_of(Construct::Class, Vec::new()),
            unsupported_of(Construct::Generic, vec![int(), Ty::String]),
        ] {
            assert_eq!(
                panic_of_an_entry_returning(ty.clone()),
                "internal error generating code for UsersApi.Main: invalid MIR: an entry \
                 point that returns a type other than `()`, `int` or a `Result`",
                "{ty:?}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "an entry point with parameters")]
    fn an_entry_with_parameters_is_an_internal_error() {
        let db = TestDb::default();
        let project = app_of(&db, "F(a: int): int => 1;\n");
        let file = mora_hir::test_support::file_named(&db, project, "src/Program.mr");
        let item = mora_hir::test_support::item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, mora_hir::BodyOwner::Item(item.item));
        let program = Program::from_body(&db, project, file, &mir.bodies[0]);
        let _ = Cranelift.jit(&program, Profile::Debug);
    }

    #[test]
    #[should_panic(expected = "an entry point with parameters")]
    fn an_entry_with_parameters_and_no_result_is_an_internal_error() {
        let db = TestDb::default();
        let project = app_of(&db, "F(a: int) {}\n");
        let file = mora_hir::test_support::file_named(&db, project, "src/Program.mr");
        let item = mora_hir::test_support::item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, mora_hir::BodyOwner::Item(item.item));
        let program = Program::from_body(&db, project, file, &mir.bodies[0]);
        let _ = Cranelift.jit(&program, Profile::Debug);
    }

    #[test]
    fn every_target_probes_the_stack_inline() {
        for name in builtin_names() {
            let target = Target::builtin(name).unwrap();
            for profile in [Profile::Debug, Profile::Release] {
                let isa = isa(&target, profile, true).unwrap();
                assert!(isa.flags().enable_probestack(), "{name}");
                assert_eq!(
                    isa.flags().probestack_strategy(),
                    ProbestackStrategy::Inline,
                    "{name}"
                );
            }
        }
    }

    /// `F`, whose locals take 128 KiB, built up from a `long` of 7 that it
    /// returns, and a call of it.
    fn large_frame() -> String {
        // Writing to a `String` cannot fail.
        let mut text = doubling(13);
        text.push_str("F(): long {\n    let s0 = new S0 { A = 7 };\n");
        for i in 1..=13 {
            let before = i - 1;
            let _ = writeln!(
                text,
                "    let s{i} = new S{i} {{ A = s{before}, B = s{before} }};"
            );
        }
        let _ = write!(
            text,
            "    return s13{};\n}}\nF();\n",
            ".B".repeat(13) + ".A"
        );
        text
    }

    #[test]
    fn a_large_frame_runs_with_its_pages_probed() {
        let text = large_frame();
        for profile in [Profile::Debug, Profile::Release] {
            let program = jit(&text, profile);
            assert_eq!(func::<extern "C" fn() -> i64>(&program, "F")(), 7);
        }
    }

    /// The code of the function `F()` in the object of `text` built for
    /// `target`.
    fn code_of_f(text: &str, target: &Target) -> Vec<u8> {
        let bytes = object_of(text, target, Profile::Release);
        let file = object::File::parse(&*bytes).unwrap();
        let f = file
            .symbols()
            .find(|s| s.name() == Ok(symbol("F()").as_str()))
            .unwrap();
        let section = file.section_by_index(f.section_index().unwrap()).unwrap();
        let start = usize::try_from(f.address() - section.address()).unwrap();
        let end = start + usize::try_from(f.size()).unwrap();
        section.data().unwrap()[start..end].to_vec()
    }

    /// The little-endian 32-bit immediate at `at` in `code`.
    fn imm32(code: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(code[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn x86_64_probes_each_page_of_a_large_frame_first() {
        // After saving the frame pointer, and before any other access to
        // the stack, `F` steps the stack pointer down a 4 KiB page at a
        // time, from the top, and stores to each page; then it puts the
        // stack pointer back and allocates the frame the pages cover:
        //
        //       push rbp ; mov rbp, rsp
        //       mov r11, rsp ; sub r11, PROBED
        //   L:  sub rsp, 0x1000 ; mov dword [rsp], 0
        //       cmp r11, rsp ; jne L
        //       add rsp, PROBED ; sub rsp, FRAME
        // A local of `S19`, 4 MiB.
        let code = code_of_f(&frame_of([19]), &x86_64());
        assert_eq!(
            code[..4],
            [0x55, 0x48, 0x89, 0xE5],
            "push rbp; mov rbp, rsp"
        );
        assert_eq!(
            code[4..10],
            [0x49, 0x89, 0xE3, 0x49, 0x81, 0xEB],
            "mov r11, rsp; sub r11"
        );
        let probed = imm32(&code, 10);
        const LOOP: [u8; 23] = [
            0x48, 0x81, 0xEC, 0x00, 0x10, 0x00, 0x00, // sub rsp, 0x1000
            0xC7, 0x04, 0x24, 0x00, 0x00, 0x00, 0x00, // mov dword [rsp], 0
            0x4C, 0x3B, 0xDC, // cmp r11, rsp
            0x0F, 0x85, 0xE9, 0xFF, 0xFF, 0xFF, // jne back to the `sub`
        ];
        assert_eq!(code[14..37], LOOP);
        assert_eq!(code[37..40], [0x48, 0x81, 0xC4], "add rsp");
        assert_eq!(imm32(&code, 40), probed);
        assert_eq!(code[44..47], [0x48, 0x81, 0xEC], "sub rsp");
        let frame = imm32(&code, 47);
        // Whole pages, every one the frame reaches into and no more.
        assert_eq!(probed % 4096, 0);
        assert!(
            frame >= 4 << 20 && frame <= probed && probed - frame < 4096,
            "{frame} {probed}"
        );
    }

    #[test]
    fn aarch64_probes_each_page_of_a_large_frame_first() {
        // After saving the frame pointer and link register, and before any
        // other access to the stack, `F` stores to each 4 KiB page below the
        // stack pointer, from the top down, until it passes the frame:
        //
        //       stp x29, x30, [sp, #-16]! ; mov x29, sp
        //       mov x16, #0 ; mov x17, #END (one or more instructions)
        //   L:  sub x16, x16, #1, lsl #12 ; str wzr, [sp, x16]
        //       cmn x16, x17 ; b.gt L
        let code = code_of_f(&frame_of([19]), &Target::builtin("aarch64-linux").unwrap());
        let words: Vec<u32> = code
            .chunks(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect();
        assert_eq!(words[..3], [0xA9BF_7BFD, 0x9100_03FD, 0xD280_0010]);
        // A `movz` and `movk`s into w17 or x17 build the end.
        let mut end = 0u64;
        let mut at = 3;
        while let Some(&word) = words.get(at)
            && word & 0x1F == 17
            && matches!(word & 0x7F80_0000, 0x5280_0000 | 0x7280_0000)
        {
            end |= u64::from((word >> 5) & 0xFFFF) << (16 * ((word >> 21) & 3));
            at += 1;
        }
        assert_eq!(
            words[at..at + 4],
            [0xD140_0610, 0xB830_6BFF, 0xAB11_021F, 0x54FF_FFAC],
            "sub x16, x16, #1, lsl #12; str wzr, [sp, x16]; cmn x16, x17; b.gt"
        );
        assert!(end >= 4 << 20, "{end}");
    }

    #[test]
    fn riscv64_probes_a_large_frame_from_the_bottom_up() {
        // This pins the order Cranelift 0.137 uses on riscv64, which §13.10
        // states as an exception: its loop stores first to the bottom of the
        // probed area, `sp - STEP`, and moves up a page at a time, stopping
        // before the page just below `sp`. A frame that runs past the free
        // stack is touched first far below the guard page. Reported as
        // https://github.com/bytecodealliance/wasmtime/issues/15447. When a
        // Cranelift update probes from the top down, as on x86-64 and
        // aarch64, this test fails; then the exception leaves §13.10 and
        // this test checks the new order.
        //
        //       addi sp, sp, -16 ; sd ra, 8(sp) ; sd s0, 0(sp) ; mv s0, sp
        //       lui t6, STEP ; lui t3, 0x1
        //   L:  bgeu t3, t6, done ; sub t5, sp, t6 ; sb zero, 0(t5)
        //       sub t6, t6, t3 ; j L
        let code = code_of_f(
            &frame_of([19]),
            &Target::builtin("riscv64gc-linux").unwrap(),
        );
        let words: Vec<u32> = code
            .chunks(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect();
        assert_eq!(
            words[..4],
            [0xFF01_0113, 0x0011_3423, 0x0081_3023, 0x0001_0413]
        );
        // `lui t6, STEP`: the step starts at the bottom, the whole frame.
        assert_eq!(words[4] & 0xFFF, 0xFB7, "lui t6");
        let step = words[4] & 0xFFFF_F000;
        assert_eq!(step, 4 << 20);
        assert_eq!(words[5], 0x0000_1E37, "lui t3, 0x1");
        assert_eq!(
            words[6..11],
            [
                0x01FE_7A63,
                0x41F1_0F33,
                0x000F_0023,
                0x41CF_8FB3,
                0xFF1F_F06F
            ],
            "bgeu t3, t6, done; sub t5, sp, t6; sb zero, 0(t5); sub t6, t6, t3; j L"
        );
    }

    /// The structs of [`doubling`] up to `S28`, a function `F` with a local
    /// of each struct `S{i}` for `i` in `sizes`, and a call of it.
    fn frame_of(sizes: impl IntoIterator<Item = u32>) -> String {
        // Writing to a `String` cannot fail.
        let mut text = doubling(28);
        text.push_str("F(): int {\n");
        for i in sizes {
            let _ = writeln!(text, "    var a{i}: S{i};");
        }
        text.push_str("    return 1;\n}\nF();\n");
        text
    }

    /// What building the app of `text` for x86_64 gives.
    fn build(text: &str) -> Result<Vec<u8>, CodegenError> {
        build_object(text, &x86_64(), Profile::Debug)
    }

    #[test]
    fn a_frame_at_the_limit_builds() {
        // `S13` to `S26` take 2^30 - 2^16 bytes. Running it would need a
        // 1 GiB stack, so it is only built.
        let size: u64 = (13..=26).map(|i| 8u64 << i).sum();
        assert_eq!(size, crate::size::MAX_FRAME_SLOTS);
        assert!(build(&frame_of(13..=26)).is_ok());
    }

    #[test]
    fn a_frame_past_the_limit_is_reported_at_its_function() {
        let text = frame_of((13..=26).chain([0]));
        let Err(CodegenError::FrameTooLarge { name, span }) = build(&text) else {
            panic!("built");
        };
        assert_eq!(name, "UsersApi.F");
        assert_eq!(&text[std::ops::Range::<usize>::from(span.range)], "F");
        let diagnostic = CodegenError::FrameTooLarge { name, span }
            .diagnostic()
            .unwrap();
        assert_eq!(diagnostic.code.to_string(), "MR0156");
        assert_eq!(
            diagnostic.primary.message.as_deref(),
            Some("its stack frame is too large")
        );
    }

    #[test]
    #[ignore = "compiling a call with 9,000 arguments takes minutes; the nightly workflow runs it"]
    fn a_frame_too_large_for_the_code_generator_is_reported_at_its_function() {
        // The call to `G` keeps each of its 9,000 arguments in a stack slot
        // of 8 bytes, and passes each on the stack too. The locals and the
        // slots of the arguments take 2^30 - 71,360 bytes, within the limit
        // of the slots; the 72,000 bytes of arguments passed on the stack
        // take the frame past 2^30.
        let locals = [9, 11, 12, 13].into_iter().chain(15..=26);
        let size: u64 = locals.clone().map(|i| 8u64 << i).sum::<u64>() + 9000 * 8;
        assert_eq!(size, (1 << 30) - 71_360);
        assert!(size <= crate::size::MAX_FRAME_SLOTS);
        let arguments: Vec<String> = (0..9000).map(|i| i.to_string()).collect();
        let parameters: Vec<String> = (0..9000).map(|i| format!("p{i}: int")).collect();
        let mut text = frame_of(locals).replace(
            "    return 1;\n",
            &format!("    return G({});\n", arguments.join(", ")),
        );
        // Writing to a `String` cannot fail.
        let _ = writeln!(text, "G({}): int => 1;", parameters.join(", "));
        let error = build(&text).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the stack frame of `UsersApi.F` would take more than 2^30 bytes"
        );
        let CodegenError::FrameTooLarge { span, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(&text[std::ops::Range::<usize>::from(span.range)], "F");
    }

    #[test]
    fn large_locals_of_the_top_level_statements_are_reported() {
        for locals in [
            "var t: S28;\n",
            "var t: S27;\n",
            "var a: S26;\nvar b: S26;\nvar c: S26;\n",
        ] {
            let text = format!("{}{locals}", doubling(28));
            assert!(
                matches!(build(&text), Err(CodegenError::FrameTooLarge { .. })),
                "{locals:?}"
            );
        }
    }

    #[test]
    fn a_large_parameter_has_no_slot() {
        let program = function::tests::jit_one_in(
            &format!("{}F(a: S28): int => 1;\n", doubling(28)),
            "F",
            Profile::Debug,
        );
        let _ = func::<extern "C" fn(*const u8) -> i32>(&program, "F");
    }
}
