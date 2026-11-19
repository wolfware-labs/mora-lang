//! The Cranelift backend (§14.13): object files for every built-in target,
//! and the JIT on the host.

mod aggregates;
mod checks;
mod function;
mod ops;
mod runtime;
mod strings;

use std::fmt;
use std::str::FromStr;

use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName, types};
use cranelift_codegen::isa::{self, CallConv as IsaCallConv, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use mora_hir::{Primitive, Ty};
use mora_mir::{Body, LocalId, LocalTy, TerminatorKind};
use mora_target::{CallConv, ObjectFormat, Target};
use target_lexicon::Triple;

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
        // COFF has no position-independent code: the linker adjusts the
        // addresses an image holds.
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
/// the target, or when the convention its triple implies is not the one the
/// description names.
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
    let triple = Triple::from_str(&target.triple).map_err(|error| {
        unsupported(
            target,
            format!("`{}` is not a target triple: {error}", target.triple),
        )
    })?;
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
/// `__mora_main`, which calls the entry point. Returns the id of
/// `__mora_main`.
///
/// # Errors
///
/// [`CodegenError::Cranelift`] when Cranelift rejects a function.
///
/// # Panics
///
/// With an internal error for MIR that is not lowered yet, and for an entry
/// point with parameters or with a result other than `int` or a zero-sized
/// one, which M1.13 brings.
fn define_program<M: Module>(
    module: &mut M,
    program: &Program<'_>,
    layouts: &Layouts<'_>,
    profile: Profile,
) -> Result<FuncId, CodegenError> {
    let entry = program.functions()[program.entry().0 as usize].body;
    let exit = Exit::of(entry, layouts);
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
    let entry_ref = module.declare_func_in_func(ids[program.entry().0 as usize], &mut context.func);
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
        .map_err(cranelift_error)?;
    Ok(main)
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
            (FunctionId(caller), function.body),
            &ids,
            layouts,
            &mut context.func,
        );
        let env = runtime.env(module, program, function, &mut context.func, profile, calls);
        function::lower(
            &mut context.func,
            &mut function_context,
            function.body,
            layouts,
            abi,
            module.isa().frontend_config(),
            &env,
        );
        module
            .define_function(*id, &mut context)
            .map_err(cranelift_error)?;
        module.clear_context(&mut context);
    }
    Ok(ids)
}

/// The function each block of `caller`, a function of `program` and its
/// body, calls, declared in `func`, with the layout of its result: `None` for
/// a block that ends in no call, or calls a function of the standard library
/// or one outside `program`. `ids` holds the id of each function of
/// `program`.
fn call_targets<M: Module>(
    module: &mut M,
    program: &Program<'_>,
    (caller, body): (FunctionId, &Body),
    ids: &[FuncId],
    layouts: &Layouts<'_>,
    func: &mut Function,
) -> Vec<Option<Call>> {
    body.blocks
        .iter()
        .map(|block| match &block.terminator.kind {
            TerminatorKind::Call { callee, .. } => {
                program.callee(caller, callee).map(|target| Call {
                    func: module.declare_func_in_func(ids[target.0 as usize], func),
                    result: layouts.of_local(
                        &program.functions()[target.0 as usize].body.locals
                            [LocalId::RETURN.index()]
                        .ty,
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
    /// With an internal error for an entry point with parameters or a result
    /// that is neither `int` nor zero-sized.
    fn of(entry: &Body, layouts: &Layouts<'_>) -> Exit {
        let result = &entry.locals[LocalId::RETURN.index()].ty;
        if entry.params == 0 && *result == LocalTy::Value(Ty::Primitive(Primitive::Int)) {
            Exit::Int
        } else if entry.params == 0 && layouts.of_local(result).size == 0 {
            Exit::Zero
        } else {
            panic!(
                "internal error generating code for {}: an entry point with parameters \
                 or a result other than `int` is not lowered until M1.13",
                entry.name
            )
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use cranelift_codegen::settings::OptLevel;
    use mora_hir::test_support::TestDb;
    use mora_target::builtin_names;
    use object::{Object, ObjectSymbol};

    use super::*;
    use crate::test_util::{app_of, func, jit, object_of, symbol};

    fn x86_64_linux() -> Target {
        Target::builtin("x86_64-linux").unwrap()
    }

    fn is_unsupported(result: Result<OwnedTargetIsa, CodegenError>, reason: &str) {
        assert_eq!(
            result.err().unwrap(),
            CodegenError::UnsupportedTarget {
                target: "x86_64-linux".into(),
                reason: reason.into(),
            }
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
            let wide = object::File::parse(&*aggregates).unwrap();
            assert_eq!(wide.architecture(), architecture, "{name}");
            let file = object::File::parse(&*bytes).unwrap();
            assert_eq!(file.format(), format, "{name}");
            assert_eq!(file.architecture(), architecture, "{name}");
            let defined = |wanted: &str, global: bool| {
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
            assert!(defined(MAIN, true), "{name}: no global {MAIN}");
            assert!(defined(&symbol("Main"), false), "{name}: no Main");
            assert!(!defined(&symbol("Main"), true), "{name}: Main is global");
        }
    }

    #[test]
    fn objects_are_position_independent_and_the_jit_is_not() {
        let target = x86_64_linux();
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
        let mut wasm = x86_64_linux();
        wasm.object_format = ObjectFormat::Wasm;
        is_unsupported(
            isa(&wasm, Profile::Debug, true),
            "Cranelift writes no WebAssembly objects",
        );
        let mut arm = x86_64_linux();
        arm.call_conv = CallConv::Aapcs;
        is_unsupported(
            isa(&arm, Profile::Debug, true),
            "Cranelift generates no 32-bit Arm code",
        );
        let mut narrow = x86_64_linux();
        narrow.pointer_width = 32;
        is_unsupported(
            isa(&narrow, Profile::Debug, true),
            "pointers must be 64 bits wide",
        );
        let mut disabled = x86_64_linux();
        disabled.triple = "s390x-unknown-linux-gnu".into();
        is_unsupported(
            isa(&disabled, Profile::Debug, true),
            "Support for this target is disabled",
        );
        let mut garbage = x86_64_linux();
        garbage.triple = "not a triple".into();
        is_unsupported(
            isa(&garbage, Profile::Debug, true),
            "`not a triple` is not a target triple: Unrecognized architecture: not a triple",
        );
        let mut mismatch = x86_64_linux();
        mismatch.call_conv = CallConv::Win64;
        is_unsupported(
            isa(&mismatch, Profile::Debug, true),
            "the triple implies the system_v convention, not windows_fastcall",
        );
        let db = TestDb::default();
        let project = app_of(&db, "Main(): int { return 7; }\n");
        let program = Program::collect(&db, project).unwrap();
        assert!(matches!(
            Cranelift.object(&program, &wasm, Profile::Debug),
            Err(CodegenError::UnsupportedTarget { .. })
        ));
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
        let target = x86_64_linux();
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
                               an entry point with parameters or a result other than `int` \
                               is not lowered until M1.13")]
    fn an_entry_returning_another_type_is_an_internal_error() {
        jit("Main(): long { return 7; }\n", Profile::Debug);
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
}
