//! Expressions: values and places, operators, assignments, interpolated
//! strings and calls.

use mora_hir::{
    BinaryOp, Constant, Conversion, ConversionKind, Expr, ExprId, Inference, InterpPart, Literal,
    Ty, UnaryOp, literal_constant,
};
use mora_span::Span;
use mora_syntax::lexer::literal::{char_value, string_value};

use super::{Lowered, Lowerer};
use crate::{
    AggregateKind, BinOp, CastKind, Const, InterpPiece, LocalId, LocalTy, Operand, Place,
    Projection, Rvalue, UnOp,
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
    fn effect(&mut self, expr: ExprId) -> Lowered<()> {
        let hir = self.owner.hir;
        match &hir.exprs[expr] {
            Expr::Call { callee, args } => {
                self.call(expr, *callee, args, Dest::Discard)?;
            }
            Expr::Assign { op, lhs, rhs } => self.assign(expr, *op, *lhs, *rhs)?,
            Expr::Postfix {
                increment,
                expr: target,
            } => {
                self.step(expr, *target, *increment)?;
            }
            // A conditional in statement position has statements as branches;
            // inference leaves it untyped when they assign.
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => self.statement_conditional(expr, *condition, *then, *r#else)?,
            // Anything else, a prefix step among them, is evaluated and its
            // value dropped.
            _ => {
                self.operand(expr)?;
            }
        }
        Ok(())
    }

    /// `expr` as a statement of its own: its effects, then the end of the
    /// temporaries it made.
    pub(super) fn expr_statement(&mut self, expr: ExprId) -> Lowered<()> {
        let mark = self.builder.temp_mark();
        let effect = self.effect(expr);
        self.builder.end_temps(mark, self.span(expr));
        effect
    }

    /// Stores the value of `expr`, converted as inference records, in
    /// `dest`.
    pub(super) fn expr_into(&mut self, expr: ExprId, dest: Place) -> Lowered<()> {
        let hir = self.owner.hir;
        if let Expr::Call { callee, args } = &hir.exprs[expr]
            && self.owner.inference.conversion(expr).is_none()
        {
            self.call(expr, *callee, args, Dest::Place(dest))?;
            return Ok(());
        }
        let value = self.converted_rvalue(expr)?;
        self.builder.assign(dest, value, self.span(expr));
        Ok(())
    }

    /// The computation of the value of `expr`, converted as inference
    /// records.
    fn converted_rvalue(&mut self, expr: ExprId) -> Lowered<Rvalue> {
        Ok(match self.owner.inference.conversion(expr) {
            Some(conversion) => cast(conversion, self.operand_unconverted(expr)?),
            None => self.rvalue(expr)?,
        })
    }

    /// The value of `expr`, converted as inference records.
    pub(super) fn operand(&mut self, expr: ExprId) -> Lowered<Operand> {
        let value = self.operand_unconverted(expr)?;
        let conversion = self.owner.inference.conversion(expr);
        Ok(self.converted(conversion, value, self.span(expr)))
    }

    /// The value of `expr` before any conversion: a constant, a place that
    /// holds it, or a temporary that it is stored in.
    fn operand_unconverted(&mut self, expr: ExprId) -> Lowered<Operand> {
        if let Some(constant) = self.constant(expr) {
            return Ok(Operand::Const(constant));
        }
        let hir = self.owner.hir;
        Ok(match &hir.exprs[expr] {
            Expr::Local(local) => Operand::Copy(self.local_place(*local)),
            Expr::Unary {
                op: op @ (UnaryOp::PreIncrement | UnaryOp::PreDecrement),
                expr: target,
            } => Operand::Copy(self.step(expr, *target, *op == UnaryOp::PreIncrement)?),
            Expr::Postfix {
                increment,
                expr: target,
            } => self.postfix(expr, *target, *increment)?,
            // A field, `this`, or what a field of `this` names.
            Expr::Member { .. } | Expr::Path(_) | Expr::This => Operand::Copy(self.place(expr)?),
            // The base comes first, and may not return.
            Expr::Index { base, .. } => {
                self.operand(*base)?;
                self.ice(expr, "an index is not lowered")
            }
            Expr::Call { callee, args } => match self.call(expr, *callee, args, Dest::Temp)? {
                Some(place) => Operand::Copy(place),
                None => self.ice(expr, "the call has no result"),
            },
            Expr::Binary {
                op: op @ (BinaryOp::And | BinaryOp::Or),
                lhs,
                rhs,
            } => self.logical(expr, *op == BinaryOp::And, *lhs, *rhs)?,
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => self.conditional(expr, *condition, *then, *r#else)?,
            _ => {
                let ty = self.ty(expr);
                let value = self.rvalue(expr)?;
                let span = self.span(expr);
                let temp = self.builder.temp(LocalTy::Value(ty), span);
                self.builder.assign(Place::local(temp), value, span);
                Operand::Copy(Place::local(temp))
            }
        })
    }

    /// The computation of the value of `expr`, before any conversion.
    fn rvalue(&mut self, expr: ExprId) -> Lowered<Rvalue> {
        if let Some(constant) = self.constant(expr) {
            return Ok(Rvalue::Use(Operand::Const(constant)));
        }
        let hir = self.owner.hir;
        Ok(match &hir.exprs[expr] {
            // A value that is a branch is stored in a temporary.
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            }
            | Expr::Conditional { .. } => Rvalue::Use(self.operand_unconverted(expr)?),
            Expr::Binary { op, lhs, rhs } => self.binary(expr, *op, *lhs, *rhs)?,
            Expr::Unary {
                op: op @ (UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot),
                expr: operand,
            } => {
                let op = match op {
                    UnaryOp::Neg => UnOp::Neg,
                    UnaryOp::Not => UnOp::Not,
                    _ => UnOp::BitNot,
                };
                Rvalue::Unary(op, self.operand(*operand)?)
            }
            // A written cast is the conversion inference records on its
            // operand.
            Expr::Cast { expr: inner, .. }
            | Expr::Unary {
                op: UnaryOp::Plus,
                expr: inner,
            } => self.converted_rvalue(*inner)?,
            Expr::Interpolated(parts) => self.interpolate(expr, parts)?,
            Expr::New { args, inits, .. } => {
                self.new_struct(expr, args.as_deref(), inits.as_deref())?
            }
            Expr::Tuple(elems) => {
                let values: Vec<ExprId> = elems.iter().map(|&(_, elem)| elem).collect();
                Rvalue::Aggregate(AggregateKind::Tuple, self.operands(&values)?)
            }
            Expr::Local(_)
            | Expr::Member { .. }
            | Expr::Path(_)
            | Expr::This
            | Expr::Index { .. }
            | Expr::Call { .. }
            | Expr::Postfix { .. }
            | Expr::Unary {
                op: UnaryOp::PreIncrement | UnaryOp::PreDecrement,
                ..
            } => Rvalue::Use(self.operand_unconverted(expr)?),
            _ => self.ice(expr, "the expression is not lowered"),
        })
    }

    /// The type inference gives `expr`.
    pub(super) fn ty(&self, expr: ExprId) -> Ty {
        match self.owner.inference.expr(expr) {
            Some(ty) => ty.clone(),
            None => self.ice(expr, "the value has no type"),
        }
    }

    /// The constant `expr` stands for, if it is one.
    fn constant(&self, expr: ExprId) -> Option<Const> {
        constant(self.owner.hir, self.owner.inference, expr)
    }

    /// `value` converted as `conversion` says, in a new temporary; `value`
    /// itself when there is no conversion.
    pub(super) fn converted(
        &mut self,
        conversion: Option<&Conversion>,
        value: Operand,
        span: Span,
    ) -> Operand {
        let Some(conversion) = conversion else {
            return value;
        };
        let temp = self
            .builder
            .temp(LocalTy::Value(conversion.to.clone()), span);
        self.builder
            .assign(Place::local(temp), cast(conversion, value), span);
        Operand::Copy(Place::local(temp))
    }

    /// The type of the value of `operand`.
    fn operand_ty(&self, operand: &Operand) -> Ty {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => {
                self.builder.body().place_ty(place, self.owner.db)
            }
            Operand::Const(constant) => constant.ty(),
        }
    }

    // Places.

    /// The place `expr` names: a local, `this`, a field of a value, or
    /// what a field of `this` names.
    pub(super) fn place(&mut self, expr: ExprId) -> Lowered<Place> {
        let hir = self.owner.hir;
        Ok(match &hir.exprs[expr] {
            Expr::Local(local) => self.local_place(*local),
            Expr::This => self.this_place(expr),
            Expr::Path(path) => self.path_place(expr, path),
            Expr::Member { receiver, name, .. } => {
                // A receiver that is not a place is stored in a temporary.
                let value = self.operand(*receiver)?;
                let base = self.operand_place(value, self.span(*receiver));
                self.field_place(expr, base, name)
            }
            _ => self.ice(expr, "the place is not lowered"),
        })
    }

    /// The place that holds `operand`: a constant is stored in a
    /// temporary first.
    pub(super) fn operand_place(&mut self, operand: Operand, span: Span) -> Place {
        match operand {
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
        }
    }

    /// The place of the value of the HIR local `local`: the local itself,
    /// or what it borrows for a parameter of mode `in` or `ref`.
    pub(super) fn local_place(&self, local: mora_hir::LocalId) -> Place {
        self.value_of(self.local(local))
    }

    /// The place of `this`, which `expr` uses.
    pub(super) fn this_place(&self, expr: ExprId) -> Place {
        match self.this {
            Some(this) => self.value_of(this),
            None => self.ice(expr, "`this` is not lowered outside a method"),
        }
    }

    /// The place of the value of `local`: the local itself, or what it
    /// borrows.
    fn value_of(&self, local: LocalId) -> Place {
        match self.builder.body().local(local).ty {
            LocalTy::Value(_) => Place::local(local),
            LocalTy::Borrow(..) => Place {
                local,
                projections: vec![Projection::Deref],
            },
        }
    }

    // Operands in order.

    /// The values of `exprs`, evaluated from left to right.
    pub(super) fn operands(&mut self, exprs: &[ExprId]) -> Lowered<Vec<Operand>> {
        let mut operands = Vec::with_capacity(exprs.len());
        for (index, &expr) in exprs.iter().enumerate() {
            let operand = self.operand(expr)?;
            operands.push(self.stable(operand, &exprs[index + 1..], self.span(expr)));
        }
        Ok(operands)
    }

    /// `operand`, read now: a place that one of the expressions evaluated
    /// after it may change is copied to a temporary first. A temporary is
    /// assigned once and never changes.
    pub(super) fn stable(&mut self, operand: Operand, later: &[ExprId], span: Span) -> Operand {
        match operand {
            Operand::Copy(place)
                if self.builder.body().local(place.local).source.is_some()
                    && later.iter().any(|&expr| self.has_effects(expr)) =>
            {
                let ty = self.builder.body().place_ty(&place, self.owner.db);
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
        match &self.owner.hir.exprs[expr] {
            Expr::Literal(_) | Expr::Local(_) | Expr::Path(_) | Expr::This => false,
            Expr::Member { receiver, .. } => self.has_effects(*receiver),
            // The initializers of omitted fields cannot change a place of
            // the body.
            Expr::New {
                args: None, inits, ..
            } => inits
                .iter()
                .flatten()
                .any(|&(_, init)| self.has_effects(init)),
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

    fn binary(&mut self, expr: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Lowered<Rvalue> {
        let Some(op) = bin_op(op) else {
            self.ice(expr, "the operator is not lowered")
        };
        let left = self.operand(lhs)?;
        let left = self.stable(left, &[rhs], self.span(lhs));
        let right = self.operand(rhs)?;
        Ok(Rvalue::Binary(op, left, right))
    }

    /// `lhs = rhs`, or `lhs op= rhs`: `lhs = (T)(lhs op rhs)`, its place
    /// evaluated once, with the cast back to the type `T` of the place
    /// that inference records on the assignment (§7.12).
    fn assign(
        &mut self,
        expr: ExprId,
        op: Option<BinaryOp>,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Lowered<()> {
        // `_ = value` evaluates the value and drops it.
        if op.is_none() && matches!(self.owner.hir.exprs[lhs], Expr::Discard) {
            return self.effect(rhs);
        }
        let place = self.place(lhs)?;
        let Some(op) = op else {
            return self.expr_into(rhs, place);
        };
        let Some(op) = bin_op(op) else {
            self.ice(expr, "the operator is not lowered")
        };
        // The conversion recorded on `lhs` applies to reading it.
        let conversion = self.owner.inference.conversion(lhs);
        let read = self.converted(conversion, Operand::Copy(place.clone()), self.span(lhs));
        let result_ty = self.operand_ty(&read);
        let read = self.stable(read, &[rhs], self.span(lhs));
        let right = self.operand(rhs)?;
        let value = Rvalue::Binary(op, read, right);
        let span = self.span(expr);
        match self.owner.inference.conversion(expr) {
            Some(back) => {
                let temp = self.builder.temp(LocalTy::Value(result_ty), span);
                self.builder.assign(Place::local(temp), value, span);
                let value = cast(back, Operand::Copy(Place::local(temp)));
                self.builder.assign(place, value, span);
            }
            None => self.builder.assign(place, value, span),
        }
        Ok(())
    }

    /// `++` or `--` on the place `target` names: `target = target ± 1` in
    /// its own type. Returns the place.
    fn step(&mut self, expr: ExprId, target: ExprId, increment: bool) -> Lowered<Place> {
        let place = self.place(target)?;
        self.step_at(expr, target, place.clone(), increment);
        Ok(place)
    }

    /// `++` or `--` on `place`, which `target` names.
    fn step_at(&mut self, expr: ExprId, target: ExprId, place: Place, increment: bool) {
        let one = match self.builder.body().place_ty(&place, self.owner.db) {
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
        self.builder.assign(place, value, self.span(expr));
    }

    /// `target++` or `target--` whose value is used: the old value, kept
    /// in a temporary before the step.
    fn postfix(&mut self, expr: ExprId, target: ExprId, increment: bool) -> Lowered<Operand> {
        let place = self.place(target)?;
        let span = self.span(expr);
        let ty = self.builder.body().place_ty(&place, self.owner.db);
        let old = self.builder.temp(LocalTy::Value(ty), span);
        self.builder.assign(
            Place::local(old),
            Rvalue::Use(Operand::Copy(place.clone())),
            span,
        );
        self.step_at(expr, target, place, increment);
        Ok(Operand::Copy(Place::local(old)))
    }

    // Interpolated strings.

    /// An interpolated string as [`Rvalue::Interpolate`], which stands in
    /// until the standard library formats values.
    fn interpolate(&mut self, expr: ExprId, parts: &[InterpPart]) -> Lowered<Rvalue> {
        // Each hole's value, then its alignment, from left to right.
        let mut values = Vec::new();
        for part in parts {
            if let InterpPart::Hole {
                value, alignment, ..
            } = part
            {
                values.push(*value);
                values.extend(alignment);
            }
        }
        let mut operands = self.operands(&values)?.into_iter();
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            pieces.push(match part {
                InterpPart::Text { value, .. } => match value {
                    Some(value) => InterpPiece::Text(value.clone()),
                    None => self.ice(expr, "the text of the string has no value"),
                },
                InterpPart::Hole {
                    alignment, format, ..
                } => {
                    let value = operands.next();
                    let aligned = alignment.map(|_| operands.next());
                    let (Some(value), None | Some(Some(_))) = (value, &aligned) else {
                        self.ice(expr, "a hole without its value")
                    };
                    let alignment = aligned.flatten();
                    InterpPiece::Operand {
                        value,
                        alignment,
                        format: format.clone(),
                    }
                }
            });
        }
        Ok(Rvalue::Interpolate(pieces))
    }
}

/// The constant `expr` of `hir` stands for: a literal, or an operation
/// on literals, whose value inference records for a number.
pub(super) fn constant(hir: &mora_hir::Body, inference: &Inference, expr: ExprId) -> Option<Const> {
    let ty = inference.expr(expr)?;
    if let Ty::Primitive(primitive) = ty
        && let Some(value) = inference.constant(expr).or(inference.literal(expr))
    {
        return Some(numeric(value, *primitive));
    }
    match &hir.exprs[expr] {
        Expr::Literal(Literal::Bool(value)) => Some(Const::Bool(*value)),
        Expr::Literal(Literal::Char(text)) => char_value(text).map(Const::Char),
        Expr::Literal(Literal::String(text)) => {
            string_value(text).map(|(value, _)| Const::String(value))
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

/// The cast of `value` that applies `conversion`.
fn cast(conversion: &Conversion, value: Operand) -> Rvalue {
    let kind = match conversion.kind {
        ConversionKind::Implicit => CastKind::Implicit,
        ConversionKind::Numeric => CastKind::Numeric,
    };
    Rvalue::Cast(kind, value, conversion.to.clone())
}
