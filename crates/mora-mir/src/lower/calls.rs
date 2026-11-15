//! Calls: of functions and methods, of local functions, and of the
//! standard library while it is not loaded. Arguments are evaluated in the
//! order they are written, after the receiver, and passed in the order of
//! the parameters (§7, §10.3). A receiver taken by `in`, `ref` or `mut` is
//! borrowed once the arguments are evaluated, so an argument may still
//! change it; a `move` receiver is a value taken before them (D-146).

use mora_hir::{
    Arg, BodyOwner, Captured, Expr, ExprId, Inference, ItemId, ParamMode, Ty, body,
    calls_diverging, infer, signature,
};
use mora_span::{Project, SourceFile, Span};

use super::consts::default_ty;
use super::expr::{Dest, constant};
use super::{Diverged, Lowered, Lowerer};
use crate::eval::{ConstKey, Evaluated, const_value};
use crate::{BorrowMode, Callee, LocalTy, Operand, Place, Rvalue, TerminatorKind};

impl Lowerer<'_> {
    /// The call `expr` of `callee` with `args`, its result going to `dest`.
    /// Returns the place the result went to.
    pub(super) fn call(
        &mut self,
        expr: ExprId,
        callee: ExprId,
        args: &[Arg],
        dest: Dest,
    ) -> Lowered<Option<Place>> {
        match self.owner.inference.callee(expr) {
            Some(mora_hir::Callee::Item(item)) => self.item_call(expr, item, callee, args, dest),
            Some(mora_hir::Callee::Local(local)) => self.local_call(expr, local, args, dest),
            None => {
                // A method of a value that never exists is never called:
                // its receiver does not return.
                if let Expr::Member { receiver, .. } = &self.owner.hir.exprs[callee]
                    && self.owner.inference.expr(callee) == Some(&Ty::Never)
                {
                    let what = "a value of the never type was made";
                    return self.operand(*receiver).map(|_| self.ice(expr, what));
                }
                let path = self.external_path(callee);
                let path = self.expect(path, expr, "the call has no callee");
                // A call that does not return has no value to store.
                let diverges = calls_diverging(self.owner.hir, self.owner.resolution, callee);
                let what = "the value of an external call is used";
                self.check(matches!(dest, Dest::Discard) || diverges, expr, what);
                self.external_call(expr, path, args, diverges)?;
                Ok(None)
            }
        }
    }

    /// The path of `callee` when it names a function of the standard
    /// library while the library is not loaded, as written.
    fn external_path(&self, callee: ExprId) -> Option<Vec<String>> {
        match &self.owner.hir.exprs[callee] {
            Expr::Path(path) if self.owner.resolution.is_external(callee) => {
                Some(path.segments.clone())
            }
            Expr::Member { receiver, name, .. } => {
                let mut path = self.external_path(*receiver)?;
                path.push(name.clone());
                Some(path)
            }
            _ => None,
        }
    }

    /// A call of a function of the standard library, whose arguments are
    /// passed as values and whose result is dropped. A function that never
    /// returns, as `diverges` says, ends the block (D-138).
    fn external_call(
        &mut self,
        expr: ExprId,
        path: Vec<String>,
        args: &[Arg],
        diverges: bool,
    ) -> Lowered<()> {
        let values = arg_values(args);
        let args = self.operands(&values)?;
        let span = self.span(expr);
        let call = |next| TerminatorKind::Call {
            callee: Callee::External(path),
            args,
            dest: None,
            next,
        };
        if diverges {
            self.builder.terminate_dead_end(call(None), span);
            return Err(Diverged);
        }
        let next = self.builder.new_block();
        self.builder.terminate(call(Some(next)), span);
        self.builder.switch_to(next);
        Ok(())
    }

    /// A call of the function or method `item` through `callee`. A method
    /// takes its receiver first; each omitted argument is its default value.
    fn item_call(
        &mut self,
        expr: ExprId,
        item: ItemId,
        callee: ExprId,
        args: &[Arg],
        dest: Dest,
    ) -> Lowered<Option<Place>> {
        let function = signature(self.owner.db, item).as_function();
        let function = self.expect(function, expr, "the callee is not a function");
        let values = arg_values(args);
        // The receiver is evaluated first. One taken by `in`, `ref` or `mut`
        // is borrowed once the arguments are, as a local function borrows
        // `this`; a `move` receiver is a value, taken before them (D-146).
        let receiver = match function.receiver {
            Some(mode) => Some((self.receiver(expr, callee, mode, &values)?, mode)),
            None => None,
        };
        let modes: Vec<ParamMode> = function.params.iter().map(|param| param.mode).collect();
        let passed = self.arguments(expr, &values, &modes)?;
        let mut arguments = Vec::with_capacity(passed.len());
        // The callee's body is read only for a default value, so an edit
        // inside it lowers again only the callers that use one.
        let owner = BodyOwner::Item(item.item);
        let db = self.owner.db;
        let mut callee_hir = None;
        for (index, operand) in passed.into_iter().enumerate() {
            arguments.push(match operand {
                Some(operand) => operand,
                None => {
                    let declared = *callee_hir.get_or_insert_with(|| Declared {
                        project: item.project,
                        file: item.file,
                        owner,
                        hir: body(db, item.file, owner),
                        inference: infer(db, item.project, item.file, owner),
                    });
                    self.default_argument(
                        expr,
                        declared,
                        declared.hir.params.get(index).copied(),
                        &declared.hir.defaults,
                        modes[index],
                    )
                }
            });
        }
        let mut operands = Vec::with_capacity(arguments.len() + 1);
        if let Some(((receiver, span), mode)) = receiver {
            operands.push(self.pass(receiver, mode, span));
        }
        operands.extend(arguments);
        Ok(self.finish_call(
            expr,
            Callee::Item(item),
            operands,
            function.ret.clone(),
            dest,
        ))
    }

    /// A call of the local function `local`: a borrow of `this` when it
    /// takes it, its arguments, then a borrow of each local of the bodies
    /// around it that it takes.
    fn local_call(
        &mut self,
        expr: ExprId,
        local: mora_hir::LocalId,
        args: &[Arg],
        dest: Dest,
    ) -> Lowered<Option<Place>> {
        let owner = self.owner;
        let what = "the callee is not a local function of the body";
        let (index, function) = self.expect(owner.function(local), expr, what);
        let def = function.def;
        let span = self.span(expr);
        let values = arg_values(args);
        let mut this_mode = None;
        let mut locals = Vec::with_capacity(function.captures.len());
        for &(captured, mode) in function.captures {
            match captured {
                Captured::This => this_mode = Some(mode),
                Captured::Local(local) => locals.push((local, mode)),
            }
        }
        let passed = self.arguments(expr, &values, &def.modes)?;
        let mut arguments = Vec::with_capacity(passed.len());
        for (index, operand) in passed.into_iter().enumerate() {
            arguments.push(match operand {
                Some(operand) => operand,
                None => self.default_argument(
                    expr,
                    Declared {
                        project: owner.project,
                        file: owner.file,
                        owner: owner.owner,
                        hir: owner.hir,
                        inference: owner.inference,
                    },
                    Some(def.params[index]),
                    &def.defaults,
                    def.modes[index],
                ),
            });
        }
        // `this` goes first, then the arguments, then the locals it takes;
        // `this` and the locals are borrowed once the arguments are
        // evaluated, so an argument may still change them.
        let mut operands = Vec::with_capacity(def.params.len() + function.captures.len());
        if let Some(mode) = this_mode {
            let this = Operand::Copy(self.this_place(expr));
            operands.push(self.pass(this, mode, span));
        }
        operands.extend(arguments);
        for (local, mode) in locals {
            let place = Operand::Copy(self.local_place(local));
            operands.push(self.pass(place, mode, span));
        }
        let ret = owner.resolution.return_type(def.ret);
        Ok(self.finish_call(expr, Callee::Local(index), operands, ret, dest))
    }

    /// The arguments `values` of `call`, evaluated in the order they are
    /// written, at the positions of the parameters they go to, of the modes
    /// `modes`; `None` where an argument is left out.
    fn arguments(
        &mut self,
        call: ExprId,
        values: &[ExprId],
        modes: &[ParamMode],
    ) -> Lowered<Vec<Option<Operand>>> {
        let bound = self.owner.inference.arguments(call);
        let bound = self.expect(bound, call, "the arguments of the call are not bound");
        debug_assert_eq!(
            bound.len(),
            values.len(),
            "every argument goes to a parameter"
        );
        let mut passed: Vec<Option<Operand>> = vec![None; modes.len()];
        for (index, (&value, &param)) in values.iter().zip(bound).enumerate() {
            passed[param] = Some(self.argument(value, modes[param], &values[index + 1..])?);
        }
        Ok(passed)
    }

    /// The receiver of the call `call` of a method through `callee`, which
    /// takes it by `mode`, before the arguments `later`, evaluated but not
    /// yet passed, with its span: the place of the value `callee` is a
    /// member of for `ref`, that value for `in` and `move`, or `this` for a
    /// method named alone. A `move` receiver is a value read here, as a
    /// `move` argument is, so the arguments cannot change it (D-146).
    fn receiver(
        &mut self,
        call: ExprId,
        callee: ExprId,
        mode: ParamMode,
        later: &[ExprId],
    ) -> Lowered<(Operand, Span)> {
        let hir = self.owner.hir;
        let (value, span) = match &hir.exprs[callee] {
            Expr::Member { receiver, .. } => {
                let span = self.span(*receiver);
                let value = match mode {
                    ParamMode::Ref => Operand::Copy(self.place(*receiver)?),
                    ParamMode::In | ParamMode::Move => self.operand(*receiver)?,
                };
                (value, span)
            }
            Expr::Path(_) => (Operand::Copy(self.this_place(callee)), self.span(callee)),
            _ => self.ice(call, "a method call without its receiver"),
        };
        if mode == ParamMode::Move {
            return Ok((self.stable(value, later, span), span));
        }
        Ok((value, span))
    }

    /// Ends the current block with the call of `callee` that `expr` makes,
    /// with `args`, and goes on in the block after it. The result, of type
    /// `ret`, goes to `dest`; returns the place it went to.
    pub(super) fn finish_call(
        &mut self,
        expr: ExprId,
        callee: Callee,
        args: Vec<Operand>,
        ret: Ty,
        dest: Dest,
    ) -> Option<Place> {
        let span = self.span(expr);
        let dest = match dest {
            Dest::Discard => None,
            Dest::Place(place) => Some(place),
            Dest::Temp => Some(Place::local(self.builder.temp(LocalTy::Value(ret), span))),
        };
        let next = self.builder.new_block();
        let call = TerminatorKind::Call {
            callee,
            args,
            dest: dest.clone(),
            next: Some(next),
        };
        self.builder.terminate(call, span);
        self.builder.switch_to(next);
        dest
    }

    /// The argument `value` for a parameter of mode `mode`, before the
    /// arguments `later`: a borrow for `in` and `ref`, a value for `move`
    /// (§10.3).
    fn argument(&mut self, value: ExprId, mode: ParamMode, later: &[ExprId]) -> Lowered<Operand> {
        let span = self.span(value);
        if mode == ParamMode::Ref {
            let place = self.place(value)?;
            return Ok(self.borrow(BorrowMode::Ref, place, span));
        }
        let operand = self.operand(value)?;
        let operand = self.stable(operand, later, span);
        Ok(self.pass(operand, mode, span))
    }

    /// `operand` passed to a parameter of mode `mode`: a borrow of the
    /// place that holds it for `in` and `ref`, a constant first stored in
    /// a temporary; itself for `move`.
    fn pass(&mut self, operand: Operand, mode: ParamMode, span: Span) -> Operand {
        let Some(mode) = BorrowMode::of(mode) else {
            return operand;
        };
        let place = self.operand_place(operand, span);
        self.borrow(mode, place, span)
    }

    /// A temporary holding a borrow of `place`.
    fn borrow(&mut self, mode: BorrowMode, place: Place, span: Span) -> Operand {
        let ty = self.builder.body().place_ty(&place, self.owner.db);
        let temp = self.builder.temp(LocalTy::Borrow(mode, ty), span);
        self.builder
            .assign(Place::local(temp), Rvalue::Borrow(mode, place), span);
        Operand::Copy(Place::local(temp))
    }

    /// The default value of the parameter `param`, declared in the body
    /// `declared` with the defaults `defaults`, which the call `call`
    /// leaves out, passed as mode `mode` says. A literal, or an operation
    /// on literals whose value inference records, is that constant,
    /// converted as inference records; any other default is the value
    /// constant evaluation computes, already of [`default_ty`].
    ///
    /// # Panics
    ///
    /// When the default has no value: a body whose call leaves one out
    /// is never lowered, since its evaluation reports an error or waits
    /// for a later milestone first.
    fn default_argument(
        &mut self,
        call: ExprId,
        declared: Declared<'_>,
        param: Option<mora_hir::LocalId>,
        defaults: &[(mora_hir::LocalId, ExprId)],
        mode: ParamMode,
    ) -> Operand {
        let default = param.and_then(|param| default_of(param, defaults));
        let what = "an argument without a default value is left out";
        let default = self.expect(default, call, what);
        let span = self.span(call);
        let inference = declared.inference;
        if let Some(value) = constant(declared.hir, inference, default) {
            let operand =
                self.converted(inference.conversion(default), Operand::Const(value), span);
            return self.pass(operand, mode, span);
        }
        let key = ConstKey::Default {
            file: declared.file,
            owner: declared.owner,
            default,
        };
        let Evaluated::Value(value) = const_value(self.owner.db, declared.project, key) else {
            self.ice(call, "a left out default value without a value");
        };
        let ty = default_ty(inference, default);
        let ty = self.expect(ty, call, "the default value has no type");
        let operand = self.value_operand(value, &ty, span);
        self.pass(operand, mode, span)
    }
}

/// The body that declares the default values of a callee: that of a
/// function or method, or the body around a local function, with what
/// constant evaluation names it by.
#[derive(Clone, Copy)]
struct Declared<'a> {
    /// The project of the body.
    project: Project,
    /// The file of the body.
    file: SourceFile,
    /// The owner of the body.
    owner: BodyOwner,
    /// The HIR body.
    hir: &'a mora_hir::Body,
    /// The types and records of the HIR body.
    inference: &'a Inference,
}

/// The default value of the parameter `param` among `defaults`, those of
/// the body or the local function that declares it.
pub(super) fn default_of(
    param: mora_hir::LocalId,
    defaults: &[(mora_hir::LocalId, ExprId)],
) -> Option<ExprId> {
    defaults
        .iter()
        .find(|&&(local, _)| local == param)
        .map(|&(_, default)| default)
}

/// The values of `args`, in the order they are written.
fn arg_values(args: &[Arg]) -> Vec<ExprId> {
    args.iter().map(|arg| arg.expr).collect()
}
