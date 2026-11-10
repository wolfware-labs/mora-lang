//! The walk over a body that gives each expression and local its type.

mod call;

use std::collections::{HashMap, HashSet};

use mora_span::{Db, Project, SourceFile};
use mora_syntax::lexer::number::{
    IntSuffix, RealSuffix, int_value, real_value, too_large_for_float,
};

use super::constant::Value;
use super::{
    Callee, Constant, Context, Conversion, ConversionKind, Inference, NotAField, TypeError,
    constant, converts, is_known, numeric, same,
};
use crate::arena::arena_index;
use crate::bodies::{
    Arg, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, Expr, ExprId, InterpPart, Literal,
    LocalId, MatchArm, Pat, PatId, Stmt, StmtId, Target, TypeId, UnaryOp, body, body_resolution,
    body_scope,
};
use crate::defs::ItemId;
use crate::items::{ValueKind, item_tree};
use crate::resolve::{Resolved, Scope, item_data};
use crate::signature::{instance_fields, signature, struct_field, this_type};
use crate::suggest::closest;
use crate::ty::{Construct, Primitive, Signature, TupleElem, Ty};

const BOOL: Ty = Ty::Primitive(Primitive::Bool);
const INT: Ty = Ty::Primitive(Primitive::Int);

/// Whether `place` is a name, `this` or a field of one, which can be read
/// again without a change.
fn is_plain_place(body: &Body, place: ExprId) -> bool {
    match &body.exprs[place] {
        Expr::Path(_) | Expr::Local(_) | Expr::This => true,
        Expr::Member {
            receiver,
            optional: false,
            ..
        } => is_plain_place(body, *receiver),
        _ => false,
    }
}

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
        calls: HashMap::new(),
        arguments: HashMap::new(),
        local_functions: None,
        constants: HashMap::new(),
        literals: HashMap::new(),
        conversions: HashMap::new(),
        element_conversions: HashMap::new(),
        errors: Vec::new(),
        statements: HashSet::new(),
        splittable: HashMap::new(),
        block_stmt: None,
        ret: None,
        scope: body_scope(project, file, owner),
        this: None,
    };
    check.locals = body
        .locals
        .iter()
        .map(|(_, local)| local.ty.map(|ty| check.resolution.written(ty)))
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
    check.this = item.and_then(|item| this_type(db, item));
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
        // The initializer of a field of a struct, whose type the struct's
        // signature holds.
        _ if let BodyOwner::Value(ty, index) = owner
            && let Some((_, field)) = struct_field(
                db,
                ItemId {
                    project,
                    file,
                    item: ty,
                },
                index,
            ) =>
        {
            expected = Some(field.ty.clone());
        }
        _ => match body.kind {
            BodyKind::Initializer => {
                if let [ty] = body.signature_types[..] {
                    expected = Some(check.resolution.written(ty));
                }
            }
            // A function that implements an interface member or extends a
            // type holds its own return type, `()` when none is written.
            BodyKind::Function if !body.is_async => {
                expected = Some(check.resolution.return_type(body.ret));
            }
            _ => {}
        },
    }
    check.defaults(&body.defaults);
    if let Some(call) = body.this_call {
        check.infer(call);
    }
    if let Some(root) = body.root {
        let statement = matches!(
            body.kind,
            BodyKind::Function | BodyKind::Constructor | BodyKind::Destructor | BodyKind::Set
        );
        check.function_body(root, expected, statement);
    }
    Inference {
        exprs: check.exprs,
        locals: check.locals,
        calls: check.calls,
        arguments: check.arguments,
        constants: check.constants,
        literals: check.literals,
        conversions: check.conversions,
        element_conversions: check.element_conversions,
        errors: check.errors,
    }
}

/// The state of the walk over one body: what it reads, and the types and
/// errors found so far.
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
    /// The function each checked call calls, so far.
    calls: HashMap<ExprId, Callee>,
    /// The parameter of each argument of a checked call, so far.
    arguments: HashMap<ExprId, Vec<usize>>,
    /// The statement of each local function, found on the first call of
    /// one.
    local_functions: Option<HashMap<LocalId, StmtId>>,
    /// The value of each operation made only of literals so far.
    constants: HashMap<ExprId, Constant>,
    /// The value of each number literal and negated literal so far.
    literals: HashMap<ExprId, Constant>,
    /// The conversions applied so far.
    conversions: HashMap<ExprId, Conversion>,
    /// The conversions of deconstructed elements so far.
    element_conversions: HashMap<(ExprId, u32), Conversion>,
    /// The errors so far.
    errors: Vec<TypeError>,
    /// The assignments written as statements of their own, whose value
    /// nothing uses.
    statements: HashSet<ExprId>,
    /// The assignments that are the value of another one written as a
    /// statement in a block, each with that statement: a fix can split them.
    splittable: HashMap<ExprId, ExprId>,
    /// The statement of the block being walked whose turn it is.
    block_stmt: Option<StmtId>,
    /// The type `return e` checks `e` against, `None` when it is unknown.
    ret: Option<Ty>,
    /// Where the body's names are looked up from, which decides what is
    /// visible from it.
    scope: Scope,
    /// The type of `this`, in a method of a struct.
    this: Option<Ty>,
}

impl Check<'_> {
    /// The body of a function, a local function or a lambda, whose
    /// `return` statements check against `ret`. An expression body is the
    /// value returned, unless the function returns `()`.
    fn function_body(&mut self, root: ExprId, ret: Option<Ty>, statement: bool) {
        let outer = std::mem::replace(&mut self.ret, ret);
        match &self.ret {
            // A block body gives no type, so checking it only walks its
            // statements, whose `return`s check against `ret`.
            Some(ret) if *ret != Ty::UNIT => {
                let ret = ret.clone();
                self.check(root, &ret);
            }
            // The expression body of a function that returns `()`, or whose
            // return type is not checked, is a statement: nothing uses its
            // value. A lambda's may be its result.
            Some(_) => self.statement(root, false),
            None if statement => self.statement(root, false),
            None => {
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

    /// Reports `expr` when `context` does not accept its type `found` where
    /// it expects `expected` (`Context::accepts`).
    fn coerce(&mut self, expr: ExprId, found: Option<&Ty>, expected: &Ty, context: Context) {
        if let Some(found) = found
            && is_known(found)
            && is_known(expected)
        {
            if !context.accepts(found, expected) {
                self.errors.push(TypeError::Mismatch {
                    expr,
                    expected: expected.clone(),
                    found: found.clone(),
                    context,
                });
            } else {
                self.widen(expr, found, expected);
            }
        }
    }

    /// Records that `expr`, of type `from`, converts to `to` where it is
    /// used, when the types differ.
    fn widen(&mut self, expr: ExprId, from: &Ty, to: &Ty) {
        // A value of the never type does not exist, so it is not converted.
        if !same(from, to) && *from != Ty::Never {
            self.convert(expr, to.clone(), ConversionKind::Implicit);
        }
    }

    /// Records the conversion of `expr` to `to`.
    fn convert(&mut self, expr: ExprId, to: Ty, kind: ConversionKind) {
        let previous = self.conversions.insert(expr, Conversion { to, kind });
        debug_assert!(previous.is_none(), "an expression is recorded once");
    }

    fn block(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            self.block_stmt = Some(stmt);
            self.stmt(stmt);
        }
    }

    /// Checks `expr` where nothing uses its value. An assignment is only
    /// allowed there; `in_block` when a statement can be put before it.
    fn statement(&mut self, expr: ExprId, in_block: bool) {
        let body = self.body;
        if let Expr::Assign { op, lhs, rhs } = &body.exprs[expr] {
            self.statements.insert(expr);
            // `a ??= b` evaluates `b` only when it must, so it is not split.
            if in_block
                && *op != Some(BinaryOp::Coalesce)
                && let Expr::Assign { lhs: inner, .. } = &body.exprs[*rhs]
                && is_plain_place(body, *lhs)
                && is_plain_place(body, *inner)
            {
                self.splittable.insert(*rhs, expr);
            }
        }
        match &body.exprs[expr] {
            // The arms of a `match` in statement position are statements.
            Expr::Match { scrutinee, arms } => self.r#match(*scrutinee, arms, true),
            // So are the branches of a conditional that assigns.
            Expr::Conditional {
                condition,
                then,
                r#else,
            } if [*then, *r#else]
                .iter()
                .any(|&branch| matches!(body.exprs[branch], Expr::Assign { .. })) =>
            {
                self.condition(*condition);
                self.statement(*then, false);
                self.statement(*r#else, false);
            }
            _ => {
                self.infer(expr);
            }
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        let body = self.body;
        match &body.stmts[stmt] {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
            Stmt::Expr(expr) => self.statement(*expr, self.block_stmt == Some(stmt)),
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
                let ret = (!function.is_async).then(|| self.resolution.return_type(function.ret));
                if let Some(root) = function.body {
                    self.function_body(root, ret, true);
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
                // Not a block: nothing can be put before an initializer.
                for &stmt in init {
                    self.stmt(stmt);
                }
                if let Some(condition) = condition {
                    self.condition(*condition);
                }
                for &expr in update {
                    self.statement(expr, false);
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
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms, true),
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    /// A `match`; `statement` when its arms are statements, whose values
    /// nothing uses.
    fn r#match(&mut self, scrutinee: ExprId, arms: &[MatchArm], statement: bool) {
        self.infer(scrutinee);
        for arm in arms {
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.condition(guard);
            }
            if statement {
                self.statement(arm.body, false);
            } else {
                self.infer(arm.body);
            }
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
        if let Expect::Target(target) = expect
            && let Some(target) = numeric::as_numeric(target)
            && let Some(ty) = self.constant(expr, target)
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
        // A real for an integer type follows the usual rules.
        if !numeric::is_floating_point(target)
            && matches!(
                value,
                constant::Value::Real(_)
                    | constant::Value::Infinity
                    | constant::Value::OverflowReal
            )
        {
            return None;
        }
        // The exact value converted to `target`, `None` when it does not fit.
        let fitting = value.of_type(target);
        // `-` before a literal belongs to it (§3.5): only an operation has a
        // value of its own.
        let negated_literal = matches!(
            &body.exprs[expr],
            Expr::Unary { op: UnaryOp::Neg, expr: operand }
                if matches!(body.exprs[*operand], Expr::Literal(_))
        );
        if let Some(value) = fitting {
            if negated_literal {
                self.literals.insert(expr, value);
            } else {
                self.constants.insert(expr, value);
            }
        } else {
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
            // The parser reported a missing expression, and a discarded
            // element takes any type, so neither checks anything.
            Expr::Missing | Expr::Discard => Some(Ty::Error),
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
                // A value that never exists has every member (§11.4).
                if receiver == Some(Ty::Never) {
                    return receiver;
                }
                // `x?.F` gives an option (§7.5).
                if *optional {
                    return None;
                }
                self.field(receiver.as_ref(), name)
            }
            Expr::Call { callee, args } => self.call(expr, *callee, args),
            Expr::Index { base, args, .. } => {
                let base = self.infer(*base);
                self.args(args);
                base.filter(|base| *base == Ty::Never)
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
                if !self.statements.remove(&expr) {
                    let split = self.splittable.get(&expr).copied();
                    self.errors.push(TypeError::AssignmentValue { expr, split });
                }
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
                self.r#match(*scrutinee, arms, false);
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
                // Its result type is known only when a function type is
                // expected; a unit result makes the body a statement.
                let ret = match expect {
                    Expect::Target(Ty::Unsupported(function))
                        if function.construct == Construct::Function =>
                    {
                        function.parts.last().cloned()
                    }
                    _ => None,
                };
                self.function_body(lambda.body, ret, false);
                None
            }
            Expr::New { ty, args, inits } => {
                self.new_expr(expr, *ty, args.as_deref(), inits.as_deref())
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
            Expr::This => self.this.clone(),
            Expr::Type(_) | Expr::Default(_) | Expr::Macro => None,
        }
    }

    /// The type of a value a path or a member of a type names.
    fn target(&self, target: &Target) -> Option<Ty> {
        match target {
            Target::Error => Some(Ty::Error),
            // Only a constant or a static has a value signature.
            Target::Item(Resolved::Item(item)) => match signature(self.db, *item) {
                Signature::Value(ty) => Some(ty.clone()),
                _ => None,
            },
            Target::Item(_) => None,
            Target::Value(value) => {
                let member = value.member(self.db)?;
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
    /// its expected type. A tuple never takes a hint, which only a literal
    /// operand does (`takes_hint`).
    fn tuple(&mut self, elems: &[(Option<String>, ExprId)], expect: Expect<'_>) -> Option<Ty> {
        if let Expect::Target(Ty::Tuple(expected)) = expect
            && expected.len() == elems.len()
        {
            for (&(_, elem), expected) in elems.iter().zip(expected) {
                self.check(elem, &expected.ty);
            }
            return Some(Ty::Tuple(expected.clone()));
        }
        let mut types = Vec::with_capacity(elems.len());
        let mut known = true;
        for (name, elem) in elems {
            match self.infer(*elem) {
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
            Literal::Real(text) => return self.real(expr, text, negated, expect),
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
                    Expect::None => (None, false),
                    Expect::Hint(ty) => (numeric::as_numeric(ty), false),
                    Expect::Target(ty) => (numeric::as_numeric(ty), true),
                };
                match asked {
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
        self.record_literal(expr, Value::Int(value), ty);
        Some(Ty::Primitive(ty))
    }

    /// A real literal (§3.5), negated when it is the operand of unary `-`:
    /// its suffix's type, `float` when its context asks for one, or
    /// `double`.
    fn real(&mut self, expr: ExprId, text: &str, negated: bool, expect: Expect<'_>) -> Option<Ty> {
        // The lexer reported a literal it cannot read or that is too large
        // for its suffix's type or for a `double`.
        let Some((value, suffix)) = real_value(text).filter(|(value, _)| value.is_finite()) else {
            return Some(Ty::Error);
        };
        let too_large = too_large_for_float(value);
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
        let value = if negated { -value } else { value };
        self.record_literal(expr, Value::Real(value), ty);
        Some(Ty::Primitive(ty))
    }

    /// Records the value of the literal `expr` in its type `ty`, when the
    /// type holds it.
    fn record_literal(&mut self, expr: ExprId, value: Value, ty: Primitive) {
        if let Some(value) = value.of_type(ty) {
            self.literals.insert(expr, value);
        }
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
                // An operand of unknown type, or a struct, which may declare
                // `!` (§6.9), leaves the result unknown.
                let found = self.infer(operand).filter(is_known)?;
                if is_struct(&found) {
                    return None;
                }
                if found != BOOL && found != Ty::Never {
                    self.errors.push(TypeError::Unary {
                        expr,
                        op,
                        operand,
                        ty: found,
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
                if is_struct(&found) {
                    return None;
                }
                if found == Ty::Never {
                    return Some(Ty::Never);
                }
                let result = match numeric::as_numeric(&found) {
                    Some(ty) => match (op, ty) {
                        (UnaryOp::Neg, Primitive::UInt) => Some(Primitive::Long),
                        (UnaryOp::Neg, Primitive::ULong | Primitive::NUInt) => None,
                        (UnaryOp::BitNot, ty) if numeric::is_floating_point(ty) => None,
                        _ => Some(numeric::widen_small(ty)),
                    },
                    None if !is_known(&found) => return None,
                    None => None,
                };
                self.operated(expr, op, operand, found, result)
            }
        }
    }

    /// `++` and `--`, prefix or postfix: the operand keeps its numeric type.
    fn step(&mut self, expr: ExprId, op: UnaryOp, operand: ExprId) -> Option<Ty> {
        let found = self.infer(operand)?;
        let result = numeric::as_numeric(&found);
        if result.is_none() && (!is_known(&found) || is_struct(&found)) {
            return None;
        }
        self.operated(expr, op, operand, found, result)
    }

    /// The type of a unary operation on `operand`, of type `ty`, or its
    /// error when `result` is `None`.
    fn operated(
        &mut self,
        expr: ExprId,
        op: UnaryOp,
        operand: ExprId,
        ty: Ty,
        result: Option<Primitive>,
    ) -> Option<Ty> {
        match result {
            Some(result) => {
                let result = Ty::Primitive(result);
                self.widen(operand, &ty, &result);
                Some(result)
            }
            None => {
                self.errors.push(TypeError::Unary {
                    expr,
                    op,
                    operand,
                    ty,
                });
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
            // `&&` and `||` are defined on two `bool` values (§7.12).
            // Each operand is checked on its own, and an operand of unknown
            // type leaves the result unknown.
            BinaryOp::And | BinaryOp::Or => {
                let mut known = true;
                for operand in [lhs, rhs] {
                    match self.infer(operand).filter(is_known) {
                        Some(ty) if ty != BOOL && ty != Ty::Never => {
                            self.errors.push(TypeError::Logical { op, operand, ty });
                        }
                        Some(_) => {}
                        None => known = false,
                    }
                }
                return known.then_some(BOOL);
            }
            // Option operators come with options (§7.5).
            BinaryOp::Coalesce => {
                self.infer(lhs);
                self.infer(rhs);
                return None;
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr => {
                let value = self.infer(lhs);
                let count = self.expr(rhs, Expect::Target(&INT));
                if value.as_ref().is_some_and(is_struct) || count.as_ref().is_some_and(is_struct) {
                    return None;
                }
                self.coerce(rhs, count.as_ref(), &INT, Context::Value);
                let value = value?;
                if value == Ty::Never {
                    return Some(Ty::Never);
                }
                return match numeric::as_numeric(&value).filter(|&ty| numeric::is_integer(ty)) {
                    Some(ty) => {
                        let widened = Ty::Primitive(numeric::widen_small(ty));
                        self.widen(lhs, &value, &widened);
                        Some(widened)
                    }
                    None if !is_known(&value) => None,
                    None => {
                        self.errors.push(TypeError::Binary {
                            expr,
                            op,
                            lhs,
                            rhs,
                            lhs_ty: value,
                            rhs_ty: count.unwrap_or(Ty::Error),
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
        // An operand of unknown type leaves the result unknown, a comparison's
        // too, so that nothing is reported because of it.
        let (Some(l), Some(r)) = self.operands(lhs, rhs) else {
            return None;
        };
        if !is_known(&l) || !is_known(&r) {
            return None;
        }
        // An operand of the never type takes the other's type; with two, the
        // operation is of the never type too, a comparison aside (§11.4).
        let (found_l, found_r) = (l.clone(), r.clone());
        let (l, r) = match (l, r) {
            (Ty::Never, Ty::Never) if comparison => return Some(BOOL),
            (Ty::Never, Ty::Never) => return Some(Ty::Never),
            (Ty::Never, r) => (r.clone(), r),
            (l, Ty::Never) => (l.clone(), l),
            (l, r) => (l, r),
        };
        // A struct may declare its arithmetic, bitwise and shift operators
        // (§6.9), which this milestone does not look up.
        if !comparison && (is_struct(&l) || is_struct(&r)) {
            return None;
        }
        // Comparing two tuples of one shape but different element types is
        // for the rules of tuples to decide (§5.5).
        let tuples = matches!((&l, &r), (Ty::Tuple(_), Ty::Tuple(_)));
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
            && tuples
            && same_shape(&l, &r)
            && !same(&l, &r)
        {
            return None;
        }
        match operator(op, &l, &r) {
            Some(ty) => {
                if let (Some(a), Some(b)) = (numeric::as_numeric(&l), numeric::as_numeric(&r))
                    && let Some(promoted) = numeric::promote(a, b)
                {
                    let promoted = Ty::Primitive(promoted);
                    self.widen(lhs, &found_l, &promoted);
                    self.widen(rhs, &found_r, &promoted);
                }
                Some(ty)
            }
            None => {
                self.errors.push(TypeError::Binary {
                    expr,
                    op,
                    lhs,
                    rhs,
                    lhs_ty: l,
                    rhs_ty: r,
                });
                Some(if comparison { BOOL } else { Ty::Error })
            }
        }
    }

    /// `a = b`, where the value converts to the type of the place, and the
    /// compound assignments, whose result converts back to the type of the
    /// place as a cast would (§7.12): only the operation itself is checked.
    fn assign(&mut self, expr: ExprId, op: Option<BinaryOp>, lhs: ExprId, rhs: ExprId) {
        match op {
            None => {
                let places = self.place_type(lhs);
                self.deconstruct(lhs, &places, rhs);
            }
            Some(op) => {
                let result = self.binary(expr, op, lhs, rhs);
                // The result converts back to the type of the place as a
                // cast would (§7.12).
                let place = self.exprs[arena_index(lhs)].clone();
                if let (Some(result), Some(place)) = (result, place)
                    && is_known(&result)
                    && is_known(&place)
                    && !same(&result, &place)
                {
                    self.convert(expr, place, ConversionKind::Numeric);
                }
            }
        }
    }

    /// The type of the place `place` of `=`: a tuple for a tuple of places
    /// (D-139), and `_` for a discarded element or a place of unknown type.
    fn place_type(&mut self, place: ExprId) -> Ty {
        let body = self.body;
        match &body.exprs[place] {
            Expr::Tuple(places) => Ty::Tuple(
                places
                    .iter()
                    .map(|(name, place)| TupleElem {
                        name: name.clone(),
                        ty: self.place_type(*place),
                    })
                    .collect(),
            ),
            _ => self.infer(place).unwrap_or(Ty::Error),
        }
    }

    /// `place = value`, where `places` is the type of `place`: a tuple
    /// literal gives each of its elements to the place at its position, and
    /// another value is matched against the places by its type.
    fn deconstruct(&mut self, place: ExprId, places: &Ty, value: ExprId) {
        let body = self.body;
        match (&body.exprs[place], places, &body.exprs[value]) {
            (Expr::Tuple(inner), Ty::Tuple(types), Expr::Tuple(values))
                if inner.len() == values.len() =>
            {
                for ((&(_, place), ty), &(_, value)) in inner.iter().zip(types).zip(values) {
                    self.deconstruct(place, &ty.ty, value);
                }
            }
            (Expr::Tuple(_), ..) => {
                if let Some(found) = self.infer(value) {
                    self.deconstruct_type(place, places, value, &found, &mut 0);
                }
            }
            // One place takes a value of its type; `_`, a discarded element or
            // a place of unknown type, takes any value.
            _ => self.check(value, places),
        }
    }

    /// `place = value`, where `places` is the type of `place` and `found`
    /// the type of `value`: a tuple of places takes a tuple of as many
    /// elements, each stored in its place, or a positional struct, whose
    /// deconstruction is not checked yet, as in a declaration (D-139).
    ///
    /// `leaf` counts the places that take an element, depth first, which is
    /// the position an element's conversion is recorded at.
    fn deconstruct_type(
        &mut self,
        place: ExprId,
        places: &Ty,
        value: ExprId,
        found: &Ty,
        leaf: &mut u32,
    ) {
        let body = self.body;
        let Expr::Tuple(inner) = &body.exprs[place] else {
            let index = *leaf;
            *leaf += 1;
            if !is_known(found) || !is_known(places) {
                return;
            }
            if !Context::Value.accepts(found, places) {
                self.errors.push(TypeError::Mismatch {
                    expr: value,
                    expected: places.clone(),
                    found: found.clone(),
                    context: Context::Value,
                });
            } else if !same(found, places) && *found != Ty::Never {
                let conversion = Conversion {
                    to: places.clone(),
                    kind: ConversionKind::Implicit,
                };
                let previous = self.element_conversions.insert((value, index), conversion);
                debug_assert!(previous.is_none(), "an element is recorded once");
            }
            return;
        };
        match (places, found) {
            (Ty::Tuple(types), Ty::Tuple(elems)) if elems.len() == inner.len() => {
                for ((&(_, place), ty), elem) in inner.iter().zip(types).zip(elems) {
                    self.deconstruct_type(place, &ty.ty, value, &elem.ty, leaf);
                }
            }
            (_, Ty::Struct(item)) if item_data(self.db, *item).is_positional => {}
            (_, Ty::Tuple(_) | Ty::Primitive(_) | Ty::String | Ty::Struct(_)) => {
                self.errors.push(TypeError::Mismatch {
                    expr: value,
                    expected: places.clone(),
                    found: found.clone(),
                    context: Context::Value,
                });
            }
            // A value of the never type deconstructs to anything (§11.4).
            (_, Ty::Unsupported(_) | Ty::Error | Ty::Never) => {}
        }
    }

    /// `new T(args) { F = v }`. For a struct built memberwise, each name
    /// is an instance field, assigned once, whose value is checked against
    /// its type, and every field without an initializer of its own must be
    /// assigned (§6.9); its type is the struct. A call with arguments runs
    /// a constructor, which assigns the fields. Other targets have no type
    /// yet.
    fn new_expr(
        &mut self,
        expr: ExprId,
        ty: Option<TypeId>,
        args: Option<&[Arg]>,
        inits: Option<&[(String, ExprId)]>,
    ) -> Option<Ty> {
        let has_args = args.is_some();
        if let Some(args) = args {
            self.args(args);
        }
        let written = ty.map(|ty| self.resolution.written(ty));
        // A struct type names a struct that is not generic.
        let item = match &written {
            Some(Ty::Struct(item)) => Some(*item),
            _ => None,
        };
        let (Some(written), Some(item)) = (written, item) else {
            for &(_, init) in inits.into_iter().flatten() {
                self.infer(init);
            }
            return None;
        };
        let inits = inits.unwrap_or_default();
        for (position, (name, init)) in inits.iter().enumerate() {
            match instance_fields(self.db, item).find(|(_, field)| field.name == *name) {
                Some((_, field)) => self.check(*init, &field.ty),
                None => {
                    self.infer(*init);
                }
            }
            // A constructor call assigns the members as plain assignments.
            if !has_args && let Some(error) = self.init_name(item, &written, inits, position) {
                self.errors.push(error);
            }
        }
        let missing = instance_fields(self.db, item)
            .filter(|_| !has_args)
            .find(|(_, field)| {
                field.needs_init && !inits.iter().any(|(name, _)| *name == field.name)
            });
        if let Some((_, missing)) = missing {
            self.errors.push(TypeError::MissingField {
                expr,
                ty: written.clone(),
                field: missing.name.clone(),
            });
        }
        Some(written)
    }

    /// What is wrong with the name of the initializer at `position` of
    /// `inits`, the memberwise creation of the struct `item` of type `ty`:
    /// a name that is not an instance field, or one assigned before.
    fn init_name(
        &self,
        item: ItemId,
        ty: &Ty,
        inits: &[(String, ExprId)],
        position: usize,
    ) -> Option<TypeError> {
        let (name, value) = &inits[position];
        // An initializer without a name is a syntax error.
        if name.is_empty() {
            return None;
        }
        if instance_fields(self.db, item).any(|(_, field)| field.name == *name) {
            let (_, first) = inits[..position]
                .iter()
                .find(|(earlier, _)| earlier == name)?;
            return Some(TypeError::RepeatedField {
                value: *value,
                first: *first,
                previous: inits[position - 1].1,
                ty: ty.clone(),
                name: name.clone(),
            });
        }
        let member = item_tree(self.db, item.file)
            .item(item.item)
            .values
            .iter()
            .find(|member| member.name == *name)
            .map_or(NotAField::Unknown, |member| match member.kind {
                ValueKind::Getter => NotAField::Getter,
                ValueKind::Const => NotAField::Constant,
                // A field that is not an instance field is `static`.
                ValueKind::Field => NotAField::Static,
                ValueKind::EnumMember | ValueKind::UnionCase => NotAField::Unknown,
            });
        let suggestion = if member == NotAField::Unknown {
            let unassigned: Vec<String> = instance_fields(self.db, item)
                .map(|(_, field)| field.name.clone())
                .filter(|field| !inits.iter().any(|(name, _)| name == field))
                .collect();
            closest(name, &unassigned).map(str::to_owned)
        } else {
            None
        };
        Some(TypeError::NotAField {
            value: *value,
            ty: ty.clone(),
            name: name.clone(),
            member,
            suggestion,
        })
    }

    /// `(T)x`: a cast converts between numeric types (§5.10).
    fn cast(&mut self, expr: ExprId, ty: TypeId, operand: ExprId) -> Option<Ty> {
        let to = self.resolution.written(ty);
        if let Some(from) = self.infer(operand)
            && is_known(&from)
            && is_known(&to)
        {
            if numeric::as_numeric(&from).is_some() && numeric::as_numeric(&to).is_some() {
                self.convert(operand, to.clone(), ConversionKind::Numeric);
            } else if !same(&from, &to) {
                self.errors.push(TypeError::Cast {
                    expr,
                    operand,
                    from,
                    to: to.clone(),
                });
            }
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
            self.widen(r#else, &b, &a);
            Some(a)
        } else if converts(&a, &b) {
            self.widen(then, &a, &b);
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

/// Whether two types have one shape: at every level both are tuples of one
/// length, or neither is a tuple.
fn same_shape(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_shape(&a.ty, &b.ty))
        }
        (Ty::Tuple(_), _) | (_, Ty::Tuple(_)) => false,
        _ => true,
    }
}

/// Whether `ty` is a struct, which may declare operators (§6.9).
fn is_struct(ty: &Ty) -> bool {
    matches!(ty, Ty::Struct(_))
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

/// The type `op` gives on operands of the known types `l` and `r`, or
/// `None` when it is not defined on them (§5.3, §7.12).
fn operator(op: BinaryOp, l: &Ty, r: &Ty) -> Option<Ty> {
    use BinaryOp as B;
    if let (Some(a), Some(b)) = (numeric::as_numeric(l), numeric::as_numeric(r)) {
        let ty = numeric::promote(a, b)?;
        return match op {
            B::Eq | B::Ne | B::Lt | B::Gt | B::Le | B::Ge => Some(BOOL),
            _ if numeric::is_integer_only(op) => {
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
