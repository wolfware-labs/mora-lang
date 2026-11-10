//! Expressions: values and places, operators, assignments, interpolated
//! strings and calls.

use mora_hir::{
    Arg, BinaryOp, BodyOwner, Constant, ConversionKind, Expr, ExprId, Inference, InterpPart,
    ItemId, Literal, ParamMode, Resolved, Signature, Target, Ty, UnaryOp, body, infer,
    is_diverging, literal_constant, signature,
};
use mora_span::Span;
use mora_syntax::lexer::literal::{char_value, interp_text_value, string_value};

use super::Lowerer;
use crate::types::has_receiver;
use crate::{
    AggregateKind, BinOp, BorrowMode, Callee, CastKind, Const, InterpPiece, LocalTy, Operand,
    Place, Projection, Rvalue, TerminatorKind, UnOp,
};

/// Where the value of a call goes.
pub(super) enum Dest {
    /// Nowhere: the call is an expression statement.
    Discard,
    /// Into the place.
    Place(Place),
    /// Into a new temporary.
    Temp,
}

impl Lowerer<'_> {
    /// `expr` as an expression statement: its effects, its value dropped.
    pub(super) fn effect(&mut self, expr: ExprId) {
        let hir = self.hir;
        match &hir.exprs[expr] {
            Expr::Call { callee, args } => {
                self.call(expr, *callee, args, Dest::Discard);
            }
            Expr::Assign { op, lhs, rhs } => self.assign(expr, *op, *lhs, *rhs),
            Expr::Postfix {
                increment,
                expr: target,
            } => {
                self.step(expr, *target, *increment);
            }
            // Anything else, a prefix step among them, is evaluated and its
            // value dropped.
            _ => {
                self.operand(expr);
            }
        }
    }

    /// Stores the value of `expr`, converted as inference records, in
    /// `dest`.
    pub(super) fn expr_into(&mut self, expr: ExprId, dest: Place) {
        let hir = self.hir;
        if let Expr::Call { callee, args } = &hir.exprs[expr]
            && self.inference.conversion(expr).is_none()
        {
            self.call(expr, *callee, args, Dest::Place(dest));
            return;
        }
        let value = self.converted_rvalue(expr);
        self.builder.assign(dest, value, self.span(expr));
    }

    /// The computation of the value of `expr`, converted as inference
    /// records.
    fn converted_rvalue(&mut self, expr: ExprId) -> Rvalue {
        match self.inference.conversion(expr) {
            Some(conversion) => {
                let value = self.operand_unconverted(expr);
                Rvalue::Cast(cast_kind(conversion.kind), value, conversion.to.clone())
            }
            None => self.rvalue(expr),
        }
    }

    /// The value of `expr`, converted as inference records.
    pub(super) fn operand(&mut self, expr: ExprId) -> Operand {
        let value = self.operand_unconverted(expr);
        match self.inference.conversion(expr) {
            Some(conversion) => self.cast_to_temp(
                cast_kind(conversion.kind),
                value,
                conversion.to.clone(),
                self.span(expr),
            ),
            None => value,
        }
    }

    /// The value of `expr` before any conversion: a constant, a place that
    /// holds it, or a temporary that it is stored in.
    fn operand_unconverted(&mut self, expr: ExprId) -> Operand {
        if let Some(constant) = self.constant(expr) {
            return Operand::Const(constant);
        }
        let hir = self.hir;
        match &hir.exprs[expr] {
            Expr::Local(local) => Operand::Copy(self.local_place(*local)),
            Expr::Unary {
                op: op @ (UnaryOp::PreIncrement | UnaryOp::PreDecrement),
                expr: target,
            } => Operand::Copy(self.step(expr, *target, *op == UnaryOp::PreIncrement)),
            Expr::Postfix {
                increment,
                expr: target,
            } => self.postfix(expr, *target, *increment),
            Expr::Call { callee, args } => match self.call(expr, *callee, args, Dest::Temp) {
                Some(place) => Operand::Copy(place),
                None => self.ice(expr, "the call has no result"),
            },
            _ => {
                let ty = self.ty(expr);
                let value = self.rvalue(expr);
                let span = self.span(expr);
                let temp = self.builder.temp(LocalTy::Value(ty), span);
                self.builder.assign(Place::local(temp), value, span);
                Operand::Copy(Place::local(temp))
            }
        }
    }

    /// The computation of the value of `expr`, before any conversion.
    fn rvalue(&mut self, expr: ExprId) -> Rvalue {
        if let Some(constant) = self.constant(expr) {
            return Rvalue::Use(Operand::Const(constant));
        }
        let hir = self.hir;
        match &hir.exprs[expr] {
            Expr::Binary { op, lhs, rhs } => self.binary(expr, *op, *lhs, *rhs),
            Expr::Unary {
                op: op @ (UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot),
                expr: operand,
            } => {
                let op = match op {
                    UnaryOp::Neg => UnOp::Neg,
                    UnaryOp::Not => UnOp::Not,
                    _ => UnOp::BitNot,
                };
                Rvalue::Unary(op, self.operand(*operand))
            }
            // A written cast is the conversion inference records on its
            // operand.
            Expr::Cast { expr: inner, .. }
            | Expr::Unary {
                op: UnaryOp::Plus,
                expr: inner,
            } => self.converted_rvalue(*inner),
            Expr::Interpolated(parts) => self.interpolate(expr, parts),
            Expr::Tuple(elems) => {
                let values: Vec<ExprId> = elems.iter().map(|&(_, elem)| elem).collect();
                Rvalue::Aggregate(AggregateKind::Tuple, self.operands(&values))
            }
            Expr::Local(_)
            | Expr::Call { .. }
            | Expr::Postfix { .. }
            | Expr::Unary {
                op: UnaryOp::PreIncrement | UnaryOp::PreDecrement,
                ..
            } => Rvalue::Use(self.operand_unconverted(expr)),
            _ => self.ice(expr, "the expression is not lowered"),
        }
    }

    /// The type inference gives `expr`.
    fn ty(&self, expr: ExprId) -> Ty {
        match self.inference.expr(expr) {
            Some(ty) => ty.clone(),
            None => self.ice(expr, "the value has no type"),
        }
    }

    /// The constant `expr` stands for, if it is one.
    fn constant(&self, expr: ExprId) -> Option<Const> {
        constant(self.hir, self.inference, expr)
    }

    /// `value`, of the type it has, converted to `to` in a new temporary.
    fn cast_to_temp(&mut self, kind: CastKind, value: Operand, to: Ty, span: Span) -> Operand {
        let temp = self.builder.temp(LocalTy::Value(to.clone()), span);
        self.builder
            .assign(Place::local(temp), Rvalue::Cast(kind, value, to), span);
        Operand::Copy(Place::local(temp))
    }

    /// The type of the value of `operand`.
    fn operand_ty(&self, operand: &Operand) -> Ty {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => {
                self.builder.body().place_ty(place, self.db)
            }
            Operand::Const(constant) => constant.ty(),
        }
    }

    // Places.

    /// The place `expr` names.
    fn place(&mut self, expr: ExprId) -> Place {
        let hir = self.hir;
        match &hir.exprs[expr] {
            Expr::Local(local) => self.local_place(*local),
            _ => self.ice(expr, "the place is not lowered"),
        }
    }

    /// The place of the value of the HIR local `local`: the local itself,
    /// or what it borrows for a parameter of mode `in` or `ref`.
    fn local_place(&self, local: mora_hir::LocalId) -> Place {
        let id = self.local(local);
        match self.builder.body().local(id).ty {
            LocalTy::Value(_) => Place::local(id),
            LocalTy::Borrow(..) => Place {
                local: id,
                projections: vec![Projection::Deref],
            },
        }
    }

    // Operands in order.

    /// The values of `exprs`, evaluated from left to right.
    fn operands(&mut self, exprs: &[ExprId]) -> Vec<Operand> {
        let mut operands = Vec::with_capacity(exprs.len());
        for (index, &expr) in exprs.iter().enumerate() {
            let operand = self.operand(expr);
            operands.push(self.stable(operand, &exprs[index + 1..], self.span(expr)));
        }
        operands
    }

    /// `operand`, read now: a place that one of the expressions evaluated
    /// after it may change is copied to a temporary first. A temporary is
    /// assigned once and never changes.
    fn stable(&mut self, operand: Operand, later: &[ExprId], span: Span) -> Operand {
        match operand {
            Operand::Copy(place)
                if self.builder.body().local(place.local).source.is_some()
                    && later.iter().any(|&expr| self.has_effects(expr)) =>
            {
                let ty = self.builder.body().place_ty(&place, self.db);
                let temp = self.builder.temp(LocalTy::Value(ty), span);
                self.builder
                    .assign(Place::local(temp), Rvalue::Use(Operand::Copy(place)), span);
                Operand::Copy(Place::local(temp))
            }
            operand => operand,
        }
    }

    /// Whether evaluating `expr` may change a place: it calls, assigns or
    /// steps somewhere in it.
    fn has_effects(&self, expr: ExprId) -> bool {
        match &self.hir.exprs[expr] {
            Expr::Literal(_) | Expr::Local(_) => false,
            Expr::Unary {
                op: UnaryOp::Plus | UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot,
                expr,
            }
            | Expr::Cast { expr, .. } => self.has_effects(*expr),
            Expr::Binary { lhs, rhs, .. } => self.has_effects(*lhs) || self.has_effects(*rhs),
            _ => true,
        }
    }

    // Operators and assignments.

    fn binary(&mut self, expr: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Rvalue {
        let Some(op) = bin_op(op) else {
            self.ice(expr, "the operator is not lowered")
        };
        let left = self.operand(lhs);
        let left = self.stable(left, &[rhs], self.span(lhs));
        let right = self.operand(rhs);
        Rvalue::Binary(op, left, right)
    }

    /// `lhs = rhs`, or `lhs op= rhs`: `lhs = (T)(lhs op rhs)`, its place
    /// evaluated once, with the cast back to the type `T` of the place
    /// that inference records on the assignment (§7.12).
    fn assign(&mut self, expr: ExprId, op: Option<BinaryOp>, lhs: ExprId, rhs: ExprId) {
        let place = self.place(lhs);
        let Some(op) = op else {
            self.expr_into(rhs, place);
            return;
        };
        let Some(op) = bin_op(op) else {
            self.ice(expr, "the operator is not lowered")
        };
        // The conversion recorded on `lhs` applies to reading it.
        let read = Operand::Copy(place.clone());
        let read = match self.inference.conversion(lhs) {
            Some(conversion) => self.cast_to_temp(
                cast_kind(conversion.kind),
                read,
                conversion.to.clone(),
                self.span(lhs),
            ),
            None => read,
        };
        let result_ty = self.operand_ty(&read);
        let read = self.stable(read, &[rhs], self.span(lhs));
        let right = self.operand(rhs);
        let value = Rvalue::Binary(op, read, right);
        let span = self.span(expr);
        match self.inference.conversion(expr) {
            Some(back) => {
                let temp = self.builder.temp(LocalTy::Value(result_ty), span);
                self.builder.assign(Place::local(temp), value, span);
                let cast = Rvalue::Cast(
                    cast_kind(back.kind),
                    Operand::Copy(Place::local(temp)),
                    back.to.clone(),
                );
                self.builder.assign(place, cast, span);
            }
            None => self.builder.assign(place, value, span),
        }
    }

    /// `++` or `--` on the place `target` names: `target = target ± 1` in
    /// its own type. Returns the place.
    fn step(&mut self, expr: ExprId, target: ExprId, increment: bool) -> Place {
        let place = self.place(target);
        let one = match self.builder.body().place_ty(&place, self.db) {
            Ty::Primitive(primitive) => {
                literal_constant(&Literal::Int("1".to_owned()), false, primitive)
                    .map(|one| numeric(one, primitive))
            }
            _ => None,
        };
        let Some(one) = one else {
            self.ice(target, "a step of a value that is not a number")
        };
        let op = if increment { BinOp::Add } else { BinOp::Sub };
        let value = Rvalue::Binary(op, Operand::Copy(place.clone()), Operand::Const(one));
        self.builder.assign(place.clone(), value, self.span(expr));
        place
    }

    /// `target++` or `target--` whose value is used: the old value, kept
    /// in a temporary before the step.
    fn postfix(&mut self, expr: ExprId, target: ExprId, increment: bool) -> Operand {
        let place = self.place(target);
        let span = self.span(expr);
        let ty = self.builder.body().place_ty(&place, self.db);
        let old = self.builder.temp(LocalTy::Value(ty), span);
        self.builder
            .assign(Place::local(old), Rvalue::Use(Operand::Copy(place)), span);
        self.step(expr, target, increment);
        Operand::Copy(Place::local(old))
    }

    // Interpolated strings.

    /// An interpolated string as [`Rvalue::Interpolate`], which stands in
    /// until the standard library formats values.
    fn interpolate(&mut self, expr: ExprId, parts: &[InterpPart]) -> Rvalue {
        let text = self.file.text(self.db);
        let start = usize::from(self.map.expr(expr).start());
        let raw = text
            .get(start..)
            .is_some_and(|rest| rest.trim_start_matches('$').starts_with('`'));
        if raw {
            self.ice(expr, "a raw interpolated string is not lowered");
        }
        let mut values = Vec::new();
        for part in parts {
            if let InterpPart::Hole {
                value,
                alignment,
                format,
            } = part
            {
                if alignment.is_some() || format.is_some() {
                    self.ice(
                        *value,
                        "a hole with an alignment or a format is not lowered",
                    );
                }
                values.push(*value);
            }
        }
        let mut operands = self.operands(&values).into_iter();
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            pieces.push(match part {
                InterpPart::Text(text) => match interp_text_value(text) {
                    Some(value) => InterpPiece::Text(value),
                    None => self.ice(expr, "the text of the string has no value"),
                },
                InterpPart::Hole { .. } => match operands.next() {
                    Some(operand) => InterpPiece::Operand(operand),
                    None => self.ice(expr, "a hole without its value"),
                },
            });
        }
        Rvalue::Interpolate(pieces)
    }

    // Calls.

    /// The call `expr` of `callee` with `args`, its result going to `dest`.
    /// Returns the place the result went to.
    fn call(&mut self, expr: ExprId, callee: ExprId, args: &[Arg], dest: Dest) -> Option<Place> {
        match self.inference.callee(expr) {
            Some(mora_hir::Callee::Item(item)) => self.item_call(expr, item, args, dest),
            Some(mora_hir::Callee::Local(_)) => {
                self.ice(expr, "a call of a local function is not lowered")
            }
            None => {
                let Some(path) = self.external_path(callee) else {
                    self.ice(expr, "the call has no callee")
                };
                if !matches!(dest, Dest::Discard) {
                    self.reject_external(expr);
                }
                self.external_call(expr, path, args);
                None
            }
        }
    }

    /// Panics when `expr` is a call of a function of the standard library
    /// while the library is not loaded: such a call has no result.
    pub(super) fn reject_external_value(&self, expr: ExprId) {
        if let Expr::Call { callee, .. } = &self.hir.exprs[expr]
            && self.inference.callee(expr).is_none()
            && self.external_path(*callee).is_some()
        {
            self.reject_external(expr);
        }
    }

    fn reject_external(&self, expr: ExprId) -> ! {
        self.ice(expr, "the value of an external call is used")
    }

    /// The path of `callee` when it names a function of the standard
    /// library while the library is not loaded, as written.
    fn external_path(&self, callee: ExprId) -> Option<Vec<String>> {
        match &self.hir.exprs[callee] {
            Expr::Path(path)
                if self
                    .resolution
                    .paths
                    .get(&callee)
                    .is_some_and(|resolution| {
                        matches!(resolution.target, Target::Item(Resolved::External))
                    }) =>
            {
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
    /// returns ends the block (D-138).
    fn external_call(&mut self, expr: ExprId, path: Vec<String>, args: &[Arg]) {
        let values: Vec<ExprId> = args.iter().map(|arg| arg.expr).collect();
        let args = self.operands(&values);
        let span = self.span(expr);
        let diverges = is_diverging(&path);
        let call = |next| TerminatorKind::Call {
            callee: Callee::External(path),
            args,
            dest: None,
            next,
        };
        if diverges {
            self.builder.terminate_dead_end(call(None), span);
        } else {
            let next = self.builder.new_block();
            self.builder.terminate(call(Some(next)), span);
            self.builder.switch_to(next);
        }
    }

    /// A call of the function `item`. The arguments are evaluated in the
    /// order they are written and passed in the order of the parameters,
    /// each omitted one as its default value.
    fn item_call(&mut self, expr: ExprId, item: ItemId, args: &[Arg], dest: Dest) -> Option<Place> {
        let Signature::Function(function) = signature(self.db, item) else {
            self.ice(expr, "the callee is not a function")
        };
        if has_receiver(self.db, item) {
            self.ice(expr, "a method call is not lowered");
        }
        let Some(bound) = self.inference.arguments(expr) else {
            self.ice(expr, "the arguments of the call are not bound")
        };
        let values: Vec<ExprId> = args.iter().map(|arg| arg.expr).collect();
        let mut passed: Vec<Option<Operand>> = vec![None; function.params.len()];
        for (index, (&value, &param)) in values.iter().zip(bound).enumerate() {
            let mode = function.params[param].mode;
            passed[param] = Some(self.argument(value, mode, &values[index + 1..]));
        }
        let span = self.span(expr);
        let mut operands = Vec::with_capacity(passed.len());
        for (index, operand) in passed.into_iter().enumerate() {
            operands.push(match operand {
                Some(operand) => operand,
                None => self.default_argument(expr, item, index, function.params[index].mode),
            });
        }
        let dest = match dest {
            Dest::Discard => None,
            Dest::Place(place) => Some(place),
            Dest::Temp => {
                let temp = self
                    .builder
                    .temp(LocalTy::Value(function.ret.clone()), span);
                Some(Place::local(temp))
            }
        };
        let next = self.builder.new_block();
        let call = TerminatorKind::Call {
            callee: Callee::Item(item),
            args: operands,
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
    fn argument(&mut self, value: ExprId, mode: ParamMode, later: &[ExprId]) -> Operand {
        let span = self.span(value);
        if mode == ParamMode::Ref {
            let place = self.place(value);
            return self.borrow(BorrowMode::Ref, place, span);
        }
        let operand = self.operand(value);
        let operand = self.stable(operand, later, span);
        self.pass(operand, mode, span)
    }

    /// `operand` passed to a parameter of mode `mode`: a borrow of the
    /// place that holds it for `in` and `ref`, a constant first stored in
    /// a temporary; itself for `move`.
    fn pass(&mut self, operand: Operand, mode: ParamMode, span: Span) -> Operand {
        let mode = match mode {
            ParamMode::Move => return operand,
            ParamMode::In => BorrowMode::In,
            ParamMode::Ref => BorrowMode::Ref,
        };
        let place = match operand {
            Operand::Copy(place) | Operand::Move(place) => place,
            Operand::Const(constant) => {
                let temp = self.builder.temp(LocalTy::Value(constant.ty()), span);
                self.builder.assign(
                    Place::local(temp),
                    Rvalue::Use(Operand::Const(constant)),
                    span,
                );
                Place::local(temp)
            }
        };
        self.borrow(mode, place, span)
    }

    /// A temporary holding a borrow of `place`.
    fn borrow(&mut self, mode: BorrowMode, place: Place, span: Span) -> Operand {
        let ty = self.builder.body().place_ty(&place, self.db);
        let temp = self.builder.temp(LocalTy::Borrow(mode, ty), span);
        self.builder
            .assign(Place::local(temp), Rvalue::Borrow(mode, place), span);
        Operand::Copy(Place::local(temp))
    }

    /// The default value of the parameter `index` of `item`, which the
    /// call `call` leaves out, passed as mode `mode` says. It is a
    /// constant: a literal, or an operation on literals whose value
    /// inference records, in the callee's body. Any other default needs
    /// constant evaluation.
    fn default_argument(
        &mut self,
        call: ExprId,
        item: ItemId,
        index: usize,
        mode: ParamMode,
    ) -> Operand {
        let owner = BodyOwner::Item(item.item);
        let callee = body(self.db, item.file, owner);
        let inference = infer(self.db, item.project, item.file, owner);
        let default = callee.params.get(index).and_then(|&param| {
            callee
                .defaults
                .iter()
                .find(|&&(local, _)| local == param)
                .map(|&(_, default)| default)
        });
        let Some(default) = default else {
            self.ice(call, "an argument without a default value is left out")
        };
        let Some(value) = constant(callee, inference, default) else {
            self.ice(
                call,
                "a default value that is not a literal needs constant evaluation (M1.11)",
            )
        };
        let span = self.span(call);
        let operand = match inference.conversion(default) {
            Some(conversion) => self.cast_to_temp(
                cast_kind(conversion.kind),
                Operand::Const(value),
                conversion.to.clone(),
                span,
            ),
            None => Operand::Const(value),
        };
        self.pass(operand, mode, span)
    }
}

/// The constant `expr` of `hir` stands for: a literal, a negated number
/// literal, or an operation on literals whose value inference records.
fn constant(hir: &mora_hir::Body, inference: &Inference, expr: ExprId) -> Option<Const> {
    let ty = inference.expr(expr)?;
    if let (Some(value), Ty::Primitive(primitive)) = (inference.constant(expr), ty) {
        return Some(numeric(value, *primitive));
    }
    match &hir.exprs[expr] {
        Expr::Literal(literal) => literal_const(literal, false, ty),
        // `-` before a number literal belongs to it (§3.5), unless the
        // literal is converted first, as `-1u` is.
        Expr::Unary {
            op: UnaryOp::Neg,
            expr: operand,
        } if inference.conversion(*operand).is_none() => match &hir.exprs[*operand] {
            Expr::Literal(literal @ (Literal::Int(_) | Literal::Real(_))) => {
                literal_const(literal, true, ty)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The value of `literal`, of the type `ty` inference gave it.
fn literal_const(literal: &Literal, negated: bool, ty: &Ty) -> Option<Const> {
    match (literal, ty) {
        (Literal::Bool(value), _) => Some(Const::Bool(*value)),
        (Literal::Char(text), _) => char_value(text).map(Const::Char),
        (Literal::String(text), _) => string_value(text).map(|(value, _)| Const::String(value)),
        (Literal::Int(_) | Literal::Real(_), Ty::Primitive(primitive)) => {
            literal_constant(literal, negated, *primitive).map(|value| numeric(value, *primitive))
        }
        _ => None,
    }
}

/// A number of the primitive type as a MIR constant.
fn numeric(value: Constant, primitive: mora_hir::Primitive) -> Const {
    match value {
        Constant::Int(value) => Const::Int(value, primitive),
        Constant::Real(bits) => Const::Real(bits, primitive),
    }
}

/// The MIR operator of `op`; `None` for `&&`, `||` and `??`, which are not
/// operations on two values.
fn bin_op(op: BinaryOp) -> Option<BinOp> {
    use BinaryOp as B;
    Some(match op {
        B::Add => BinOp::Add,
        B::Sub => BinOp::Sub,
        B::Mul => BinOp::Mul,
        B::Div => BinOp::Div,
        B::Rem => BinOp::Rem,
        B::WrappingAdd => BinOp::WrappingAdd,
        B::WrappingSub => BinOp::WrappingSub,
        B::WrappingMul => BinOp::WrappingMul,
        B::Shl => BinOp::Shl,
        B::Shr => BinOp::Shr,
        B::UShr => BinOp::UShr,
        B::BitAnd => BinOp::BitAnd,
        B::BitOr => BinOp::BitOr,
        B::BitXor => BinOp::BitXor,
        B::Eq => BinOp::Eq,
        B::Ne => BinOp::Ne,
        B::Lt => BinOp::Lt,
        B::Le => BinOp::Le,
        B::Gt => BinOp::Gt,
        B::Ge => BinOp::Ge,
        B::And | B::Or | B::Coalesce => return None,
    })
}

/// The cast that applies a conversion of `kind`.
fn cast_kind(kind: ConversionKind) -> CastKind {
    match kind {
        ConversionKind::Implicit => CastKind::Implicit,
        ConversionKind::Numeric => CastKind::Numeric,
    }
}
