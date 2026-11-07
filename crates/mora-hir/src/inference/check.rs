//! The walk over a body that gives each expression and local its type.

use mora_span::{Db, Project, SourceFile};
use mora_syntax::lexer::number::{IntSuffix, RealSuffix, int_value, real_value};

use super::{Context, Inference, TypeError, constant, converts, is_known, numeric, same};
use crate::arena::arena_index;
use crate::bodies::{
    Arg, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, Expr, ExprId, InterpPart, Literal,
    LocalId, MatchArm, Pat, PatId, Stmt, StmtId, Target, TypeId, UnaryOp, body, body_resolution,
};
use crate::defs::ItemId;
use crate::items::ItemKind;
use crate::resolve::{Resolved, item_data};
use crate::signature::signature;
use crate::ty::{Primitive, Signature, TupleElem, Ty};

const BOOL: Ty = Ty::Primitive(Primitive::Bool);
const INT: Ty = Ty::Primitive(Primitive::Int);

/// What the context of an expression asks of its type.
#[derive(Clone, Copy)]
enum Expect<'t> {
    /// Nothing.
    None,
    /// The type of the other operand of a binary operator, which an
    /// unsuffixed literal takes when it holds the value (§7.12, rule 1).
    Hint(&'t Ty),
    /// The type the value must convert to: a literal takes it, or does not
    /// fit it (`MR0010`).
    Target(&'t Ty),
}

/// An expectation of `ty` as a hint, or none when it is unknown.
fn hint(ty: Option<&Ty>) -> Expect<'_> {
    ty.map_or(Expect::None, Expect::Hint)
}

/// Checks the body of `owner`.
pub(super) fn infer_body(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Inference {
    let body = body(db, file, owner);
    let resolution = body_resolution(db, project, file, owner);
    let mut check = Check {
        db,
        body,
        resolution,
        exprs: vec![None; body.exprs.len()],
        locals: Vec::with_capacity(body.locals.len()),
        errors: Vec::new(),
        ret: None,
    };
    check.locals = body
        .locals
        .iter()
        .map(|(_, local)| local.ty.map(|ty| check.written(ty)))
        .collect();
    // What the body's root is checked against: an initializer's type, or a
    // function's return type.
    let mut expected = None;
    let item = match owner {
        BodyOwner::Item(item) => Some(ItemId {
            project,
            file,
            item,
        }),
        _ => None,
    };
    match item.map(|item| signature(db, item)) {
        Some(Signature::Function(function)) => {
            // A parameter the parser recovered from declares no local, and
            // then the locals cannot be matched to the signature.
            if function.params.len() == body.params.len() {
                for (&local, param) in body.params.iter().zip(&function.params) {
                    check.locals[arena_index(local)].get_or_insert_with(|| param.ty.clone());
                }
            }
            // What an `async` function's body returns is for §12 to say.
            if !function.is_async {
                expected = Some(function.ret.clone());
            }
        }
        Some(Signature::Value(ty)) => expected = Some(ty.clone()),
        _ => {
            if body.kind == BodyKind::Initializer
                && let [ty] = body.signature_types[..]
            {
                expected = Some(check.written(ty));
            }
        }
    }
    check.defaults(&body.defaults);
    if let Some(call) = body.this_call {
        check.infer(call);
    }
    if let Some(root) = body.root {
        check.function_body(root, expected);
    }
    Inference {
        exprs: check.exprs,
        locals: check.locals,
        errors: check.errors,
    }
}

struct Check<'a> {
    /// The database the signatures are read from.
    db: &'a dyn Db,
    /// The body being checked.
    body: &'a Body,
    /// What its paths and types name.
    resolution: &'a BodyResolution,
    /// The type of each expression so far.
    exprs: Vec<Option<Ty>>,
    /// The type of each local so far.
    locals: Vec<Option<Ty>>,
    /// The errors so far.
    errors: Vec<TypeError>,
    /// The type `return e` checks `e` against, `None` when it is unknown.
    ret: Option<Ty>,
}

impl Check<'_> {
    /// The type written at `ty`, with its names looked up.
    fn written(&self, ty: TypeId) -> Ty {
        self.resolution
            .types
            .get(arena_index(ty))
            .cloned()
            .unwrap_or(Ty::Error)
    }

    /// The body of a function, a local function or a lambda, whose
    /// `return` statements check against `ret`. An expression body is the
    /// value returned, unless the function returns `()`.
    fn function_body(&mut self, root: ExprId, ret: Option<Ty>) {
        let outer = std::mem::replace(&mut self.ret, ret);
        match &self.ret {
            Some(ret) if *ret != Ty::UNIT && !matches!(self.body.exprs[root], Expr::Block(_)) => {
                let ret = ret.clone();
                self.check(root, &ret);
            }
            _ => {
                self.infer(root);
            }
        }
        self.ret = outer;
    }

    /// Checks default values against the types of their parameters.
    fn defaults(&mut self, defaults: &[(LocalId, ExprId)]) {
        for &(local, default) in defaults {
            match self.locals[arena_index(local)].clone() {
                Some(ty) => self.check(default, &ty),
                None => {
                    self.infer(default);
                }
            }
        }
    }

    fn infer(&mut self, expr: ExprId) -> Option<Ty> {
        self.expr(expr, Expect::None)
    }

    /// Checks that `expr` converts to `expected`.
    fn check(&mut self, expr: ExprId, expected: &Ty) {
        let found = self.expr(expr, Expect::Target(expected));
        self.coerce(expr, found.as_ref(), expected, Context::Value);
    }

    /// Checks that the condition `expr` is a `bool`.
    fn condition(&mut self, expr: ExprId) {
        let found = self.expr(expr, Expect::Target(&BOOL));
        self.coerce(expr, found.as_ref(), &BOOL, Context::Condition);
    }

    /// Reports `expr` when its type `found` does not convert to `expected`.
    fn coerce(&mut self, expr: ExprId, found: Option<&Ty>, expected: &Ty, context: Context) {
        if let Some(found) = found
            && is_known(found)
            && is_known(expected)
            && !converts(found, expected)
        {
            self.errors.push(TypeError::Mismatch {
                expr,
                expected: expected.clone(),
                found: found.clone(),
                context,
            });
        }
    }

    fn block(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        let body = self.body;
        match &body.stmts[stmt] {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
            Stmt::Expr(expr) => {
                self.infer(*expr);
            }
            Stmt::Local { local, init } => {
                let Some(init) = *init else { return };
                let slot = arena_index(*local);
                match self.locals[slot].clone() {
                    Some(declared) => self.check(init, &declared),
                    None => self.locals[slot] = self.infer(init),
                }
            }
            Stmt::Deconstruct { pat, init } => {
                self.infer(*init);
                self.pat(*pat);
            }
            Stmt::Function(function) => {
                self.defaults(&function.defaults);
                let ret = (!function.is_async)
                    .then(|| function.ret.map_or(Ty::UNIT, |ty| self.written(ty)));
                if let Some(root) = function.body {
                    self.function_body(root, ret);
                }
            }
            Stmt::Block(stmts) => self.block(stmts),
            Stmt::If {
                condition,
                then,
                r#else,
            } => {
                self.condition(*condition);
                self.stmt(*then);
                if let Some(r#else) = r#else {
                    self.stmt(*r#else);
                }
            }
            Stmt::While { condition, body } | Stmt::DoWhile { body, condition } => {
                self.condition(*condition);
                self.stmt(*body);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                self.block(init);
                if let Some(condition) = condition {
                    self.condition(*condition);
                }
                for &expr in update {
                    self.infer(expr);
                }
                self.stmt(*body);
            }
            Stmt::Foreach {
                pat,
                iterable,
                body,
                ..
            } => {
                self.infer(*iterable);
                self.pat(*pat);
                self.stmt(*body);
            }
            Stmt::Return(Some(expr)) => {
                // Taken out while the value is checked: a lambda inside it
                // has its own return type.
                let ret = self.ret.take();
                match &ret {
                    Some(ret) => self.check(*expr, ret),
                    None => {
                        self.infer(*expr);
                    }
                }
                self.ret = ret;
            }
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    fn r#match(&mut self, scrutinee: ExprId, arms: &[MatchArm]) {
        self.infer(scrutinee);
        for arm in arms {
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.condition(guard);
            }
            self.infer(arm.body);
        }
    }

    /// The expressions inside a pattern; its bindings stay unknown.
    fn pat(&mut self, pat: PatId) {
        let body = self.body;
        match &body.pats[pat] {
            Pat::Missing | Pat::Bind(_) | Pat::Discard | Pat::Slice(None) => {}
            Pat::Constant(expr) | Pat::Relational { value: expr, .. } => {
                self.infer(*expr);
            }
            Pat::Type { binding, .. } => {
                if let Some(binding) = binding {
                    self.pat(*binding);
                }
            }
            Pat::Not(inner) | Pat::Slice(Some(inner)) => self.pat(*inner),
            Pat::And(lhs, rhs) | Pat::Or(lhs, rhs) => {
                self.pat(*lhs);
                self.pat(*rhs);
            }
            Pat::Case { path, fields } => {
                self.infer(*path);
                self.fields(fields);
            }
            Pat::Positional(fields) => self.fields(fields),
            Pat::Member {
                fields, binding, ..
            } => {
                self.fields(fields);
                if let Some(binding) = binding {
                    self.pat(*binding);
                }
            }
            Pat::List(items) => {
                for &item in items {
                    self.pat(item);
                }
            }
        }
    }

    fn fields(&mut self, fields: &[(Option<String>, PatId)]) {
        for &(_, pat) in fields {
            self.pat(pat);
        }
    }

    fn args(&mut self, args: &[Arg]) {
        for arg in args {
            self.infer(arg.expr);
        }
    }

    /// The type of `expr`, recorded for it.
    fn expr(&mut self, expr: ExprId, expect: Expect<'_>) -> Option<Ty> {
        if let Expect::Target(Ty::Primitive(target)) = expect
            && numeric::is_numeric(*target)
            && let Some(ty) = self.constant(expr, *target)
        {
            return Some(ty);
        }
        let ty = self.expr_kind(expr, expect);
        self.exprs[arena_index(expr)].clone_from(&ty);
        ty
    }

    /// An operation made only of unsuffixed literals takes the numeric
    /// type `target` like one literal (§3.5): its value is computed exactly
    /// and must fit. `None` when `expr` is not such an operation, or is a
    /// real that an integer type cannot take, so the usual rules apply.
    fn constant(&mut self, expr: ExprId, target: Primitive) -> Option<Ty> {
        let body = self.body;
        if !matches!(body.exprs[expr], Expr::Binary { .. } | Expr::Unary { .. }) {
            return None;
        }
        let value = constant::value(&body.exprs, expr)?;
        let fits = match value {
            constant::Value::Int(value) => numeric::holds(target, value),
            constant::Value::Real(value) => match target {
                Primitive::Float => !(value as f32).is_infinite(),
                Primitive::Double => value.is_finite(),
                _ => return None,
            },
            constant::Value::Overflow => false,
        };
        if !fits {
            self.errors.push(TypeError::Literal {
                expr,
                ty: Some(target),
            });
        }
        let ty = Ty::Primitive(target);
        constant::parts(&body.exprs, expr, &mut |part| {
            self.exprs[arena_index(part)] = Some(ty.clone());
        });
        Some(ty)
    }

    fn expr_kind(&mut self, expr: ExprId, expect: Expect<'_>) -> Option<Ty> {
        let body = self.body;
        match &body.exprs[expr] {
            // The parser reported it.
            Expr::Missing => Some(Ty::Error),
            Expr::Literal(literal) => self.literal(expr, literal, false, expect),
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Hole {
                        value, alignment, ..
                    } = part
                    {
                        self.infer(*value);
                        if let Some(alignment) = alignment {
                            self.check(*alignment, &INT);
                        }
                    }
                }
                Some(Ty::String)
            }
            Expr::Local(local) => self.locals[arena_index(*local)].clone(),
            Expr::Path(path) => {
                let resolution = self.resolution.paths.get(&expr)?;
                let mut ty = self.target(&resolution.target);
                // The segments after the target are member accesses.
                let named = resolution.segments.max(1);
                for name in path.segments.iter().skip(named) {
                    ty = self.field(ty.as_ref(), name);
                }
                ty
            }
            Expr::Tuple(elems) => self.tuple(elems, expect),
            Expr::Collection(elems) => {
                for &(_, elem) in elems {
                    self.infer(elem);
                }
                None
            }
            Expr::Member {
                receiver,
                name,
                optional,
                ..
            } => {
                let receiver = self.infer(*receiver);
                if let Some(resolution) = self.resolution.paths.get(&expr) {
                    return self.target(&resolution.target);
                }
                // `x?.F` gives an option (§7.5).
                if *optional {
                    return None;
                }
                self.field(receiver.as_ref(), name)
            }
            Expr::Call { callee, args } => {
                self.infer(*callee);
                self.args(args);
                None
            }
            Expr::Index { base, args, .. } => {
                self.infer(*base);
                self.args(args);
                None
            }
            Expr::Unary { op, expr: operand } => self.unary(expr, *op, *operand, expect),
            Expr::Postfix {
                increment,
                expr: operand,
            } => {
                let op = if *increment {
                    UnaryOp::PreIncrement
                } else {
                    UnaryOp::PreDecrement
                };
                self.step(expr, op, *operand)
            }
            Expr::Binary { op, lhs, rhs } => self.binary(expr, *op, *lhs, *rhs),
            Expr::Assign { op, lhs, rhs } => {
                self.assign(expr, *op, *lhs, *rhs);
                None
            }
            Expr::Propagate(inner) | Expr::Await(inner) | Expr::As { expr: inner, .. } => {
                self.infer(*inner);
                None
            }
            Expr::Cast { ty, expr: operand } => self.cast(expr, *ty, *operand),
            Expr::Range { start, end } => {
                for &end in start.iter().chain(end) {
                    self.infer(end);
                }
                None
            }
            Expr::Match { scrutinee, arms } => {
                self.r#match(*scrutinee, arms);
                None
            }
            Expr::With { expr: value, inits } => {
                let ty = self.infer(*value);
                for (name, init) in inits {
                    match self.field(ty.as_ref(), name) {
                        Some(field) => self.check(*init, &field),
                        None => {
                            self.infer(*init);
                        }
                    }
                }
                ty
            }
            Expr::Is { expr: value, pat } => {
                self.infer(*value);
                self.pat(*pat);
                Some(BOOL)
            }
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => self.conditional(*condition, *then, *r#else, expect),
            Expr::Lambda(lambda) => {
                self.defaults(&lambda.defaults);
                self.function_body(lambda.body, None);
                None
            }
            Expr::New { args, inits, .. } => {
                if let Some(args) = args {
                    self.args(args);
                }
                for &(_, init) in inits.iter().flatten() {
                    self.infer(init);
                }
                None
            }
            Expr::NewArray { size, .. } => {
                self.infer(*size);
                None
            }
            Expr::Nameof(inner) => {
                self.infer(*inner);
                Some(Ty::String)
            }
            Expr::Sizeof(_) => Some(INT),
            Expr::Block(stmts) => {
                self.block(stmts);
                None
            }
            Expr::This | Expr::Type(_) | Expr::Default(_) | Expr::Macro => None,
        }
    }

    /// The type of a value a path or a member of a type names.
    fn target(&self, target: &Target) -> Option<Ty> {
        match target {
            Target::Error => Some(Ty::Error),
            Target::Item(Resolved::Item(item))
                if item_data(self.db, *item).kind == ItemKind::Field =>
            {
                match signature(self.db, *item) {
                    Signature::Value(ty) => Some(ty.clone()),
                    _ => None,
                }
            }
            Target::Item(_) => None,
            Target::Value(value) => {
                let member = item_data(self.db, value.ty).values.get(value.index)?;
                self.struct_field(value.ty, &member.name)
            }
        }
    }

    /// The type of the field `name` of a value of type `receiver`, when it
    /// is a struct with that field.
    fn field(&self, receiver: Option<&Ty>, name: &str) -> Option<Ty> {
        match receiver {
            Some(Ty::Struct(item)) => self.struct_field(*item, name),
            _ => None,
        }
    }

    fn struct_field(&self, item: ItemId, name: &str) -> Option<Ty> {
        match signature(self.db, item) {
            Signature::Struct(fields) => fields
                .iter()
                .find(|field| field.name == name)
                .map(|field| field.ty.clone()),
            _ => None,
        }
    }

    /// A tuple: an expected tuple type of the same length gives each element
    /// its expected type.
    fn tuple(&mut self, elems: &[(Option<String>, ExprId)], expect: Expect<'_>) -> Option<Ty> {
        let expected = match expect {
            Expect::Target(Ty::Tuple(expected)) | Expect::Hint(Ty::Tuple(expected))
                if expected.len() == elems.len() =>
            {
                Some(expected)
            }
            _ => None,
        };
        if let (Expect::Target(_), Some(expected)) = (expect, expected) {
            for (&(_, elem), expected) in elems.iter().zip(expected) {
                self.check(elem, &expected.ty);
            }
            return Some(Ty::Tuple(expected.clone()));
        }
        let mut types = Vec::with_capacity(elems.len());
        let mut known = true;
        for (index, (name, elem)) in elems.iter().enumerate() {
            let expect =
                expected.map_or(Expect::None, |expected| Expect::Hint(&expected[index].ty));
            match self.expr(*elem, expect) {
                Some(ty) => types.push(TupleElem {
                    name: name.clone(),
                    ty,
                }),
                None => known = false,
            }
        }
        known.then_some(Ty::Tuple(types))
    }

    /// A literal, negated when it is the operand of unary `-`; `expr` is
    /// the expression a diagnostic points at.
    fn literal(
        &mut self,
        expr: ExprId,
        literal: &Literal,
        negated: bool,
        expect: Expect<'_>,
    ) -> Option<Ty> {
        let ty = match literal {
            Literal::Int(text) => return self.integer(expr, text, negated, expect),
            Literal::Real(text) => return self.real(expr, text, expect),
            Literal::Char(_) => Primitive::Char,
            Literal::Bool(_) => Primitive::Bool,
            Literal::String(_) => return Some(Ty::String),
            // `none` takes its type from its target, which §5.6 gives.
            Literal::None => return None,
        };
        Some(Ty::Primitive(ty))
    }

    /// An integer literal (§3.5): its suffix's type, the numeric type its
    /// context asks for when it holds the value, or the first of `int`,
    /// `long` and `ulong` that does.
    fn integer(
        &mut self,
        expr: ExprId,
        text: &str,
        negated: bool,
        expect: Expect<'_>,
    ) -> Option<Ty> {
        // The lexer reported a literal it cannot read.
        let Some((value, suffix)) = int_value(text) else {
            return Some(Ty::Error);
        };
        let value = if negated {
            -i128::from(value)
        } else {
            i128::from(value)
        };
        let ty = match suffix {
            IntSuffix::U => Primitive::UInt,
            IntSuffix::L => Primitive::Long,
            IntSuffix::Ul => Primitive::ULong,
            IntSuffix::None => {
                let (asked, target) = match expect {
                    Expect::Hint(Ty::Primitive(ty)) => (Some(*ty), false),
                    Expect::Target(Ty::Primitive(ty)) => (Some(*ty), true),
                    _ => (None, false),
                };
                match asked.filter(|&ty| numeric::is_numeric(ty)) {
                    Some(ty) if numeric::holds(ty, value) => ty,
                    // The literal still takes the type, so nothing else is
                    // reported about it.
                    Some(ty) if target => {
                        self.errors.push(TypeError::Literal { expr, ty: Some(ty) });
                        ty
                    }
                    _ => match numeric::default_integer(value) {
                        Some(ty) => ty,
                        None => {
                            self.errors.push(TypeError::Literal { expr, ty: None });
                            return Some(Ty::Error);
                        }
                    },
                }
            }
        };
        if suffix != IntSuffix::None && !numeric::holds(ty, value) {
            self.errors.push(TypeError::Literal { expr, ty: Some(ty) });
        }
        Some(Ty::Primitive(ty))
    }

    /// A real literal (§3.5): its suffix's type, `float` when its context
    /// asks for one, or `double`.
    fn real(&mut self, expr: ExprId, text: &str, expect: Expect<'_>) -> Option<Ty> {
        // The lexer reported a literal it cannot read or that is too large
        // for its suffix's type or for a `double`.
        let Some((value, suffix)) = real_value(text).filter(|(value, _)| value.is_finite()) else {
            return Some(Ty::Error);
        };
        // Whether the value is too large for a `float` is the question.
        let too_large = (value as f32).is_infinite();
        let ty = match (suffix, expect) {
            (RealSuffix::F, _) => Primitive::Float,
            (RealSuffix::None, Expect::Target(&Ty::Primitive(Primitive::Float))) => {
                if too_large {
                    self.errors.push(TypeError::Literal {
                        expr,
                        ty: Some(Primitive::Float),
                    });
                }
                Primitive::Float
            }
            (RealSuffix::None, Expect::Hint(&Ty::Primitive(Primitive::Float))) if !too_large => {
                Primitive::Float
            }
            _ => Primitive::Double,
        };
        Some(Ty::Primitive(ty))
    }

    /// Unary `+`, `-`, `~`, `!`, `^`, `++` and `--` (§7.12).
    fn unary(
        &mut self,
        expr: ExprId,
        op: UnaryOp,
        operand: ExprId,
        expect: Expect<'_>,
    ) -> Option<Ty> {
        let body = self.body;
        // A negated literal is one value, so `-128` fits an `sbyte`.
        if op == UnaryOp::Neg
            && let Expr::Literal(literal) = &body.exprs[operand]
            && negatable(literal)
        {
            let ty = self.literal(expr, literal, true, expect);
            self.exprs[arena_index(operand)].clone_from(&ty);
            return ty;
        }
        match op {
            UnaryOp::Not => {
                let found = self.infer(operand);
                if let Some(found) = found.filter(|found| is_known(found) && *found != BOOL) {
                    self.errors.push(TypeError::Unary {
                        expr,
                        op,
                        operand: found,
                    });
                }
                Some(BOOL)
            }
            UnaryOp::FromEnd => {
                self.infer(operand);
                None
            }
            UnaryOp::PreIncrement | UnaryOp::PreDecrement => self.step(expr, op, operand),
            UnaryOp::Plus | UnaryOp::Neg | UnaryOp::BitNot => {
                let found = self.infer(operand)?;
                let result = match found {
                    Ty::Primitive(ty) if numeric::is_numeric(ty) => match (op, ty) {
                        (UnaryOp::Neg, Primitive::UInt) => Some(Primitive::Long),
                        (UnaryOp::Neg, Primitive::ULong | Primitive::NUInt)
                        | (UnaryOp::BitNot, Primitive::Float | Primitive::Double) => None,
                        _ => Some(numeric::widen(ty)),
                    },
                    _ if !is_known(&found) => return None,
                    _ => None,
                };
                self.operated(expr, op, found, result)
            }
        }
    }

    /// `++` and `--`, prefix or postfix: the operand keeps its numeric type.
    fn step(&mut self, expr: ExprId, op: UnaryOp, operand: ExprId) -> Option<Ty> {
        let found = self.infer(operand)?;
        let result = match found {
            Ty::Primitive(ty) if numeric::is_numeric(ty) => Some(ty),
            _ if !is_known(&found) => return None,
            _ => None,
        };
        self.operated(expr, op, found, result)
    }

    /// The type of a unary operation, or its error when `result` is `None`.
    fn operated(
        &mut self,
        expr: ExprId,
        op: UnaryOp,
        operand: Ty,
        result: Option<Primitive>,
    ) -> Option<Ty> {
        match result {
            Some(ty) => Some(Ty::Primitive(ty)),
            None => {
                self.errors.push(TypeError::Unary { expr, op, operand });
                Some(Ty::Error)
            }
        }
    }

    /// Whether `expr` is a literal that can take its type from the other
    /// operand (§7.12, rule 1).
    fn takes_hint(&self, expr: ExprId) -> bool {
        let exprs = &self.body.exprs;
        let expr = match &exprs[expr] {
            Expr::Unary {
                op: UnaryOp::Neg,
                expr,
            } => *expr,
            _ => expr,
        };
        matches!(
            exprs[expr],
            Expr::Literal(Literal::Int(_) | Literal::Real(_))
        )
    }

    /// The types of two operands; a literal operand takes the other's type
    /// when it holds the value (§7.12, rule 1).
    fn operands(&mut self, lhs: ExprId, rhs: ExprId) -> (Option<Ty>, Option<Ty>) {
        match (self.takes_hint(lhs), self.takes_hint(rhs)) {
            (false, true) => {
                let l = self.infer(lhs);
                let r = self.expr(rhs, hint(l.as_ref()));
                (l, r)
            }
            (true, false) => {
                let r = self.infer(rhs);
                let l = self.expr(lhs, hint(r.as_ref()));
                (l, r)
            }
            _ => (self.infer(lhs), self.infer(rhs)),
        }
    }

    /// A binary operation, or the operation of a compound assignment.
    fn binary(&mut self, expr: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Option<Ty> {
        match op {
            BinaryOp::And | BinaryOp::Or => {
                self.condition(lhs);
                self.condition(rhs);
                return Some(BOOL);
            }
            // Option operators come with options (§7.5).
            BinaryOp::Coalesce => {
                self.infer(lhs);
                self.infer(rhs);
                return None;
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr => {
                let value = self.infer(lhs);
                self.check(rhs, &INT);
                let value = value?;
                return match value {
                    Ty::Primitive(ty) if numeric::is_integer(ty) => {
                        Some(Ty::Primitive(numeric::widen(ty)))
                    }
                    _ if !is_known(&value) => None,
                    _ => {
                        let rhs = self.exprs[arena_index(rhs)].clone().unwrap_or(Ty::Error);
                        self.errors.push(TypeError::Binary {
                            expr,
                            op,
                            lhs: value,
                            rhs,
                        });
                        Some(Ty::Error)
                    }
                };
            }
            _ => {}
        }
        let comparison = matches!(
            op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge
        );
        let otherwise = comparison.then_some(BOOL);
        let (Some(l), Some(r)) = self.operands(lhs, rhs) else {
            return otherwise;
        };
        if !is_known(&l) || !is_known(&r) {
            return otherwise;
        }
        match operator(op, &l, &r) {
            Some(ty) => Some(ty),
            None => {
                self.errors.push(TypeError::Binary {
                    expr,
                    op,
                    lhs: l,
                    rhs: r,
                });
                Some(otherwise.unwrap_or(Ty::Error))
            }
        }
    }

    /// `a = b`, where the value converts to the type of the place, and the
    /// compound assignments, whose result converts back to the type of the
    /// place as a cast would (§7.12): only the operation itself is checked.
    fn assign(&mut self, expr: ExprId, op: Option<BinaryOp>, lhs: ExprId, rhs: ExprId) {
        match op {
            None => match self.infer(lhs) {
                Some(place) => self.check(rhs, &place),
                None => {
                    self.infer(rhs);
                }
            },
            Some(op) => {
                self.binary(expr, op, lhs, rhs);
            }
        }
    }

    /// `(T)x`: a cast converts between numeric types (§5.10).
    fn cast(&mut self, expr: ExprId, ty: TypeId, operand: ExprId) -> Option<Ty> {
        let to = self.written(ty);
        if let Some(from) = self.infer(operand)
            && is_known(&from)
            && is_known(&to)
            && !same(&from, &to)
            && !(is_numeric(&from) && is_numeric(&to))
        {
            self.errors.push(TypeError::Cast {
                expr,
                from,
                to: to.clone(),
            });
        }
        Some(to)
    }

    /// `c ? a : b`: the branches check against the expected type, or have
    /// one type that the other converts to.
    fn conditional(
        &mut self,
        condition: ExprId,
        then: ExprId,
        r#else: ExprId,
        expect: Expect<'_>,
    ) -> Option<Ty> {
        self.condition(condition);
        if let Expect::Target(expected) = expect {
            self.check(then, expected);
            self.check(r#else, expected);
            return Some(expected.clone());
        }
        let (Some(a), Some(b)) = self.operands(then, r#else) else {
            return None;
        };
        if !is_known(&a) || !is_known(&b) {
            return None;
        }
        if converts(&b, &a) {
            Some(a)
        } else if converts(&a, &b) {
            Some(b)
        } else {
            self.errors.push(TypeError::Mismatch {
                expr: r#else,
                expected: a,
                found: b,
                context: Context::Branch(then),
            });
            Some(Ty::Error)
        }
    }
}

/// Whether unary `-` on `literal` makes a negative literal: an integer
/// without an unsigned suffix, or a real.
fn negatable(literal: &Literal) -> bool {
    match literal {
        Literal::Int(text) => !matches!(int_value(text), Some((_, IntSuffix::U | IntSuffix::Ul))),
        Literal::Real(_) => true,
        _ => false,
    }
}

fn is_numeric(ty: &Ty) -> bool {
    matches!(ty, Ty::Primitive(ty) if numeric::is_numeric(*ty))
}

/// The type `op` gives on operands of the known types `l` and `r`, or
/// `None` when it is not defined on them (§5.3, §7.12).
fn operator(op: BinaryOp, l: &Ty, r: &Ty) -> Option<Ty> {
    use BinaryOp as B;
    if let (Ty::Primitive(a), Ty::Primitive(b)) = (l, r)
        && numeric::is_numeric(*a)
        && numeric::is_numeric(*b)
    {
        let ty = numeric::promote(*a, *b)?;
        return match op {
            B::Eq | B::Ne | B::Lt | B::Gt | B::Le | B::Ge => Some(BOOL),
            B::BitAnd | B::BitOr | B::BitXor | B::WrappingAdd | B::WrappingSub | B::WrappingMul => {
                numeric::is_integer(ty).then_some(Ty::Primitive(ty))
            }
            _ => Some(Ty::Primitive(ty)),
        };
    }
    let defined = match op {
        // Equality on structs and tuples is generated (§6.2, §5.5); whether
        // every field supports it is for `MR0233`.
        B::Eq | B::Ne => same(l, r),
        // A struct may be ordered through `IComparable<T>` (§8.5), which
        // this milestone does not look up.
        B::Lt | B::Gt | B::Le | B::Ge => {
            same(l, r)
                && matches!(
                    l,
                    Ty::Primitive(Primitive::Char) | Ty::String | Ty::Struct(_)
                )
        }
        B::Add => *l == Ty::String && *r == Ty::String,
        B::BitAnd | B::BitOr | B::BitXor => *l == BOOL && *r == BOOL,
        _ => false,
    };
    if !defined {
        return None;
    }
    Some(match op {
        B::Add => Ty::String,
        _ => BOOL,
    })
}
