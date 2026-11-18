//! The lowering of one MIR body to a Cranelift function.

use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{
    self, AbiParam, FuncRef, InstBuilder, MemFlagsData, Signature, StackSlot, StackSlotData,
    StackSlotKind, TrapCode, Type, Value, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use mora_mir::{
    BlockId, Body, Callee, Const, LocalId, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, TerminatorKind,
};

use crate::abi::{FnAbi, PassMode};
use crate::layout::{Layout, Layouts, Scalar, Shape};

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
        signature.params.extend(abi_param(body, mode, pointer));
    }
    signature.returns.extend(abi_param(body, abi.ret, pointer));
    signature
}

/// How a value passed in `mode` crosses a call; `None` for nothing. An
/// integer narrower than 32 bits is extended by its signedness and a `bool`
/// is zero-extended, as the C conventions of the targets expect.
fn abi_param(body: &Body, mode: PassMode, pointer: Type) -> Option<AbiParam> {
    let scalar = match mode {
        PassMode::Ignore => return None,
        PassMode::Direct(scalar) => scalar,
        PassMode::Words(_) | PassMode::Indirect => {
            fail(body, &format!("a value passed as {mode:?}"))
        }
    };
    let param = AbiParam::new(scalar_type(scalar, pointer));
    Some(match scalar {
        Scalar::Int { bytes, signed } if bytes < 4 => {
            if signed {
                param.sext()
            } else {
                param.uext()
            }
        }
        Scalar::Bool => param.uext(),
        _ => param,
    })
}

/// Lowers `body` into `func`, whose signature is already set. `calls` has
/// one entry per block of `body`: the function its call terminator reaches,
/// `None` for a block that ends in no call, or calls a function of the
/// standard library or one outside the program.
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
    calls: &[Option<FuncRef>],
) {
    let pointer = config.pointer_type();
    let mut builder = FunctionBuilder::new(func, context);
    let homes = storage(body, layouts, pointer)
        .into_iter()
        .map(|storage| Home::new(&mut builder, storage))
        .collect();
    let blocks: Vec<ir::Block> = body.blocks.iter().map(|_| builder.create_block()).collect();
    builder.append_block_params_for_function_params(blocks[BlockId::ENTRY.index()]);
    let mut lowering = Lowering {
        body,
        layouts,
        pointer,
        builder,
        homes,
        blocks,
        calls,
    };
    for (index, block) in body.blocks.iter().enumerate() {
        lowering.builder.switch_to_block(lowering.blocks[index]);
        if index == BlockId::ENTRY.index() {
            lowering.parameters();
        }
        for statement in &block.statements {
            lowering.statement(statement);
        }
        lowering.terminator(index, &block.terminator);
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
    Variable(Variable),
    Slot(StackSlot),
}

impl Home {
    fn new(builder: &mut FunctionBuilder<'_>, storage: Storage) -> Home {
        match storage {
            Storage::None => Home::None,
            Storage::Variable(ty) => Home::Variable(builder.declare_var(ty)),
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
    layouts: &'a Layouts<'a>,
    pointer: Type,
    builder: FunctionBuilder<'f>,
    homes: Vec<Home>,
    blocks: Vec<ir::Block>,
    calls: &'a [Option<FuncRef>],
}

impl Lowering<'_, '_> {
    /// Stores the parameters, which the entry block receives, in their
    /// locals.
    fn parameters(&mut self) {
        let entry = self.blocks[BlockId::ENTRY.index()];
        let mut values = self.builder.block_params(entry).to_vec().into_iter();
        let abi = FnAbi::of(self.body, self.layouts);
        for (local, mode) in self.body.param_ids().zip(abi.params) {
            if mode != PassMode::Ignore {
                self.write(&Place::local(local), values.next());
            }
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
            StatementKind::Assign(place, Rvalue::Use(operand)) => {
                let value = self.operand(operand);
                self.write(place, value);
            }
            StatementKind::Assign(place, Rvalue::Borrow(_, borrowed)) => {
                let address = self.place_address(borrowed);
                self.write(place, Some(address));
            }
            other => fail(self.body, &format!("the statement `{other:?}`")),
        }
    }

    /// Lowers the terminator of the block at `index`.
    fn terminator(&mut self, index: usize, terminator: &Terminator) {
        match &terminator.kind {
            TerminatorKind::Goto(target) => {
                self.builder.ins().jump(self.blocks[target.index()], &[]);
            }
            TerminatorKind::Switch {
                discr,
                targets,
                otherwise,
            } => self.switch(discr, targets, *otherwise),
            TerminatorKind::Return => {
                let result = self.read(&Place::local(LocalId::RETURN));
                self.builder.ins().return_(result.as_slice());
            }
            TerminatorKind::Call {
                callee,
                args,
                dest,
                next,
            } => self.call(self.calls[index], callee, args, dest.as_ref(), *next),
            TerminatorKind::Unreachable => {
                self.builder.ins().trap(UNREACHABLE);
            }
        }
    }

    /// Continues in the block of the constant `discr` equals, or in
    /// `otherwise`.
    ///
    /// # Panics
    ///
    /// With an internal error when `discr` is not a `bool`, an integer or a
    /// `char`, or a constant is not one either.
    fn switch(&mut self, discr: &Operand, targets: &[(Const, BlockId)], otherwise: BlockId) {
        let layout = self.operand_layout(discr);
        let (Some(value), Shape::Scalar(Scalar::Int { .. } | Scalar::Bool | Scalar::Char)) =
            (self.operand(discr), layout.shape)
        else {
            panic!(
                "internal error generating code for {}: a switch on `{discr:?}`, which is \
                 not a bool, an integer or a char",
                self.body.name
            )
        };
        // A constant's bits, cut to the width of the discriminant: the
        // switch compares them unsigned.
        let bits = self.builder.func.dfg.value_type(value).bits();
        let mask = u128::MAX >> (128 - bits);
        let mut switch = Switch::new();
        for (constant, target) in targets {
            let entry = match constant {
                Const::Int(value, _) => *value as u128,
                Const::Bool(value) => u128::from(*value),
                Const::Char(value) => u128::from(u32::from(*value)),
                other => panic!(
                    "internal error generating code for {}: a switch case on `{other:?}`",
                    self.body.name
                ),
            };
            switch.set_entry(entry & mask, self.blocks[target.index()]);
        }
        switch.emit(&mut self.builder, value, self.blocks[otherwise.index()]);
    }

    /// Calls `target`, the function `callee` names, writes its result to
    /// `dest` once it returns, and continues in `next`; traps when the call
    /// returns although it has no `next`.
    ///
    /// # Panics
    ///
    /// With an internal error for a function of the standard library, which
    /// M1.15 brings, or a function outside the program.
    fn call(
        &mut self,
        target: Option<FuncRef>,
        callee: &Callee,
        args: &[Operand],
        dest: Option<&Place>,
        next: Option<BlockId>,
    ) {
        let Some(target) = target else {
            match callee {
                Callee::External(path) => panic!(
                    "internal error generating code for {}: a call to `{}` of the standard \
                     library is not lowered until M1.15",
                    self.body.name,
                    path.join(".")
                ),
                other => panic!(
                    "internal error generating code for {}: the callee `{other:?}` is not in \
                     the program",
                    self.body.name
                ),
            }
        };
        // A zero-sized argument has no value, and its parameter none either.
        let args: Vec<Value> = args.iter().filter_map(|arg| self.operand(arg)).collect();
        let call = self.builder.ins().call(target, &args);
        let result = self.builder.inst_results(call).first().copied();
        if let Some(dest) = dest {
            self.write(dest, result);
        }
        match next {
            Some(next) => {
                self.builder.ins().jump(self.blocks[next.index()], &[]);
            }
            None => {
                self.builder.ins().trap(UNREACHABLE);
            }
        }
    }

    /// The value of `operand`; `None` for a zero-sized one.
    fn operand(&mut self, operand: &Operand) -> Option<Value> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.read(place),
            Operand::Const(constant) => self.constant(constant),
            // Only a body lowered for constant evaluation holds one.
            Operand::ConstRead(read) => fail(self.body, &format!("the read `{read:?}`")),
        }
    }

    /// The layout of the value of `operand`.
    fn operand_layout(&self, operand: &Operand) -> Layout {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.place_layout(place),
            Operand::Const(constant) => self.layouts.of(&constant.ty()),
            Operand::ConstRead(read) => self.layouts.of(&read.ty),
        }
    }

    /// The layout of the value at `place`.
    fn place_layout(&self, place: &Place) -> Layout {
        let ty = &self.body.locals[place.local.index()].ty;
        match place.projections.as_slice() {
            [] => self.layouts.of_local(ty),
            [Projection::Deref] => self.layouts.of(ty.ty()),
            _ => fail(self.body, &format!("the place `{place:?}`")),
        }
    }

    /// The Cranelift type of a value of `layout`; `None` for a zero-sized
    /// one.
    fn value_type(&self, layout: &Layout) -> Option<Type> {
        match layout.shape {
            _ if layout.size == 0 => None,
            Shape::Scalar(scalar) => Some(scalar_type(scalar, self.pointer)),
            ref shape => fail(self.body, &format!("a value of shape {shape:?}")),
        }
    }

    /// The value at `place`; `None` for a zero-sized one.
    fn read(&mut self, place: &Place) -> Option<Value> {
        let ty = self.value_type(&self.place_layout(place))?;
        if let (true, Home::Variable(variable)) = (
            place.projections.is_empty(),
            self.homes[place.local.index()],
        ) {
            return Some(self.builder.use_var(variable));
        }
        let address = self.place_address(place);
        Some(
            self.builder
                .ins()
                .load(ty, MemFlagsData::trusted(), address, 0),
        )
    }

    /// Stores `value` at `place`; `None`, a zero-sized value, stores nothing.
    fn write(&mut self, place: &Place, value: Option<Value>) {
        let Some(value) = value else {
            return;
        };
        if let (true, Home::Variable(variable)) = (
            place.projections.is_empty(),
            self.homes[place.local.index()],
        ) {
            self.builder.def_var(variable, value);
            return;
        }
        let address = self.place_address(place);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, address, 0);
    }

    /// The address of `place`, which is in memory: a local in a stack slot,
    /// or the value a borrow refers to.
    fn place_address(&mut self, place: &Place) -> Value {
        match place.projections.as_slice() {
            [] => self.local_address(place.local),
            // A borrow is a pointer, never zero-sized.
            [Projection::Deref] => self
                .read(&Place::local(place.local))
                .unwrap_or_else(|| fail(self.body, &format!("the place `{place:?}`"))),
            _ => fail(self.body, &format!("the place `{place:?}`")),
        }
    }

    /// The address of `local`. A zero-sized local has no memory, and its
    /// address is its alignment: never null, and never read through.
    fn local_address(&mut self, local: LocalId) -> Value {
        match self.homes[local.index()] {
            Home::Slot(slot) => self.builder.ins().stack_addr(self.pointer, slot, 0),
            Home::None => {
                let align = self
                    .layouts
                    .of_local(&self.body.locals[local.index()].ty)
                    .align;
                self.builder.ins().iconst(self.pointer, i64::from(align))
            }
            home @ Home::Variable(_) => fail(
                self.body,
                &format!("the address of _{} in {home:?}", local.0),
            ),
        }
    }

    /// The value of a scalar constant; `None` for `()`.
    fn constant(&mut self, constant: &Const) -> Option<Value> {
        // The bits of the value: an integer in two's complement, a real as
        // the bits of an `f64`.
        let bits = match constant {
            Const::Int(value, _) => *value as i64,
            Const::Real(bits, _) => *bits as i64,
            Const::Bool(value) => i64::from(*value),
            Const::Char(value) => i64::from(u32::from(*value)),
            Const::Unit => return None,
            Const::String(_) => fail(self.body, "a string constant"),
        };
        let ty = self.value_type(&self.layouts.of(&constant.ty()))?;
        let ins = self.builder.ins();
        Some(if ty == types::F32 {
            ins.f32const(Ieee32::with_float(f64::from_bits(bits as u64) as f32))
        } else if ty == types::F64 {
            ins.f64const(Ieee64::with_bits(bits as u64))
        } else {
            ins.iconst(ty, bits)
        })
    }
}

#[cfg(test)]
mod tests {
    use cranelift_codegen::ir::{
        ArgumentExtension, ExtFuncData, ExternalName, Function, UserExternalName, UserFuncName,
    };
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::default_libcall_names;
    use mora_hir::BodyOwner;
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_hir::{Primitive, Ty};
    use mora_mir::{Block, BorrowMode, LocalDecl, LocalTy};
    use mora_target::Target;

    use super::*;
    use crate::Profile;
    use crate::abi::FnAbi;
    use crate::cranelift::{define_functions, isa};
    use crate::jit::JitProgram;
    use crate::program::Program;
    use crate::test_util::{app_of, func, jit};

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
        jit_one_with(text, name, |_| {})
    }

    /// The function `name` of the app of `text`, alone, compiled by the JIT
    /// after `edit` changes its MIR.
    fn jit_one_with(text: &str, name: &str, edit: impl FnOnce(&mut Body)) -> JitProgram {
        let db = TestDb::default();
        let project = app_of(&db, text);
        let file = file_named(&db, project, "src/Program.mr");
        let mut body = body_of(&db, text, Some(name));
        edit(&mut body);
        let program = Program::from_body(&db, project, file, &body);
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
    /// `edit` changes its MIR. Every call in it calls the function itself.
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
        let calls: Vec<Option<FuncRef>> = body
            .blocks
            .iter()
            .map(|block| {
                matches!(block.terminator.kind, TerminatorKind::Call { .. }).then(|| {
                    let signature = func.signature.clone();
                    let signature = func.import_signature(signature);
                    let name = func.declare_imported_user_function(UserExternalName::new(0, 0));
                    func.import_function(ExtFuncData {
                        name: ExternalName::user(name),
                        signature,
                        colocated: true,
                        patchable: false,
                    })
                })
            })
            .collect();
        lower(
            &mut func,
            &mut FunctionBuilderContext::new(),
            &body,
            &layouts,
            isa.frontend_config(),
            &calls,
        );
        func.display().to_string()
    }

    /// A statement of `body` that assigns `rvalue` to `place`.
    fn assign(body: &Body, place: Place, rvalue: Rvalue) -> Statement {
        Statement {
            kind: StatementKind::Assign(place, rvalue),
            span: body.span,
        }
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
    fn a_call_that_does_not_return_traps_after_it() {
        let text = lowered_with(
            "G(move a: int): int => a;\nF(move a: int): int => G(a);\n",
            "F",
            |body| {
                let TerminatorKind::Call { next, .. } =
                    &mut body.blocks[BlockId::ENTRY.index()].terminator.kind
                else {
                    panic!("F calls G first");
                };
                *next = None;
            },
        );
        let call = text.find("call fn0").expect(&text);
        let trap = text.find("trap user1").expect(&text);
        assert!(call < trap, "{text}");
    }

    #[test]
    fn narrow_integers_and_bools_are_extended() {
        let db = TestDb::default();
        let body = body_of(&db, "F() {}\n", Some("F"));
        let int = |bytes, signed| PassMode::Direct(Scalar::Int { bytes, signed });
        let abi = FnAbi {
            params: vec![
                int(1, true),
                int(2, true),
                int(1, false),
                int(2, false),
                int(4, true),
                int(8, false),
                PassMode::Direct(Scalar::Bool),
                PassMode::Direct(Scalar::Char),
                PassMode::Ignore,
            ],
            ret: int(2, true),
        };
        let signature = signature(&body, &abi, CallConv::SystemV, types::I64);
        let extensions: Vec<ArgumentExtension> = signature
            .params
            .iter()
            .map(|param| param.extension)
            .collect();
        use ArgumentExtension::{None, Sext, Uext};
        assert_eq!(extensions, [Sext, Sext, Uext, Uext, None, None, Uext, None]);
        assert_eq!(signature.returns[0].extension, Sext);
    }

    #[test]
    fn a_switch_on_a_constant_takes_its_branch() {
        let program = jit_one_with(
            "Pick(c: bool): int { if c { return 1; } return 2; }\n",
            "Pick",
            |body| {
                let TerminatorKind::Switch { discr, .. } =
                    &mut body.blocks[BlockId::ENTRY.index()].terminator.kind
                else {
                    panic!("Pick switches first");
                };
                *discr = Operand::Const(Const::Bool(false));
            },
        );
        assert_eq!(
            func::<extern "C" fn(&bool) -> i32>(&program, "Pick")(&true),
            2
        );
    }

    #[test]
    fn a_borrow_of_a_zero_sized_local_is_its_alignment() {
        let text = lowered_with("F(): int => 1;\n", "F", |body| {
            body.locals.push(LocalDecl {
                ty: LocalTy::Value(Ty::UNIT),
                mutable: false,
                source: None,
            });
            body.locals.push(LocalDecl {
                ty: LocalTy::Borrow(BorrowMode::In, Ty::UNIT),
                mutable: false,
                source: None,
            });
            let borrow = assign(
                body,
                Place::local(LocalId(2)),
                Rvalue::Borrow(BorrowMode::In, Place::local(LocalId(1))),
            );
            body.blocks[0].statements.insert(0, borrow);
        });
        assert!(text.contains("iconst.i64 1"), "{text}");
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: the place `Place { local: LocalId(0), \
                               projections: [Field(0)] }` is not lowered yet")]
    fn a_store_to_a_field_is_an_internal_error() {
        lowered_with("F(): int { return 7; }\n", "F", |body| {
            body.blocks[0].statements[0] = assign(
                body,
                Place {
                    local: LocalId::RETURN,
                    projections: vec![Projection::Field(0)],
                },
                Rvalue::Use(Operand::Const(Const::Int(7, Primitive::Int))),
            );
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: the place `Place { local: LocalId(0), \
                               projections: [Field(0)] }` is not lowered yet")]
    fn a_read_of_a_field_is_an_internal_error() {
        lowered_with("F(): int { return 7; }\n", "F", |body| {
            body.blocks[0].statements[0] = assign(
                body,
                Place::local(LocalId::RETURN),
                Rvalue::Use(Operand::Copy(Place {
                    local: LocalId::RETURN,
                    projections: vec![Projection::Field(0)],
                })),
            );
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: the place `Place { local: LocalId(0), \
                               projections: [Deref] }` is not lowered yet")]
    fn a_dereference_of_a_zero_sized_value_is_an_internal_error() {
        lowered_with("F() {}\n", "F", |body| {
            let borrow = assign(
                body,
                Place::local(LocalId::RETURN),
                Rvalue::Borrow(
                    BorrowMode::In,
                    Place {
                        local: LocalId::RETURN,
                        projections: vec![Projection::Deref],
                    },
                ),
            );
            body.blocks[0].statements.insert(0, borrow);
        });
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: a value of shape Aggregate([0, 4]) is not lowered yet")]
    fn a_copy_of_an_aggregate_is_an_internal_error() {
        lowered_with("F(): int { let t = (1, 2); return 1; }\n", "F", |body| {
            for statement in body
                .blocks
                .iter_mut()
                .flat_map(|block| &mut block.statements)
            {
                if let StatementKind::Assign(place, rvalue @ Rvalue::Aggregate(..)) =
                    &mut statement.kind
                {
                    *rvalue = Rvalue::Use(Operand::Copy(place.clone()));
                }
            }
        });
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: a switch on \
                               `Copy(Place { local: LocalId(1), projections: [] })`, which is \
                               not a bool, an integer or a char"
    )]
    fn a_switch_on_a_real_is_an_internal_error() {
        lowered_with("F(move a: double): int => 1;\n", "F", |body| {
            switch_on_parameter(body, [Const::Real(0, Primitive::Double), Const::Unit]);
        });
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: a switch case on \
                               `Real(0, Double)`"
    )]
    fn a_switch_case_on_a_real_is_an_internal_error() {
        lowered_with("F(move a: int): int => 1;\n", "F", |body| {
            switch_on_parameter(
                body,
                [
                    Const::Int(1, Primitive::Int),
                    Const::Real(0, Primitive::Double),
                ],
            );
        });
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.Outer: the callee \
                               `Local(1)` is not in the program"
    )]
    fn a_callee_outside_the_program_is_an_internal_error() {
        jit_one(
            "Outer(): int { Inner(): int => 3; return Inner(); }\n",
            "Outer",
        );
    }

    #[test]
    #[should_panic(expected = "internal error generating code for UsersApi.F: \
                               a value passed as Words(1) is not lowered yet")]
    fn a_result_in_words_is_an_internal_error() {
        jit_one("F(): (int, int) => (1, 2);\n", "F");
    }

    #[test]
    #[should_panic(expected = "internal error generating code for UsersApi.F: \
                               a value passed as Indirect is not lowered yet")]
    fn a_parameter_passed_by_pointer_is_an_internal_error() {
        jit_one("F(move t: (long, long, long)): int => 1;\n", "F");
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
    fn in_parameters_are_pointers() {
        let program = jit("F(a: int): int => a;\nF(1);\n", Profile::Debug);
        let f: extern "C" fn(&i32) -> i32 = func(&program, "F");
        assert_eq!(f(&41), 41);
    }

    #[test]
    fn move_parameters_are_values() {
        let program = jit("F(move a: long): long => a;\nF(1);\n", Profile::Debug);
        let f: extern "C" fn(i64) -> i64 = func(&program, "F");
        assert_eq!(f(-5), -5);
    }

    #[test]
    fn ref_parameters_write_through() {
        let program = jit(
            "Set(ref x: int) { x = 5; }\nvar n = 0;\nSet(ref n);\n",
            Profile::Debug,
        );
        let set: extern "C" fn(&mut i32) = func(&program, "Set");
        let mut v = 0;
        set(&mut v);
        assert_eq!(v, 5);
    }

    #[test]
    fn a_borrowed_local_lives_in_memory() {
        let program = jit(
            "Set(ref x: int) { x = 5; }\nG(): int { var n = 1; Set(ref n); return n; }\nG();\n",
            Profile::Debug,
        );
        assert_eq!(func::<extern "C" fn() -> i32>(&program, "G")(), 5);
        // A parameter passed by value whose address is taken is stored in
        // its slot on entry.
        let program = jit(
            "H(x: int): int => x;\nG(move a: int): int => H(a);\nG(1);\n",
            Profile::Debug,
        );
        assert_eq!(func::<extern "C" fn(i32) -> i32>(&program, "G")(7), 7);
    }

    #[test]
    fn calls_reach_items_and_local_functions() {
        let program = jit(
            "Outer(): int { Inner(): int => 3; return Inner(); }\nOuter();\n",
            Profile::Debug,
        );
        assert_eq!(func::<extern "C" fn() -> i32>(&program, "Outer")(), 3);
        let program = jit(
            "Second(move a: int, move b: int): int => b;\nF(): int => Second(1, 2);\nF();\n",
            Profile::Debug,
        );
        assert_eq!(func::<extern "C" fn() -> i32>(&program, "F")(), 2);
        // A zero-sized result is written nowhere.
        let program = jit(
            "G() {}\nF(): int { G(); return 4; }\nF();\n",
            Profile::Debug,
        );
        assert_eq!(func::<extern "C" fn() -> i32>(&program, "F")(), 4);
    }

    #[test]
    fn bool_switches_pick_the_branch() {
        let program = jit(
            "Pick(c: bool): int { if c { return 1; } return 2; }\nPick(true);\n",
            Profile::Debug,
        );
        let pick: extern "C" fn(&bool) -> i32 = func(&program, "Pick");
        assert_eq!(pick(&true), 1);
        assert_eq!(pick(&false), 2);
    }

    /// A body that switches on its parameter `_1`: the first constant of
    /// `cases` sets `_0` to 10, the second to 20, any other value to 30.
    fn switch_on_parameter(body: &mut Body, cases: [Const; 2]) {
        let span = body.span;
        let block = |value: i128| Block {
            statements: vec![Statement {
                kind: StatementKind::Assign(
                    Place::local(LocalId::RETURN),
                    Rvalue::Use(Operand::Const(Const::Int(value, Primitive::Int))),
                ),
                span,
            }],
            terminator: Terminator {
                kind: TerminatorKind::Return,
                span,
            },
        };
        let [first, second] = cases;
        body.blocks = vec![
            Block {
                statements: Vec::new(),
                terminator: Terminator {
                    kind: TerminatorKind::Switch {
                        discr: Operand::Copy(Place::local(LocalId(1))),
                        targets: vec![(first, BlockId(1)), (second, BlockId(2))],
                        otherwise: BlockId(3),
                    },
                    span,
                },
            },
            block(10),
            block(20),
            block(30),
        ];
    }

    #[test]
    fn integer_and_char_switches_mask_their_constants() {
        let program = jit_one_with("F(move a: int): int => 1;\n", "F", |body| {
            switch_on_parameter(
                body,
                [
                    Const::Int(-1, Primitive::Int),
                    Const::Int(7, Primitive::Int),
                ],
            );
        });
        let f = func::<extern "C" fn(i32) -> i32>(&program, "F");
        assert_eq!((f(-1), f(7), f(0)), (10, 20, 30));
        let program = jit_one_with("F(move a: char): int => 1;\n", "F", |body| {
            switch_on_parameter(body, [Const::Char('é'), Const::Char('a')]);
        });
        let f = func::<extern "C" fn(u32) -> i32>(&program, "F");
        assert_eq!((f(0xE9), f(0x61), f(0x62)), (10, 20, 30));
    }

    #[test]
    fn an_unreachable_block_is_compiled_but_not_run() {
        let program = jit_one_with("F(): int => 4;\n", "F", |body| {
            let span = body.span;
            body.blocks.push(Block {
                statements: Vec::new(),
                terminator: Terminator {
                    kind: TerminatorKind::Unreachable,
                    span,
                },
            });
        });
        assert_eq!(func::<extern "C" fn() -> i32>(&program, "F")(), 4);
    }

    #[test]
    #[should_panic(expected = "internal error generating code for top-level: a call to \
                               `Console.Write` of the standard library is not lowered until \
                               M1.15")]
    fn external_callees_wait_for_the_standard_library() {
        jit("Console.Write(1);\n", Profile::Debug);
    }
}
