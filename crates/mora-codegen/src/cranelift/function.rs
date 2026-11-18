//! The lowering of one MIR body to a Cranelift function.

use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{
    self, AbiParam, InstBuilder, Signature, StackSlot, StackSlotData, StackSlotKind, TrapCode,
    Type, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use mora_mir::{
    Body, Const, LocalId, Operand, Projection, Rvalue, Statement, StatementKind, Terminator,
    TerminatorKind,
};

use crate::abi::{FnAbi, PassMode};
use crate::layout::{Layouts, Scalar, Shape};

/// The trap code of `Unreachable`.
const UNREACHABLE: TrapCode = TrapCode::unwrap_user(1);

/// Where a local lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Storage {
    /// Nowhere: its type is zero-sized.
    None,
    /// In a variable of the type.
    Variable(Type),
    /// In a stack slot.
    Slot {
        /// The size in bytes, a multiple of the pointer size.
        size: u32,
        /// The alignment in bytes.
        align: u32,
    },
}

/// Where each local of `body` lives, in the order of `Body::locals`: a
/// scalar whose address is never taken is a variable, any other local with
/// a non-zero size is a stack slot.
pub(super) fn storage(body: &Body, layouts: &Layouts<'_>, pointer: Type) -> Vec<Storage> {
    let borrowed = borrowed_locals(body);
    body.locals
        .iter()
        .zip(borrowed)
        .map(|(decl, borrowed)| {
            let layout = layouts.of_local(&decl.ty);
            match layout.shape {
                _ if layout.size == 0 => Storage::None,
                Shape::Scalar(scalar) if !borrowed => {
                    Storage::Variable(scalar_type(scalar, pointer))
                }
                _ => Storage::Slot {
                    size: layout.size.next_multiple_of(layouts.pointer_bytes()),
                    align: layout.align,
                },
            }
        })
        .collect()
}

/// Whether the address of each local is taken: a borrow of a place on it
/// that does not go through a dereference.
fn borrowed_locals(body: &Body) -> Vec<bool> {
    let mut borrowed = vec![false; body.locals.len()];
    for statement in body.blocks.iter().flat_map(|block| &block.statements) {
        if let StatementKind::Assign(_, Rvalue::Borrow(_, place)) = &statement.kind
            && !place.projections.contains(&Projection::Deref)
        {
            borrowed[place.local.index()] = true;
        }
    }
    borrowed
}

/// The Cranelift type of a scalar.
pub(super) fn scalar_type(scalar: Scalar, pointer: Type) -> Type {
    match scalar {
        // An integer is 1, 2, 4 or 8 bytes wide (layout.rs).
        Scalar::Int { bytes: 1, .. } | Scalar::Bool => types::I8,
        Scalar::Int { bytes: 2, .. } => types::I16,
        Scalar::Int { bytes: 4, .. } | Scalar::Char => types::I32,
        Scalar::Int { .. } => types::I64,
        Scalar::Float => types::F32,
        Scalar::Double => types::F64,
        Scalar::Pointer => pointer,
    }
}

/// The Cranelift signature of `body` under `abi`.
///
/// # Panics
///
/// With an internal error for a value passed in words or by pointer, which
/// is not lowered yet.
pub(super) fn signature(body: &Body, abi: &FnAbi, call_conv: CallConv, pointer: Type) -> Signature {
    let mut signature = Signature::new(call_conv);
    for &mode in &abi.params {
        signature
            .params
            .extend(abi_type(body, mode, pointer).map(AbiParam::new));
    }
    signature
        .returns
        .extend(abi_type(body, abi.ret, pointer).map(AbiParam::new));
    signature
}

/// The type a value passed in `mode` crosses a call as; `None` for nothing.
fn abi_type(body: &Body, mode: PassMode, pointer: Type) -> Option<Type> {
    match mode {
        PassMode::Ignore => None,
        PassMode::Direct(scalar) => Some(scalar_type(scalar, pointer)),
        PassMode::Words(_) | PassMode::Indirect => {
            fail(body, &format!("a value passed as {mode:?}"))
        }
    }
}

/// Lowers `body` into `func`, whose signature is already set.
///
/// # Panics
///
/// With an internal error for MIR that is not lowered yet.
pub(super) fn lower(
    func: &mut ir::Function,
    context: &mut FunctionBuilderContext,
    body: &Body,
    layouts: &Layouts<'_>,
    config: TargetFrontendConfig,
) {
    if body.params > 0 {
        fail(body, "a parameter");
    }
    let mut builder = FunctionBuilder::new(func, context);
    let homes = storage(body, layouts, config.pointer_type())
        .into_iter()
        .map(|storage| Home::new(&mut builder, storage))
        .collect();
    let blocks = body.blocks.iter().map(|_| builder.create_block()).collect();
    let mut lowering = Lowering {
        body,
        builder,
        homes,
        blocks,
    };
    for (index, block) in body.blocks.iter().enumerate() {
        lowering.builder.switch_to_block(lowering.blocks[index]);
        for statement in &block.statements {
            lowering.statement(statement);
        }
        lowering.terminator(&block.terminator);
    }
    lowering.builder.seal_all_blocks();
    lowering.builder.finalize(config);
}

/// Panics with an internal error in `body`: `what` is not lowered yet.
fn fail(body: &Body, what: &str) -> ! {
    panic!(
        "internal error generating code for {}: {what} is not lowered yet",
        body.name
    )
}

/// The Cranelift home of a local.
#[derive(Clone, Copy, Debug)]
enum Home {
    None,
    Variable(Variable, Type),
    Slot(
        #[expect(
            dead_code,
            reason = "the lowering of places in memory reads it; a dump shows it now"
        )]
        StackSlot,
    ),
}

impl Home {
    fn new(builder: &mut FunctionBuilder<'_>, storage: Storage) -> Home {
        match storage {
            Storage::None => Home::None,
            Storage::Variable(ty) => Home::Variable(builder.declare_var(ty), ty),
            Storage::Slot { size, align } => {
                Home::Slot(builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size,
                    align.trailing_zeros() as u8,
                )))
            }
        }
    }
}

/// The lowering of one body.
struct Lowering<'a, 'f> {
    body: &'a Body,
    builder: FunctionBuilder<'f>,
    homes: Vec<Home>,
    blocks: Vec<ir::Block>,
}

impl Lowering<'_, '_> {
    fn statement(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
            StatementKind::Assign(place, Rvalue::Use(Operand::Const(constant)))
                if place.projections.is_empty() =>
            {
                self.assign_constant(place.local, constant);
            }
            other => fail(self.body, &format!("the statement `{other:?}`")),
        }
    }

    fn terminator(&mut self, terminator: &Terminator) {
        match &terminator.kind {
            TerminatorKind::Goto(target) => {
                self.builder.ins().jump(self.blocks[target.index()], &[]);
            }
            TerminatorKind::Return => {
                let result = match self.homes[LocalId::RETURN.index()] {
                    Home::None => None,
                    _ => {
                        let (variable, _) = self.scalar(LocalId::RETURN);
                        Some(self.builder.use_var(variable))
                    }
                };
                self.builder.ins().return_(result.as_slice());
            }
            TerminatorKind::Unreachable => {
                self.builder.ins().trap(UNREACHABLE);
            }
            other => fail(self.body, &format!("the terminator `{other:?}`")),
        }
    }

    /// The variable of `local`, which holds a scalar, and its type.
    fn scalar(&self, local: LocalId) -> (Variable, Type) {
        match self.homes[local.index()] {
            Home::Variable(variable, ty) => (variable, ty),
            home => fail(self.body, &format!("_{} in {home:?}", local.0)),
        }
    }

    /// Stores a scalar constant in `local`. `()` stores nothing.
    fn assign_constant(&mut self, local: LocalId, constant: &Const) {
        // The bits of the value: an integer in two's complement, a real as
        // the bits of an `f64`.
        let bits = match constant {
            Const::Int(value, _) => *value as i64,
            Const::Real(bits, _) => *bits as i64,
            Const::Bool(value) => i64::from(*value),
            Const::Char(value) => i64::from(u32::from(*value)),
            Const::Unit => return,
            Const::String(_) => fail(self.body, "a string constant"),
        };
        let (variable, ty) = self.scalar(local);
        let ins = self.builder.ins();
        let value = if ty == types::F32 {
            ins.f32const(Ieee32::with_float(f64::from_bits(bits as u64) as f32))
        } else if ty == types::F64 {
            ins.f64const(Ieee64::with_bits(bits as u64))
        } else {
            ins.iconst(ty, bits)
        };
        self.builder.def_var(variable, value);
    }
}

#[cfg(test)]
mod tests {
    use cranelift_codegen::ir::{Function, UserFuncName};
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::default_libcall_names;
    use mora_hir::BodyOwner;
    use mora_hir::Primitive;
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_mir::{BlockId, Place};
    use mora_target::Target;

    use super::*;
    use crate::Profile;
    use crate::abi::FnAbi;
    use crate::cranelift::{define_functions, isa};
    use crate::jit::JitProgram;
    use crate::program::Program;
    use crate::test_util::{app_of, func};

    /// The body of `owner` in the app of `text`.
    fn body_of(db: &TestDb, text: &str, name: Option<&str>) -> Body {
        let project = app_of(db, text);
        let file = file_named(db, project, "src/Program.mr");
        let owner = match name {
            Some(name) => BodyOwner::Item(item_named(db, project, file, name).item),
            None => BodyOwner::TopLevel,
        };
        mora_mir::mir(db, project, file, owner).bodies[0].clone()
    }

    /// The function `name` of the app of `text`, alone, compiled by the JIT.
    fn jit_one(text: &str, name: &str) -> JitProgram {
        let db = TestDb::default();
        let project = app_of(&db, text);
        let file = file_named(&db, project, "src/Program.mr");
        let item = item_named(&db, project, file, name);
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        let program = Program::from_body(&db, project, file, &mir.bodies[0]);
        let target = Target::host().unwrap();
        let isa = isa(&target, Profile::Debug, false).unwrap();
        let mut jit = JitProgram::new(JITModule::new(JITBuilder::with_isa(
            isa,
            default_libcall_names(),
        )));
        let layouts = Layouts::new(&db, &target);
        define_functions(jit.module_mut(), &program, &layouts).unwrap();
        jit.module_mut().finalize_definitions().unwrap();
        jit
    }

    /// The storage of each named local of a body, with `_0` first.
    fn storage_by_name(text: &str, name: Option<&str>) -> Vec<(String, Storage)> {
        let db = TestDb::default();
        let body = body_of(&db, text, name);
        let layouts = Layouts::new(&db, &Target::builtin("x86_64-linux").unwrap());
        body.locals
            .iter()
            .zip(storage(&body, &layouts, types::I64))
            .enumerate()
            .filter_map(|(index, (decl, storage))| {
                let name = match &decl.source {
                    Some(source) => source.name.clone(),
                    None if index == 0 => "_0".to_owned(),
                    None => return None,
                };
                Some((name, storage))
            })
            .collect()
    }

    fn slot(size: u32, align: u32) -> Storage {
        Storage::Slot { size, align }
    }

    #[test]
    fn scalars_whose_address_is_not_taken_are_variables() {
        let text = "Bump(ref x: int) {}\nPass(ref x: int) { Bump(ref x); }\nvar n = 1;\nBump(ref n);\n\
                    let t = (1, 2);\nlet s: (byte, bool, byte) = (1, true, 2);\nlet b: byte = 1;\n\
                    let l: long = 1;\n";
        assert_eq!(
            storage_by_name(text, None),
            [
                ("_0".to_owned(), Storage::None),
                ("n".to_owned(), slot(8, 4)),
                ("t".to_owned(), slot(8, 4)),
                ("s".to_owned(), slot(8, 1)),
                ("b".to_owned(), Storage::Variable(types::I8)),
                ("l".to_owned(), Storage::Variable(types::I64)),
            ]
        );
        // A borrow through a dereference takes the address of what the
        // local points to, not of the local.
        assert_eq!(
            storage_by_name(text, Some("Pass")),
            [
                ("_0".to_owned(), Storage::None),
                ("x".to_owned(), Storage::Variable(types::I64)),
            ]
        );
    }

    #[test]
    fn scalars_have_their_cranelift_types() {
        let int = |bytes| Scalar::Int {
            bytes,
            signed: true,
        };
        let pointer = types::I32;
        assert_eq!(scalar_type(int(1), pointer), types::I8);
        assert_eq!(scalar_type(int(2), pointer), types::I16);
        assert_eq!(scalar_type(int(4), pointer), types::I32);
        assert_eq!(scalar_type(int(8), pointer), types::I64);
        assert_eq!(scalar_type(Scalar::Bool, pointer), types::I8);
        assert_eq!(scalar_type(Scalar::Char, pointer), types::I32);
        assert_eq!(scalar_type(Scalar::Float, pointer), types::F32);
        assert_eq!(scalar_type(Scalar::Double, pointer), types::F64);
        assert_eq!(scalar_type(Scalar::Pointer, pointer), types::I32);
    }

    #[test]
    fn constants_have_their_values() {
        let jit = jit_one("F(): double => 2.5;\n", "F");
        assert_eq!(func::<extern "C" fn() -> f64>(&jit, "F")(), 2.5);
        let jit = jit_one("F(): float => 0.1;\n", "F");
        assert_eq!(func::<extern "C" fn() -> f32>(&jit, "F")(), 0.1);
        let jit = jit_one("F(): bool => true;\n", "F");
        assert!(func::<extern "C" fn() -> bool>(&jit, "F")());
        let jit = jit_one("F(): bool => false;\n", "F");
        assert!(!func::<extern "C" fn() -> bool>(&jit, "F")());
        let jit = jit_one("F(): char => 'é';\n", "F");
        assert_eq!(func::<extern "C" fn() -> u32>(&jit, "F")(), 0xE9);
        let jit = jit_one("F(): long => 5000000000;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i64>(&jit, "F")(), 5_000_000_000);
        let jit = jit_one("F(): byte => 200;\n", "F");
        assert_eq!(func::<extern "C" fn() -> u8>(&jit, "F")(), 200);
        let jit = jit_one("F(): short => 30000;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i16>(&jit, "F")(), 30000);
    }

    #[test]
    fn negative_constants_keep_their_values() {
        let jit = jit_one("F(): int => -1;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i32>(&jit, "F")(), -1);
        let jit = jit_one("F(): short => -30000;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i16>(&jit, "F")(), -30000);
        let jit = jit_one("F(): sbyte => -128;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i8>(&jit, "F")(), -128);
        let jit = jit_one("F(): long => -5000000000;\n", "F");
        assert_eq!(func::<extern "C" fn() -> i64>(&jit, "F")(), -5_000_000_000);
    }

    #[test]
    fn blocks_run_in_order_and_locals_hold_values() {
        let jit = jit_one("F(): int { while true { return 1; } return 2; }\n", "F");
        assert_eq!(func::<extern "C" fn() -> i32>(&jit, "F")(), 1);
        let jit = jit_one("F(): int { { let y = 2; } return 3; }\n", "F");
        assert_eq!(func::<extern "C" fn() -> i32>(&jit, "F")(), 3);
        let jit = jit_one("F() { let y = 2; }\n", "F");
        func::<extern "C" fn()>(&jit, "F")();
    }

    /// The Cranelift IR of the function `name` of the app of `text`, after
    /// `edit` changes its MIR.
    fn lowered_with(text: &str, name: &str, edit: impl FnOnce(&mut Body)) -> String {
        let db = TestDb::default();
        let mut body = body_of(&db, text, Some(name));
        edit(&mut body);
        let target = Target::builtin("x86_64-linux").unwrap();
        let layouts = Layouts::new(&db, &target);
        let isa = isa(&target, Profile::Debug, false).unwrap();
        let abi = FnAbi::of(&body, &layouts);
        let mut func = Function::with_name_signature(
            UserFuncName::user(0, 0),
            signature(&body, &abi, isa.default_call_conv(), isa.pointer_type()),
        );
        lower(
            &mut func,
            &mut FunctionBuilderContext::new(),
            &body,
            &layouts,
            isa.frontend_config(),
        );
        func.display().to_string()
    }

    #[test]
    fn unreachable_traps() {
        let text = lowered_with(
            "F(): int { while true { return 1; } return 2; }\n",
            "F",
            |body| {
                body.blocks[BlockId::ENTRY.index()].terminator.kind = TerminatorKind::Unreachable;
            },
        );
        assert!(text.contains("trap user1"), "{text}");
    }

    #[test]
    #[should_panic(
        expected = "UsersApi.F: the statement `Assign(Place { local: LocalId(0), \
                               projections: [Field(0)] }"
    )]
    fn a_constant_stored_through_a_projection_is_an_internal_error() {
        lowered_with("F(): int { return 7; }\n", "F", |body| {
            body.blocks[0].statements[0].kind = StatementKind::Assign(
                Place {
                    local: LocalId::RETURN,
                    projections: vec![Projection::Field(0)],
                },
                Rvalue::Use(Operand::Const(Const::Int(7, Primitive::Int))),
            );
        });
    }

    #[test]
    #[should_panic(expected = "internal error generating code for UsersApi.F: \
                               a value passed as Words(1) is not lowered yet")]
    fn a_result_in_words_is_an_internal_error() {
        jit_one("F(): (int, int) => (1, 2);\n", "F");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: a parameter is not lowered yet")]
    fn a_parameter_is_an_internal_error() {
        jit_one("F(a: int): int => 1;\n", "F");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: the statement `Assign(")]
    fn an_aggregate_is_an_internal_error() {
        jit_one("F(): int { let t = (1, 2); return 1; }\n", "F");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: a string constant is not lowered yet")]
    fn a_string_constant_is_an_internal_error() {
        jit_one("F() { let s = \"a\"; }\n", "F");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: _1 in Slot(ss0) is not lowered yet")]
    fn a_local_in_memory_is_an_internal_error() {
        jit_one(
            "Bump(ref x: int) {}\nF() { var n = 1; Bump(ref n); }\n",
            "F",
        );
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: the terminator `Call")]
    fn a_call_is_an_internal_error() {
        jit_one("G() {}\nF() { G(); }\n", "F");
    }
}
