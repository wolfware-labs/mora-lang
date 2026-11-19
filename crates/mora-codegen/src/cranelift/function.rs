//! The lowering of one MIR body to a Cranelift function.

use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{
    self, AbiParam, ArgumentPurpose, InstBuilder, MemFlagsData, Signature, StackSlot,
    StackSlotData, StackSlotKind, TrapCode, Type, Value, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use mora_hir::Ty;
use mora_mir::{
    BlockId, Body, Callee, Const, LocalId, LocalTy, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, TerminatorKind,
};
use mora_span::Span;

use super::runtime::{Call, Env};
use crate::abi::{FnAbi, PassMode};
use crate::layout::{Layout, Layouts, Scalar, Shape};

/// The trap code of `Unreachable`, and of the end of a block that panics.
pub(super) const UNREACHABLE: TrapCode = TrapCode::unwrap_user(1);

/// Where a local lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Storage {
    /// Nowhere: its type is zero-sized.
    None,
    /// In a variable of the type.
    Variable(Type),
    /// In a stack slot.
    Slot {
        /// The size in bytes, a multiple of the pointer size; 0 for a
        /// zero-sized local whose address is taken.
        size: u32,
        /// The alignment in bytes.
        align: u32,
    },
}

/// Where each local of `body` lives, in the order of `Body::locals`: a
/// scalar whose address is never taken is a variable, a local whose address
/// is taken or any other local with a non-zero size is a stack slot, and a
/// zero-sized local whose address is never taken is nowhere.
pub(super) fn storage(body: &Body, layouts: &Layouts<'_>, pointer: Type) -> Vec<Storage> {
    let borrowed = borrowed_locals(body);
    body.locals
        .iter()
        .zip(borrowed)
        .map(|(decl, borrowed)| {
            let layout = layouts.of_local(&decl.ty);
            match layout.shape {
                _ if layout.size == 0 && !borrowed => Storage::None,
                Shape::Scalar(scalar) if !borrowed => {
                    Storage::Variable(scalar_type(scalar, pointer))
                }
                _ => Storage::Slot {
                    size: layouts.word_size(&layout),
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

/// The Cranelift signature of a function that passes its values as `abi`
/// says: a result too large for registers is written through a pointer that
/// comes first among the parameters.
pub(super) fn signature(abi: &FnAbi, call_conv: CallConv, pointer: Type) -> Signature {
    let mut signature = Signature::new(call_conv);
    match abi.ret {
        PassMode::Indirect => signature
            .params
            .push(AbiParam::special(pointer, ArgumentPurpose::StructReturn)),
        mode => signature.returns.extend(abi_params(mode, pointer)),
    }
    for &mode in &abi.params {
        signature.params.extend(abi_params(mode, pointer));
    }
    signature
}

/// How a value passed in `mode` crosses a call: nothing, a scalar, one
/// integer for each word, or a pointer. An integer narrower than 32 bits is
/// extended by its signedness and a `bool` is zero-extended, as the C
/// conventions of the targets expect.
fn abi_params(mode: PassMode, pointer: Type) -> Vec<AbiParam> {
    let scalar = match mode {
        PassMode::Ignore => return Vec::new(),
        PassMode::Direct(scalar) => scalar,
        PassMode::Words(count) => return vec![AbiParam::new(pointer); usize::from(count)],
        PassMode::Indirect => return vec![AbiParam::new(pointer)],
    };
    let param = AbiParam::new(scalar_type(scalar, pointer));
    vec![match scalar {
        Scalar::Int { bytes, signed } if bytes < 4 => {
            if signed {
                param.sext()
            } else {
                param.uext()
            }
        }
        Scalar::Bool => param.uext(),
        _ => param,
    }]
}

/// Lowers `body` into `func`, whose signature is already set from `abi`.
/// `env` holds what the code reaches outside the function: the function
/// each call terminator reaches, and the runtime.
///
/// # Panics
///
/// With an internal error for MIR that is not lowered until M1.12: a string.
pub(super) fn lower(
    func: &mut ir::Function,
    context: &mut FunctionBuilderContext,
    body: &Body,
    layouts: &Layouts<'_>,
    abi: &FnAbi,
    config: TargetFrontendConfig,
    env: &Env<'_>,
) {
    let pointer = config.pointer_type();
    let mut builder = FunctionBuilder::new(func, context);
    let (variables, slots) = storage(body, layouts, pointer)
        .into_iter()
        .enumerate()
        .map(|(local, storage)| match storage {
            Storage::None => (None, None),
            Storage::Variable(ty) => (Some(builder.declare_var(ty)), None),
            // The caller owns the memory of this local.
            Storage::Slot { .. } if in_caller_memory(abi, local) => (None, None),
            Storage::Slot { size, align } => (
                None,
                Some(builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size,
                    align.trailing_zeros() as u8,
                ))),
            ),
        })
        .unzip();
    let blocks: Vec<ir::Block> = body.blocks.iter().map(|_| builder.create_block()).collect();
    builder.append_block_params_for_function_params(blocks[BlockId::ENTRY.index()]);
    let mut lowering = Lowering {
        body,
        layouts,
        pointer,
        config,
        indirect: vec![None; body.locals.len()],
        builder,
        variables,
        slots,
        blocks,
        env,
        span: body.span,
    };
    for (index, block) in body.blocks.iter().enumerate() {
        lowering.builder.switch_to_block(lowering.blocks[index]);
        if index == BlockId::ENTRY.index() {
            lowering.parameters(abi);
        }
        for statement in &block.statements {
            lowering.statement(statement);
        }
        lowering.terminator(index, &block.terminator);
    }
    lowering.builder.seal_all_blocks();
    lowering.builder.finalize(config);
}

/// Whether the local at `index` of a body that passes its values as `abi` is
/// in memory the caller provides: the result, or a parameter, passed by
/// pointer.
fn in_caller_memory(abi: &FnAbi, index: usize) -> bool {
    let mode = match index {
        0 => Some(abi.ret),
        parameter => abi.params.get(parameter - 1).copied(),
    };
    mode == Some(PassMode::Indirect)
}

/// Panics with an internal error in `body`: `what` is not lowered until
/// `milestone`.
pub(super) fn fail(body: &Body, what: &str, milestone: &str) -> ! {
    panic!(
        "internal error generating code for {}: {what} is not lowered until {milestone}",
        body.name
    )
}

/// Panics with an internal error in `body`: the MIR breaks an invariant
/// that `what` describes, which no MIR lowering produces.
pub(super) fn broken(body: &Body, what: &str) -> ! {
    panic!(
        "internal error generating code for {}: invalid MIR: {what}",
        body.name
    )
}

/// Where a place is.
struct Located {
    /// Whether it is in the value the local borrows, not in the local.
    deref: bool,
    /// Its offset in bytes from the start of that value.
    offset: u32,
    /// The type of the value at it; `None` for the pointer of a borrow.
    ty: Option<Ty>,
    /// The layout of the value at it.
    layout: Layout,
}

/// What an operand reads.
#[derive(Clone, Copy)]
pub(super) enum Source<'o> {
    /// The value at a place.
    Place(&'o Place),
    /// A constant.
    Const(&'o Const),
}

/// The lowering of one body.
pub(super) struct Lowering<'a, 'f> {
    /// The body being lowered.
    pub(super) body: &'a Body,
    /// The layouts of its types.
    pub(super) layouts: &'a Layouts<'a>,
    /// The Cranelift type of a pointer.
    pub(super) pointer: Type,
    /// What the target's frontend needs to copy memory.
    pub(super) config: TargetFrontendConfig,
    /// The address of each local in memory the caller provides, once the
    /// entry block has read it.
    indirect: Vec<Option<Value>>,
    /// The builder of the function.
    pub(super) builder: FunctionBuilder<'f>,
    /// The variable of each local that lives in one.
    variables: Vec<Option<Variable>>,
    /// The stack slot of each local that lives in one.
    slots: Vec<Option<StackSlot>>,
    /// The Cranelift block of each MIR block.
    blocks: Vec<ir::Block>,
    /// What the code reaches outside the function.
    pub(super) env: &'a Env<'a>,
    /// The source of the statement being lowered.
    pub(super) span: Span,
}

impl Lowering<'_, '_> {
    /// Stores the parameters, which the entry block receives, in their
    /// locals, and takes the address of the result and of each parameter
    /// passed by pointer; `abi` passes them.
    fn parameters(&mut self, abi: &FnAbi) {
        let entry = self.blocks[BlockId::ENTRY.index()];
        let mut values = self.builder.block_params(entry).to_vec().into_iter();
        if abi.ret == PassMode::Indirect {
            self.indirect[LocalId::RETURN.index()] = values.next();
        }
        for (local, &mode) in self.body.param_ids().zip(&abi.params) {
            match mode {
                PassMode::Ignore => {}
                PassMode::Direct(_) => self.write(&Place::local(local), values.next()),
                PassMode::Words(count) => {
                    let words: Vec<Value> = values.by_ref().take(usize::from(count)).collect();
                    let address = self.place_address(&Place::local(local));
                    self.store_words(address, &words);
                }
                PassMode::Indirect => self.indirect[local.index()] = values.next(),
            }
        }
    }

    fn statement(&mut self, statement: &Statement) {
        self.span = statement.span;
        match &statement.kind {
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
            StatementKind::Assign(place, Rvalue::Use(operand)) => {
                let source = self.source(operand);
                let layout = self.place_layout(place);
                match layout.shape {
                    // A zero-sized aggregate has no value to copy.
                    Shape::Aggregate(_) if layout.size > 0 => {
                        self.assign_aggregate(place, source, &layout);
                    }
                    _ => {
                        let value = self.source_value(source);
                        self.write(place, value);
                    }
                }
            }
            StatementKind::Assign(place, Rvalue::Aggregate(_, operands)) => {
                self.build_aggregate(place, operands);
            }
            StatementKind::Assign(place, Rvalue::Borrow(_, borrowed)) => {
                let address = self.place_address(borrowed);
                self.write(place, Some(address));
            }
            StatementKind::Assign(place, Rvalue::Binary(op, left, right)) => {
                let value = self.binary(*op, left, right);
                self.write(place, Some(value));
            }
            StatementKind::Assign(place, Rvalue::Unary(op, operand)) => {
                let value = self.unary(*op, operand);
                self.write(place, Some(value));
            }
            StatementKind::Assign(place, Rvalue::Cast(_, operand, ty)) => {
                let value = self.cast(operand, ty);
                self.write(place, Some(value));
            }
            other => fail(self.body, &format!("the statement `{other:?}`"), "M1.12"),
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
                let result = self.result();
                self.builder.ins().return_(&result);
            }
            TerminatorKind::Call {
                callee,
                args,
                dest,
                next,
            } => self.call(
                self.env.calls[index].as_ref(),
                callee,
                args,
                dest.as_ref(),
                *next,
            ),
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
        let source = self.source(discr);
        let Shape::Scalar(Scalar::Int { .. } | Scalar::Bool | Scalar::Char) =
            self.source_layout(source).shape
        else {
            broken(
                self.body,
                &format!("a switch on `{discr:?}`, which is not a bool, an integer or a char"),
            )
        };
        // Such a scalar is never zero-sized.
        let value = self
            .source_value(source)
            .unwrap_or_else(|| broken(self.body, &format!("a switch on `{discr:?}`")));
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
                other => broken(
                    self.body,
                    &format!(
                        "a switch case on `{other:?}`, which is not a bool, an integer or a char"
                    ),
                ),
            };
            switch.set_entry(entry & mask, self.blocks[target.index()]);
        }
        switch.emit(&mut self.builder, value, self.blocks[otherwise.index()]);
    }

    /// The values the function returns in registers: the scalar in `_0`, or
    /// its words; none when the result is zero-sized or written through the
    /// pointer the caller provides.
    fn result(&mut self) -> Vec<Value> {
        let result = Place::local(LocalId::RETURN);
        match self.pass_mode(&self.place_layout(&result)) {
            PassMode::Ignore | PassMode::Indirect => Vec::new(),
            PassMode::Direct(_) => self.read(&result).into_iter().collect(),
            PassMode::Words(count) => {
                let address = self.place_address(&result);
                self.load_words(address, count)
            }
        }
    }

    /// Calls `target`, the function `callee` names, writes its result to
    /// `dest` once it returns, and continues in `next`; traps when the call
    /// returns although it has no `next`. Each argument is passed as its
    /// layout says (`PassMode`).
    ///
    /// # Panics
    ///
    /// With an internal error for a function of the standard library, which
    /// M1.15 brings, or a function outside the program.
    fn call(
        &mut self,
        target: Option<&Call>,
        callee: &Callee,
        args: &[Operand],
        dest: Option<&Place>,
        next: Option<BlockId>,
    ) {
        let Some(target) = target else {
            match callee {
                Callee::External(path) => fail(
                    self.body,
                    &format!("a call to `{}` of the standard library", path.join(".")),
                    "M1.15",
                ),
                other => broken(
                    self.body,
                    &format!("the callee `{other:?}` is not in the program"),
                ),
            }
        };
        let mode = self.pass_mode(&target.result);
        let mut values = Vec::new();
        // The result of a large value is written to a slot of the caller's.
        let sret = (mode == PassMode::Indirect).then(|| self.temporary(&target.result));
        values.extend(sret);
        for arg in args {
            self.argument(arg, &mut values);
        }
        let call = self.builder.ins().call(target.func, &values);
        let results = self.builder.inst_results(call).to_vec();
        if let Some(dest) = dest {
            match mode {
                PassMode::Ignore => {}
                PassMode::Direct(_) => self.write(dest, results.first().copied()),
                PassMode::Words(_) => {
                    let from = self.temporary(&target.result);
                    self.store_words(from, &results);
                    self.copy_to_place(dest, from, &target.result);
                }
                PassMode::Indirect => {
                    if let Some(from) = sret {
                        self.copy_to_place(dest, from, &target.result);
                    }
                }
            }
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

    /// Adds to `values` what passes `arg` to a call: nothing for a
    /// zero-sized value, the scalar, the words of a copy of the value, or
    /// the address of a copy the callee owns.
    fn argument(&mut self, arg: &Operand, values: &mut Vec<Value>) {
        let source = self.source(arg);
        let layout = self.source_layout(source);
        match self.pass_mode(&layout) {
            PassMode::Ignore => {}
            PassMode::Direct(_) => values.extend(self.source_value(source)),
            PassMode::Words(count) => {
                let copy = self.temporary(&layout);
                self.store_source(copy, source);
                values.extend(self.load_words(copy, count));
            }
            PassMode::Indirect => {
                let copy = self.temporary(&layout);
                self.store_source(copy, source);
                values.push(copy);
            }
        }
    }

    /// What `operand` reads.
    ///
    /// # Panics
    ///
    /// With an internal error for the read of another constant, which only
    /// a body lowered for constant evaluation holds.
    pub(super) fn source<'o>(&self, operand: &'o Operand) -> Source<'o> {
        let place = match operand {
            Operand::Copy(place) | Operand::Move(place) => Some(place),
            _ => None,
        };
        let constant = match operand {
            Operand::Const(constant) => Some(constant),
            _ => None,
        };
        if let Some(place) = place {
            return Source::Place(place);
        }
        Source::Const(constant.unwrap_or_else(|| broken(self.body, "a read of another constant")))
    }

    /// The type of the value `source` reads; `None` for the pointer of a
    /// borrow.
    pub(super) fn source_ty(&self, source: Source<'_>) -> Option<Ty> {
        match source {
            Source::Place(place) => self.locate(place).ty,
            Source::Const(constant) => Some(constant.ty()),
        }
    }

    /// The layout of the value `source` reads.
    pub(super) fn source_layout(&self, source: Source<'_>) -> Layout {
        match source {
            Source::Place(place) => self.place_layout(place),
            Source::Const(constant) => self.layouts.of(&constant.ty()),
        }
    }

    /// The value `source` reads; `None` for a zero-sized one.
    pub(super) fn source_value(&mut self, source: Source<'_>) -> Option<Value> {
        match source {
            Source::Place(place) => self.read(place),
            Source::Const(constant) => self.constant(constant),
        }
    }

    /// Where `place` is: the layout and type of its value, and its offset
    /// from the start of the local, or of the value the local borrows.
    ///
    /// # Panics
    ///
    /// With an internal error for a place MIR validation rules out: a
    /// dereference after a field, or a field the type does not have.
    fn locate(&self, place: &Place) -> Located {
        let local = &self.body.locals[place.local.index()].ty;
        let (deref, fields) = match place.projections.as_slice() {
            [Projection::Deref, fields @ ..] => (true, fields),
            fields => (false, fields),
        };
        // The type of the value: `None` for the pointer of a borrow.
        let mut ty = match local {
            LocalTy::Borrow(..) if !deref => None,
            local => Some(local.ty().clone()),
        };
        let mut offset = 0;
        for projection in fields {
            let Projection::Field(index) = projection else {
                broken(self.body, &format!("a dereference inside `{place:?}`"));
            };
            let field = ty.and_then(|ty| self.layouts.field(&ty, *index));
            let Some((at, field)) = field else {
                broken(
                    self.body,
                    &format!("a field that `{place:?}` does not have"),
                );
            };
            offset += at;
            ty = Some(field);
        }
        let layout = match &ty {
            Some(ty) => self.layouts.of(ty),
            None => self.layouts.of_local(local),
        };
        Located {
            deref,
            offset,
            ty,
            layout,
        }
    }

    /// The layout of the value at `place`.
    pub(super) fn place_layout(&self, place: &Place) -> Layout {
        self.locate(place).layout
    }

    /// How a value of `layout` is passed.
    pub(super) fn pass_mode(&self, layout: &Layout) -> PassMode {
        PassMode::of(layout, self.layouts.pointer_bytes())
    }

    /// The Cranelift type of a value of `layout`; `None` for a zero-sized
    /// one.
    fn value_type(&self, layout: &Layout) -> Option<Type> {
        match layout.shape {
            _ if layout.size == 0 => None,
            Shape::Scalar(scalar) => Some(scalar_type(scalar, self.pointer)),
            ref shape => fail(self.body, &format!("a value of shape {shape:?}"), "M1.12"),
        }
    }

    /// The variable of `place`: the whole of a local that has one.
    fn variable(&self, place: &Place) -> Option<Variable> {
        match place.projections.as_slice() {
            [] => self.variables[place.local.index()],
            _ => None,
        }
    }

    /// The value at `place`; `None` for a zero-sized one.
    fn read(&mut self, place: &Place) -> Option<Value> {
        let ty = self.value_type(&self.place_layout(place))?;
        if let Some(variable) = self.variable(place) {
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
        if let Some(variable) = self.variable(place) {
            self.builder.def_var(variable, value);
            return;
        }
        let address = self.place_address(place);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, address, 0);
    }

    /// The address of `place`, which is in memory: a local in a stack slot
    /// or in memory the caller provides, or the value a borrow refers to,
    /// and the offset of a field in it.
    pub(super) fn place_address(&mut self, place: &Place) -> Value {
        let located = self.locate(place);
        let base = if located.deref {
            // A borrow is a pointer, never zero-sized.
            let pointer = self.read(&Place::local(place.local));
            pointer.unwrap_or_else(|| broken(self.body, "a zero-sized dereference"))
        } else {
            // A local whose address is taken, or that is in no variable and
            // has a value, has a slot (`storage`), unless the caller
            // provides its memory.
            match (
                self.slots[place.local.index()],
                self.indirect[place.local.index()],
            ) {
                (Some(slot), _) => self.builder.ins().stack_addr(self.pointer, slot, 0),
                (None, Some(address)) => address,
                (None, None) => broken(self.body, "a local with no slot"),
            }
        };
        self.offset(base, located.offset)
    }

    /// `address` moved on by `offset` bytes.
    pub(super) fn offset(&mut self, address: Value, offset: u32) -> Value {
        match offset {
            0 => address,
            offset => self.builder.ins().iadd_imm_u(address, i64::from(offset)),
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
            Const::String(_) => fail(self.body, "a string constant", "M1.12"),
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
pub(in crate::cranelift) mod tests {
    use cranelift_codegen::ir::{
        ArgumentExtension, ExtFuncData, ExternalName, Function, GlobalValueData, UserExternalName,
        UserFuncName,
    };
    use cranelift_jit::JITModule;
    use mora_hir::BodyOwner;
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_hir::{Primitive, Ty};
    use mora_mir::{Block, BorrowMode, LocalDecl, LocalTy};
    use mora_target::Target;

    use super::*;
    use crate::Profile;
    use crate::abi::FnAbi;
    use crate::cranelift::runtime::RuntimeFn;
    use crate::cranelift::{define_functions, isa, jit_builder};
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
    pub(in crate::cranelift) fn jit_one(text: &str, name: &str) -> JitProgram {
        jit_one_in(text, name, Profile::Debug)
    }

    /// The function `name` of the app of `text`, alone, compiled by the JIT
    /// for `profile`.
    pub(in crate::cranelift) fn jit_one_in(text: &str, name: &str, profile: Profile) -> JitProgram {
        jit_one_with_in(text, name, profile, |_| {})
    }

    /// The function `name` of the app of `text`, alone, compiled by the JIT
    /// after `edit` changes its MIR.
    pub(in crate::cranelift) fn jit_one_with(
        text: &str,
        name: &str,
        edit: impl FnOnce(&mut Body),
    ) -> JitProgram {
        jit_one_with_in(text, name, Profile::Debug, edit)
    }

    /// The function `name` of the app of `text`, alone, compiled by the JIT
    /// for `profile` after `edit` changes its MIR.
    pub(in crate::cranelift) fn jit_one_with_in(
        text: &str,
        name: &str,
        profile: Profile,
        edit: impl FnOnce(&mut Body),
    ) -> JitProgram {
        let db = TestDb::default();
        let project = app_of(&db, text);
        let file = file_named(&db, project, "src/Program.mr");
        let mut body = body_of(&db, text, Some(name));
        edit(&mut body);
        let program = Program::from_body(&db, project, file, &body);
        let target = Target::host().unwrap();
        let isa = isa(&target, profile, false).unwrap();
        let mut jit = JitProgram::new(JITModule::new(jit_builder(isa)));
        let layouts = Layouts::new(&db, &target);
        define_functions(jit.module_mut(), &program, &layouts, profile).unwrap();
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
        lowered_in(text, name, Profile::Debug, edit)
    }

    /// The Cranelift IR of the function `name` of the app of `text` for
    /// `profile`, after `edit` changes its MIR. Every call in it calls the
    /// function itself, and every runtime function and data object is a
    /// symbol of its own.
    pub(in crate::cranelift) fn lowered_in(
        text: &str,
        name: &str,
        profile: Profile,
        edit: impl FnOnce(&mut Body),
    ) -> String {
        let db = TestDb::default();
        let project = app_of(&db, text);
        let file = file_named(&db, project, "src/Program.mr");
        let mut body = body_of(&db, text, Some(name));
        edit(&mut body);
        let target = Target::builtin("x86_64-linux").unwrap();
        let layouts = Layouts::new(&db, &target);
        let isa = isa(&target, profile, false).unwrap();
        let abi = FnAbi::of(&body, &layouts);
        let mut func = Function::with_name_signature(
            UserFuncName::user(0, 0),
            signature(&abi, isa.default_call_conv(), isa.pointer_type()),
        );
        let import = |func: &mut Function, signature: Signature, index: u32| {
            let signature = func.import_signature(signature);
            let name = func.declare_imported_user_function(UserExternalName::new(0, index));
            func.import_function(ExtFuncData {
                name: ExternalName::user(name),
                signature,
                colocated: true,
                patchable: false,
            })
        };
        let result = layouts.of_local(&body.locals[LocalId::RETURN.index()].ty);
        let calls: Vec<Option<Call>> = body
            .blocks
            .iter()
            .map(|block| {
                matches!(block.terminator.kind, TerminatorKind::Call { .. }).then(|| {
                    let signature = func.signature.clone();
                    Call {
                        func: import(&mut func, signature, 0),
                        result: result.clone(),
                    }
                })
            })
            .collect();
        let pointer = isa.pointer_type();
        let functions = (1..)
            .zip(RuntimeFn::ALL)
            .map(|(index, function)| {
                let signature = function.signature(pointer, isa.default_call_conv());
                import(&mut func, signature, index)
            })
            .collect();
        let mut data = |index: u32| {
            let name = func.declare_imported_user_function(UserExternalName::new(1, index));
            func.create_global_value(GlobalValueData::Symbol {
                name: ExternalName::user(name),
                offset: 0.into(),
                colocated: true,
                tls: false,
            })
        };
        let (zero_message, path) = (data(0), data(1));
        let lines = mora_span::line_index(&db, file);
        let env = Env {
            profile,
            calls,
            functions,
            zero_message,
            path,
            path_len: "src/Program.mr".len(),
            file,
            lines,
            db: &db,
        };
        lower(
            &mut func,
            &mut FunctionBuilderContext::new(),
            &body,
            &layouts,
            &abi,
            isa.frontend_config(),
            &env,
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
                // F calls G first; the IR has no trap when it does not.
                if let TerminatorKind::Call { next, .. } =
                    &mut body.blocks[BlockId::ENTRY.index()].terminator.kind
                {
                    *next = None;
                }
            },
        );
        let call = text.find("call fn0").expect(&text);
        let trap = text.find("trap user1").expect(&text);
        assert!(call < trap, "{text}");
    }

    #[test]
    fn narrow_integers_and_bools_are_extended() {
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
        let signature = signature(&abi, CallConv::SystemV, types::I64);
        let extensions: Vec<ArgumentExtension> = signature
            .params
            .iter()
            .map(|param| param.extension)
            .collect();
        use ArgumentExtension as Ext;
        assert_eq!(
            extensions,
            [
                Ext::Sext,
                Ext::Sext,
                Ext::Uext,
                Ext::Uext,
                Ext::None,
                Ext::None,
                Ext::Uext,
                Ext::None
            ]
        );
        assert_eq!(signature.returns[0].extension, Ext::Sext);
    }

    #[test]
    fn a_switch_on_a_constant_takes_its_branch() {
        let program = jit_one_with(
            "Pick(c: bool): int { if c { return 1; } return 2; }\n",
            "Pick",
            |body| {
                // Pick switches first; it returns 1 for `true` when it does not.
                if let TerminatorKind::Switch { discr, .. } =
                    &mut body.blocks[BlockId::ENTRY.index()].terminator.kind
                {
                    *discr = Operand::Const(Const::Bool(false));
                }
            },
        );
        assert_eq!(
            func::<extern "C" fn(&bool) -> i32>(&program, "Pick")(&true),
            2
        );
    }

    #[test]
    fn a_borrowed_zero_sized_local_has_an_empty_slot() {
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
        assert!(text.contains("ss0 = explicit_slot 0"), "{text}");
        assert!(text.contains("= stack_addr.i64 ss0"), "{text}");
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: invalid MIR: \
                               a zero-sized dereference"
    )]
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
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: invalid MIR: a switch on \
                    `Copy(Place { local: LocalId(1), projections: [] })`, which is not a bool, \
                    an integer or a char"
    )]
    fn a_switch_on_a_real_is_an_internal_error() {
        lowered_with("F(move a: double): int => 1;\n", "F", |body| {
            let real = || Const::Real(0, Primitive::Double);
            switch_on_parameter(body, [real(), real()]);
        });
    }

    #[test]
    #[should_panic(
        expected = "internal error generating code for UsersApi.F: invalid MIR: a switch case \
                    on `Real(0, Double)`, which is not a bool, an integer or a char"
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
        expected = "internal error generating code for UsersApi.Outer: invalid MIR: the \
                    callee `Local(1)` is not in the program"
    )]
    fn a_callee_outside_the_program_is_an_internal_error() {
        jit_one(
            "Outer(): int { Inner(): int => 3; return Inner(); }\n",
            "Outer",
        );
    }

    #[test]
    #[should_panic(expected = "UsersApi.F: a string constant is not lowered until M1.12")]
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
