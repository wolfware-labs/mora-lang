//! The walk over a body that gives each expression and local its type.

mod call;
mod places;
mod static_member;

use std::cell::{Cell, OnceCell};

use la_arena::ArenaMap;
use mora_span::{Db, Project, SourceFile};
use mora_syntax::lexer::number::{
    IntSuffix, RealSuffix, int_value, real_value, too_large_for_float,
};
use rustc_hash::FxHashMap;

use super::constant::Value;
use super::{
    Callee, Constant, Context, Conversion, ConversionKind, Inference, NewTarget, NotAField,
    Propagated, TypeError, constant, converts, is_entry_return, is_known, numeric, same,
};
use crate::arena::arena_index;
use crate::bodies::{
    Arg, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, Expr, ExprId, InterpPart, Literal,
    LocalId, MatchArm, Pat, PatId, Stmt, StmtId, Target, TypeId, UnaryOp, ValueRef, body,
    body_resolution, body_scope,
};
use crate::defs::ItemId;
use crate::items::{Item, ItemKind, ItemTree, ValueKind, ValueMember, item_tree};
use crate::resolve::{Resolved, Scope, is_visible, member_reaches, value_visible};
use crate::signature::{
    Creation, creation_of, instance_field_named, instance_fields, instance_fields_in, signature,
    struct_field, this_type,
};
use crate::suggest::closest;
use crate::ty::{Construct, Field, Primitive, Signature, TupleElem, Ty};

const BOOL: Ty = Ty::Primitive(Primitive::Bool);
const INT: Ty = Ty::Primitive(Primitive::Int);

/// The implicit conversion of a value of `from` to `to`, which `to`
/// accepts: none when the types are the same, or for a value of the never
/// type, which does not exist.
fn widening(from: &Ty, to: &Ty) -> Option<Conversion> {
    (!same(from, to) && *from != Ty::Never).then(|| Conversion {
        to: to.clone(),
        kind: ConversionKind::Implicit,
    })
}

/// Whether `expr` assigns, so that the branches of a conditional that has
/// it as a branch are statements when it is one (D-141): an assignment, a
/// conditional with a branch that assigns, or a `match` with an arm that
/// assigns.
fn assigns(body: &Body, expr: ExprId) -> bool {
    match &body.exprs[expr] {
        Expr::Assign { .. } => true,
        Expr::Conditional { then, r#else, .. } => assigns(body, *then) || assigns(body, *r#else),
        Expr::Match { arms, .. } => arms.iter().any(|arm| assigns(body, arm.body)),
        _ => false,
    }
}

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
    /// A target type that is there but that this milestone does not pass
    /// down: the parameter of an argument of a call that is not checked or
    /// not resolved yet (D-137), or of one that is an error already, and
    /// what [`Expect::within`] gives a part of a value that has a target.
    /// As `None`, except that a `new` without a type is not reported.
    Later,
}

impl Expect<'_> {
    /// What a part of the value expects whose target type is the value's
    /// own or comes from it, when the value's target does not give the
    /// part one: a target that comes later stays one that comes later, and
    /// a target type, which the part does not fit, or none gives it none.
    fn within(self) -> Expect<'static> {
        match self {
            Expect::Later => Expect::Later,
            Expect::Target(_) | Expect::None | Expect::Hint(_) => Expect::None,
        }
    }
}

/// What an element of a collection expects when the collection expects
/// `expect`: the element type of an array target, a target that comes
/// later for a collection type of the library, none for a target that is
/// no collection (§7.9).
fn collection_element(expect: Expect<'_>) -> Expect<'_> {
    match expect {
        Expect::Target(Ty::Unsupported(ty)) => match (ty.construct, ty.parts.as_slice()) {
            (Construct::Array, [element]) => Expect::Target(element),
            (Construct::External | Construct::Generic | Construct::Splice, _) => Expect::Later,
            _ => Expect::None,
        },
        other => other.within(),
    }
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
        calls: FxHashMap::default(),
        arguments: FxHashMap::default(),
        local_functions: None,
        constants: FxHashMap::default(),
        literals: ArenaMap::with_capacity(body.exprs.len()),
        conversions: FxHashMap::default(),
        element_conversions: FxHashMap::default(),
        errors: Vec::new(),
        statement: None,
        splittable: FxHashMap::default(),
        block_stmt: None,
        ret: None,
        ret_later: false,
        entry: owner == BodyOwner::TopLevel,
        entry_ret: None,
        entry_value: None,
        entry_propagate: None,
        scope: body_scope(project, file, owner),
        item: None,
        tree: Cell::new(None),
        signature: Cell::new(None),
        type_positions: OnceCell::new(),
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
    check.item = item;
    match item.map(|item| signature(db, item)) {
        Some(Signature::Function(function)) => {
            // A parameter the parser recovered from declares no local, and
            // then the locals cannot be matched to the signature.
            if function.params.len() == body.params.len() {
                for (&local, param) in body.params.iter().zip(&function.params) {
                    check.locals[arena_index(local)].get_or_insert_with(|| param.ty.clone());
                }
            }
            // `return v` in an `async` function gives a value of its
            // declared type too (§12).
            expected = Some(function.ret.clone());
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
            BodyKind::Function => {
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
    check.entry_return();
    check.instance_uses(owner);
    check.types_as_values();
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
        entry_ret: check.entry_ret,
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
    calls: FxHashMap<ExprId, Callee>,
    /// The parameter of each argument of a checked call, so far.
    arguments: FxHashMap<ExprId, Vec<usize>>,
    /// The statement of each local function, found on the first call of
    /// one.
    local_functions: Option<FxHashMap<LocalId, StmtId>>,
    /// The value of each operation made only of literals so far.
    constants: FxHashMap<ExprId, Constant>,
    /// The value of each number literal and negated literal so far.
    literals: ArenaMap<ExprId, Constant>,
    /// The conversions applied so far.
    conversions: FxHashMap<ExprId, Conversion>,
    /// The conversions of deconstructed elements so far.
    element_conversions: FxHashMap<(ExprId, u32), Conversion>,
    /// The errors so far.
    errors: Vec<TypeError>,
    /// The assignment written as a statement of its own whose turn it is,
    /// whose value nothing uses: it is checked right after it is set.
    statement: Option<ExprId>,
    /// The assignments that are the value of another one written as a
    /// statement in a block, each with that statement: a fix can split them.
    splittable: FxHashMap<ExprId, ExprId>,
    /// The statement of the block being walked whose turn it is.
    block_stmt: Option<StmtId>,
    /// The type `return e` checks `e` against, `None` when it is unknown.
    ret: Option<Ty>,
    /// Whether the return type is there when `ret` is unknown: in a lambda
    /// whose target type gives no result type here, as one passed to a
    /// call that is not checked yet.
    ret_later: bool,
    /// Whether a `return` here ends the generated `Main` of top-level
    /// statements, whose return type is inferred from its `return`s (§4.6):
    /// false inside a local function or a lambda.
    entry: bool,
    /// The type the generated `Main` returns, once a `return` gives it; a
    /// `?` makes it a `Result` after the walk.
    entry_ret: Option<Ty>,
    /// The first `return` with a value of the top-level statements.
    entry_value: Option<StmtId>,
    /// The first `?` of the top-level statements.
    entry_propagate: Option<Propagated>,
    /// Where the body's names are looked up from, which decides what is
    /// visible from it.
    scope: Scope,
    /// The item whose body this is, whose receiver, in a method of a
    /// struct, gives `this` its type. The type is looked up at each `this`,
    /// which few bodies use.
    item: Option<ItemId>,
    /// The item tree read last, with its file: most items a body names are
    /// in its own file, and one read serves them all.
    tree: Cell<Option<(SourceFile, &'a ItemTree)>>,
    /// The signature read last, with its item: the members a body uses
    /// are mostly those of one struct.
    signature: Cell<Option<(ItemId, &'a Signature)>>,
    /// Whether each expression, by its index, stands where a type may,
    /// found once an expression names a type, which most bodies have none
    /// of.
    type_positions: OnceCell<Vec<bool>>,
}

impl<'a> Check<'a> {
    /// The item tree of `file`.
    fn tree(&self, file: SourceFile) -> &'a ItemTree {
        if let Some((read, tree)) = self.tree.get()
            && read == file
        {
            return tree;
        }
        let tree = item_tree(self.db, file);
        self.tree.set(Some((file, tree)));
        tree
    }

    /// The signature of `item`.
    fn signature(&self, item: ItemId) -> &'a Signature {
        if let Some((read, signature)) = self.signature.get()
            && read == item
        {
            return signature;
        }
        let read = signature(self.db, item);
        self.signature.set(Some((item, read)));
        read
    }

    /// What the item tree says about `item`.
    fn item_data(&self, item: ItemId) -> &'a Item {
        self.tree(item.file).item(item.item)
    }

    /// The value member `value` refers to.
    fn value_member(&self, value: ValueRef) -> Option<&'a ValueMember> {
        self.item_data(value.ty).values.get(value.index)
    }
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
                let expect = self.unknown_ret();
                self.expr(root, expect);
            }
        }
        self.ret = outer;
    }

    /// What a returned value of a body whose return type is unknown
    /// expects.
    fn unknown_ret(&self) -> Expect<'static> {
        if self.ret_later {
            Expect::Later
        } else {
            Expect::None
        }
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
    /// it expects `expected` (`Context::accepts`), and records its
    /// conversion when it does.
    fn coerce(&mut self, expr: ExprId, found: Option<&Ty>, expected: &Ty, context: Context) {
        if let Some(found) = found
            && self.fits_or_report(expr, found, expected, context)
        {
            self.widen(expr, found, expected);
        }
    }

    /// Whether `context` accepts `expr`, of type `found`, where it expects
    /// `expected` (`Context::accepts`); a `Mismatch` is reported when it
    /// does not. `false` without a report when either type is not known.
    fn fits_or_report(
        &mut self,
        expr: ExprId,
        found: &Ty,
        expected: &Ty,
        context: Context,
    ) -> bool {
        if !is_known(found) || !is_known(expected) {
            return false;
        }
        let fits = context.accepts(found, expected);
        if !fits {
            self.errors.push(TypeError::Mismatch {
                expr,
                expected: expected.clone(),
                found: found.clone(),
                context,
            });
        }
        fits
    }

    /// Records that `expr`, of type `from`, converts to `to` where it is
    /// used, when the types differ.
    fn widen(&mut self, expr: ExprId, from: &Ty, to: &Ty) {
        if let Some(Conversion { to, kind }) = widening(from, to) {
            self.convert(expr, to, kind);
        }
    }

    /// Records that `expr`, of type `from`, converts to the numeric type
    /// `to` where it is used, when the types differ: [`Check::widen`] to a
    /// primitive type, without comparing two whole types.
    fn widen_number(&mut self, expr: ExprId, from: &Ty, to: Primitive) {
        match from {
            Ty::Primitive(from) if *from == to => {}
            // A value of the never type does not exist.
            Ty::Never => {}
            _ => self.convert(expr, Ty::Primitive(to), ConversionKind::Implicit),
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
            self.statement = Some(expr);
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
            Expr::Match { scrutinee, arms } => self.r#match(*scrutinee, arms, None),
            // So are the branches of a conditional that assigns.
            Expr::Conditional {
                condition,
                then,
                r#else,
            } if assigns(body, expr) => {
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
            Stmt::Missing | Stmt::Break | Stmt::Continue => {}
            // In the top-level statements, `return;` gives the generated
            // `Main` the type `()`, as a `return` with a value gives it
            // that value's type (§4.6).
            Stmt::Return(None) if self.entry && self.ret.is_none() => {
                match self.entry_ret.clone() {
                    None => self.entry_ret = Some(Ty::UNIT),
                    Some(expected) if is_known(&expected) && !same(&expected, &Ty::UNIT) => {
                        self.errors.push(TypeError::MissingValue { stmt, expected });
                    }
                    Some(_) => {}
                }
            }
            Stmt::Return(None) => {}
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
                let ret = self.resolution.return_type(function.ret);
                if let Some(root) = function.body {
                    let entry = std::mem::replace(&mut self.entry, false);
                    self.function_body(root, Some(ret), true);
                    self.entry = entry;
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
                    // The first `return` of the top-level statements gives
                    // the type of the generated `Main`; the others check
                    // against it.
                    None if self.entry => match self.entry_ret.clone() {
                        Some(ty) => self.check(*expr, &ty),
                        None => {
                            self.entry_ret = self.infer(*expr);
                            self.entry_value = Some(stmt);
                        }
                    },
                    None => {
                        let expect = self.unknown_ret();
                        self.expr(*expr, expect);
                    }
                }
                self.ret = ret;
            }
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms, None),
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    /// The return type of the generated `Main`, once its statements are
    /// checked: with a `?` in them it returns a `Result` of what its
    /// `return` statements give, `()` when none gives a value, with the
    /// error type of the first `?` (§4.6). A value of a type of the standard
    /// library, which may be a `Result` already, is the return type itself.
    fn entry_return(&mut self) {
        self.entry_value_check();
        let Some(propagate) = &self.entry_propagate else {
            return;
        };
        let value = match (self.entry_ret.take(), self.entry_value) {
            (Some(value), _) => value,
            (None, None) => Ty::UNIT,
            // A value whose type the checker does not know.
            (None, Some(_)) => Ty::Error,
        };
        self.entry_ret = Some(if value.as_external().is_some() {
            value
        } else {
            let error = propagate.error.clone().unwrap_or(Ty::Error);
            Ty::external(vec![value, error])
        });
    }

    /// `MR0120` when the first `return` with a value of the top-level
    /// statements gives the generated `Main` a type an entry point cannot
    /// return (§4.6). With a `?` in the statements the generated `Main`
    /// returns a `Result` of the value, unless the value is one already.
    fn entry_value_check(&mut self) {
        let Some(stmt) = self.entry_value else {
            return;
        };
        let Stmt::Return(Some(value)) = self.body.stmts[stmt] else {
            return;
        };
        let ty = match (&self.body.exprs[value], &self.entry_ret) {
            // A lambda has no type of its own here, and is a function value
            // whichever it would get.
            (Expr::Lambda(_), _) => None,
            (_, Some(ty)) => Some(ty.clone()),
            (_, None) => return,
        };
        // The `?` changes what the generated `Main` returns, not which values
        // it accepts: a `Result<T, E>` whose `T` is the value's type takes
        // the values an entry point returns by themselves (a call that does
        // not return gives none), and a value that is a `Result` already is
        // the return type either way.
        if !ty.as_ref().is_some_and(is_entry_return) {
            self.errors.push(TypeError::EntryReturn {
                stmt,
                ty,
                propagate: self.entry_propagate.clone(),
            });
        }
    }

    /// A `match`: without `expect`, in statement position, its arms are
    /// statements, whose values nothing uses; as a value, each arm checks
    /// against the target type, or expects what [`Expect::within`] gives of
    /// `expect`.
    fn r#match(&mut self, scrutinee: ExprId, arms: &[MatchArm], expect: Option<Expect<'_>>) {
        self.infer(scrutinee);
        for arm in arms {
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.condition(guard);
            }
            match expect {
                None => self.statement(arm.body, false),
                Some(Expect::Target(target)) => self.check(arm.body, target),
                Some(expect) => {
                    self.expr(arm.body, expect.within());
                }
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

    /// The arguments of a call, a creation or an index that this milestone
    /// does not check. A lambda or a `new` without a type among them takes
    /// its target type when a later milestone checks them.
    fn args(&mut self, args: &[Arg]) {
        for arg in args {
            self.expr(arg.expr, Expect::Later);
        }
    }

    /// The type of `expr`, recorded for it. A mistake keeps the type
    /// expected at its position (§14.5, D-159): an expression of the error
    /// type, which a diagnostic explains, has its target type when it has
    /// one. A discarded element takes any type, so it keeps none.
    fn expr(&mut self, expr: ExprId, expect: Expect<'_>) -> Option<Ty> {
        if let Expect::Target(target) = expect
            && let Some(target) = numeric::as_numeric(target)
            && let Some(ty) = self.constant(expr, target)
        {
            return Some(ty);
        }
        let mut ty = self.expr_kind(expr, expect);
        // A type or a namespace is no value (`MR0127`, D-154).
        if ty.is_none() && self.named_as_value(expr).is_some() {
            ty = Some(Ty::Error);
        }
        if let Expect::Target(target) = expect
            && ty == Some(Ty::Error)
            && !matches!(self.body.exprs[expr], Expr::Discard)
        {
            ty = Some(target.clone());
        }
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
        if !target.is_floating_point()
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
                let element = collection_element(expect);
                for &(_, elem) in elems {
                    self.expr(elem, element);
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
                if self.statement.take() != Some(expr) {
                    let split = self.splittable.get(&expr).copied();
                    self.errors.push(TypeError::AssignmentValue { expr, split });
                }
                self.assign(expr, *op, *lhs, *rhs);
                None
            }
            Expr::Propagate(inner) => {
                let operand = self.infer(*inner);
                if self.entry && self.entry_propagate.is_none() {
                    // The error type of a `Result`, the second of the two
                    // arguments the checker sees of it.
                    let error = match operand.as_ref().and_then(Ty::as_external) {
                        Some([_, error]) => Some(error.clone()),
                        _ => None,
                    };
                    self.entry_propagate = Some(Propagated { expr, error });
                }
                None
            }
            Expr::Await(inner) | Expr::As { expr: inner, .. } => {
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
                self.r#match(*scrutinee, arms, Some(expect));
                None
            }
            Expr::With { expr: value, inits } => {
                let ty = self.infer(*value);
                for (name, init) in inits {
                    match self.field(ty.as_ref(), name) {
                        Some(field) => self.check(*init, &field),
                        None => {
                            self.expr(*init, Expect::Later);
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
                // A lambda whose target a later milestone checks has its
                // result type then.
                let later = matches!(expect, Expect::Later);
                let outer = std::mem::replace(&mut self.ret_later, later);
                let entry = std::mem::replace(&mut self.entry, false);
                self.function_body(lambda.body, ret, false);
                self.entry = entry;
                self.ret_later = outer;
                None
            }
            Expr::New { ty, args, inits } => {
                self.new_expr(expr, *ty, args.as_deref(), inits.as_deref(), expect)
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
            Expr::This => self.item.and_then(|item| this_type(self.db, item)),
            Expr::Type(_) | Expr::Default(_) | Expr::Macro => None,
        }
    }

    /// The type of a value a path or a member of a type names.
    fn target(&self, target: &Target) -> Option<Ty> {
        match target {
            Target::Error => Some(Ty::Error),
            // Only a constant or a static has a value signature.
            Target::Item(Resolved::Item(item)) => self.signature(*item).as_value().cloned(),
            Target::Item(_) => None,
            // A getter has the type its body declares.
            Target::Value(value) => {
                let member = self.value_member(*value)?;
                self.struct_field(value.ty, &member.name).or_else(|| {
                    (member.kind == ValueKind::Getter)
                        .then(|| self.getter_type(value.ty, &member.name))
                        .flatten()
                })
            }
        }
    }

    /// The struct and the name of `expr` when it is a member of a value of
    /// a struct type, as `p.Scale`; `None` for any other expression.
    pub(super) fn struct_member(&self, expr: ExprId) -> Option<(ItemId, &str)> {
        let Expr::Member { receiver, name, .. } = &self.body.exprs[expr] else {
            return None;
        };
        let ty = self.exprs[arena_index(*receiver)].as_ref()?.as_struct()?;
        Some((ty, name.as_str()))
    }

    /// The type of the field `name` of a value of type `receiver`, when it
    /// is a struct with that field.
    fn field(&self, receiver: Option<&Ty>, name: &str) -> Option<Ty> {
        self.struct_field(receiver?.as_struct()?, name)
    }

    fn struct_field(&self, item: ItemId, name: &str) -> Option<Ty> {
        match self.signature(item) {
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
            match self.expr(*elem, expect.within()) {
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
                    Expect::None | Expect::Later => (None, false),
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
                        (UnaryOp::BitNot, ty) if ty.is_floating_point() => None,
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
        let found = self.infer(operand);
        self.require_place(operand, false);
        let found = found?;
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
                self.widen_number(operand, &ty, result);
                Some(Ty::Primitive(result))
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
                return match numeric::as_numeric(&value).filter(|ty| ty.is_integer()) {
                    Some(ty) => {
                        let widened = numeric::widen_small(ty);
                        self.widen_number(lhs, &value, widened);
                        Some(Ty::Primitive(widened))
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
        let comparison = op.is_comparison();
        // An operand of unknown type leaves the result unknown, a comparison's
        // too, so that nothing is reported because of it.
        let (Some(found_l), Some(found_r)) = self.operands(lhs, rhs) else {
            return None;
        };
        if !is_known(&found_l) || !is_known(&found_r) {
            return None;
        }
        // An operand of the never type takes the other's type; with two, the
        // operation is of the never type too, a comparison aside (§11.4).
        let (l, r) = match (&found_l, &found_r) {
            (Ty::Never, Ty::Never) if comparison => return Some(BOOL),
            (Ty::Never, Ty::Never) => return Some(Ty::Never),
            (Ty::Never, r) => (r, r),
            (l, Ty::Never) => (l, l),
            (l, r) => (l, r),
        };
        // A struct may declare its arithmetic, bitwise and shift operators
        // (§6.9), which this milestone does not look up.
        if !comparison && (is_struct(l) || is_struct(r)) {
            return None;
        }
        // Comparing two tuples of one shape but different element types is
        // for the rules of tuples to decide (§5.5).
        let tuples = matches!((l, r), (Ty::Tuple(_), Ty::Tuple(_)));
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && tuples && same_shape(l, r) && !same(l, r) {
            return None;
        }
        match operator(op, l, r) {
            Some((ty, promoted)) => {
                if let Some(promoted) = promoted {
                    self.widen_number(lhs, &found_l, promoted);
                    self.widen_number(rhs, &found_r, promoted);
                }
                Some(ty)
            }
            None => {
                self.errors.push(TypeError::Binary {
                    expr,
                    op,
                    lhs,
                    rhs,
                    lhs_ty: l.clone(),
                    rhs_ty: r.clone(),
                });
                Some(if comparison { BOOL } else { Ty::Error })
            }
        }
    }

    /// `a = b`, where the value converts to the type of the place, and the
    /// compound assignments, whose result converts back to the type of the
    /// place as a cast would (§7.12): only the operation itself is checked.
    fn assign(&mut self, expr: ExprId, op: Option<BinaryOp>, lhs: ExprId, rhs: ExprId) {
        let tuple = matches!(self.body.exprs[lhs], Expr::Tuple(_));
        match op {
            None => {
                let places = self.place_type(lhs);
                self.require_place(lhs, true);
                self.deconstruct(lhs, &places, rhs);
            }
            // `??=` waits for options (M3.5) whatever its left side is.
            Some(op) if tuple && op != BinaryOp::Coalesce => {
                self.tuple_operation(expr, op, lhs, rhs);
            }
            Some(op) => {
                let result = self.binary(expr, op, lhs, rhs);
                // A tuple of places is for `=` only; `??=` on one, the only
                // tuple that gets here, waits for options (M3.5).
                if !tuple {
                    self.require_place(lhs, false);
                }
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

    /// A compound `op=` with a tuple on the left, whose elements are not
    /// checked as places (§7.11, D-155): the operation is checked as
    /// [`Check::binary`] checks it. When an element of a type not known yet,
    /// as `_`, kept that silent, no operator takes the tuple: `MR0204`. It
    /// is not reported when either operand has an error already, a name in
    /// the left tuple that did not resolve included, nor when the right side
    /// is a struct, whose operators come with M3.6. Arithmetic needs the
    /// right side's type too; a tuple is never shifted, so a shift is
    /// decided from the left side alone, whatever its count is.
    fn tuple_operation(&mut self, expr: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) {
        let reported = self.errors.len();
        self.binary(expr, op, lhs, rhs);
        let operation = self.errors[reported..]
            .iter()
            .any(|error| matches!(error, TypeError::Binary { expr: at, .. } if *at == expr));
        if operation || self.has_element_error(lhs) {
            return;
        }
        let rhs_ty = self.exprs[arena_index(rhs)].clone();
        if rhs_ty.as_ref().is_some_and(is_struct) {
            return;
        }
        let shift = op.is_shift();
        if !shift && (self.errors.len() > reported || !rhs_ty.as_ref().is_some_and(is_known)) {
            return;
        }
        self.errors.push(TypeError::Binary {
            expr,
            op,
            lhs,
            rhs,
            lhs_ty: self.exprs[arena_index(lhs)].clone().unwrap_or(Ty::Error),
            rhs_ty: rhs_ty.unwrap_or(Ty::Error),
        });
    }

    /// Whether an element of the tuple `tuple`, inferred without a target
    /// type, at any depth, is a mistake reported on its own: an element of
    /// the error type, as a name that did not resolve or a type, a type
    /// keyword or a namespace (`MR0127`, D-154), which [`Check::expr`]
    /// gives it. A discarded element `_` is not one, nor is an element
    /// whose type is not known yet, as a collection or a library call.
    fn has_element_error(&self, tuple: ExprId) -> bool {
        let Expr::Tuple(elems) = &self.body.exprs[tuple] else {
            return false;
        };
        elems.iter().any(|&(_, elem)| match &self.body.exprs[elem] {
            Expr::Tuple(_) => self.has_element_error(elem),
            Expr::Discard => false,
            _ => self.exprs[arena_index(elem)] == Some(Ty::Error),
        })
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
            // `_` discards any value, which has no target type.
            (Expr::Discard, ..) => {
                self.infer(value);
            }
            // One place takes a value of its type; a place of unknown type
            // takes any value.
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
            if self.fits_or_report(value, found, places, Context::Value)
                && let Some(conversion) = widening(found, places)
            {
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
            (_, Ty::Struct(item)) if self.item_data(*item).is_positional => {}
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
    /// is an instance field visible from the body, assigned once, whose
    /// value is checked against its type, and every field without an
    /// initializer of its own must be visible and assigned (§6.9); its type
    /// is the struct. Empty parentheses on a struct without a constructor
    /// are memberwise creation, and braces alone on a struct with one, or
    /// on a positional struct, are an error. A call of a constructor
    /// assigns the fields itself, and the names after it are plain
    /// assignments. `new() { .. }` without a type does the same where
    /// `expect` gives the target type. The type written or taken from the
    /// target must be one `new` creates (`MR0154`). A class or a generic
    /// struct has its type, and nothing more is checked until classes and
    /// generic code.
    fn new_expr(
        &mut self,
        expr: ExprId,
        ty: Option<TypeId>,
        args: Option<&[Arg]>,
        inits: Option<&[(String, ExprId)]>,
        expect: Expect<'_>,
    ) -> Option<Ty> {
        if let Some(args) = args {
            self.args(args);
        }
        let written = match (ty, expect) {
            (Some(ty), _) => Some(self.resolution.written(ty)),
            (None, Expect::Target(target)) => Some(target.clone()),
            (None, Expect::Later) => None,
            (None, Expect::Hint(_) | Expect::None) => {
                self.errors.push(TypeError::NewTarget {
                    expr,
                    problem: NewTarget::Missing,
                });
                None
            }
        };
        // What `new` cannot create has no type.
        let written = written.and_then(|written| {
            if self.creatable(&written) {
                return Some(written);
            }
            let problem = match ty {
                Some(_) => NewTarget::Written(written),
                None => NewTarget::NotCreatable(written),
            };
            self.errors.push(TypeError::NewTarget { expr, problem });
            None
        });
        // A struct type names a struct that is not generic. A class and a
        // generic struct have their type, and a later milestone checks
        // their creation.
        // The fields of a type that is an error, or that a later milestone
        // checks, have their types then.
        let Some(written @ Ty::Struct(item)) = written else {
            for &(_, init) in inits.into_iter().flatten() {
                self.expr(init, Expect::Later);
            }
            return written;
        };
        let inits = inits.unwrap_or_default();
        let creation = creation_of(self.item_data(item), args);
        match creation {
            Creation::Memberwise => self.memberwise(expr, item, &written, inits),
            Creation::MemberwiseOff => {
                let writable = self.writable_after_constructor(item, inits);
                self.errors.push(TypeError::MemberwiseOff {
                    expr,
                    item,
                    written: ty,
                    from: self.scope,
                    writable,
                });
                // The names are checked as after the constructor call that
                // the fix writes.
                self.after_constructor(item, &written, inits, creation);
            }
            Creation::Constructor | Creation::Positional => {
                self.after_constructor(item, &written, inits, creation);
            }
            Creation::NoConstructor => {
                self.errors.push(TypeError::NoConstructor {
                    expr,
                    ty: written.clone(),
                });
                self.assigned_values(item, inits);
            }
        }
        Some(written)
    }

    /// Whether `new` creates a value of `ty`: a struct or a class, generic
    /// or not (§7.3). A type this milestone knows nothing of, and one that
    /// is an error already, count as such, so nothing more is reported.
    fn creatable(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Struct(_) | Ty::Error => true,
            Ty::Unsupported(ty) => match ty.construct {
                Construct::Class | Construct::External | Construct::Splice => true,
                Construct::Generic => ty.item.is_some_and(|item| {
                    matches!(
                        self.item_data(item).kind,
                        ItemKind::Struct | ItemKind::Class
                    )
                }),
                Construct::TypeParam
                | Construct::Union
                | Construct::Enum
                | Construct::Interface
                | Construct::Array
                | Construct::Optional
                | Construct::Function
                | Construct::Weak
                | Construct::Any => false,
            },
            Ty::Primitive(_) | Ty::String | Ty::Tuple(_) | Ty::Never => false,
        }
    }

    /// The assignments `inits` after a call of a constructor of the struct
    /// `item` of type `ty`, which are plain assignments (§6.9): each value
    /// is checked against the type of the field it names, a hidden field is
    /// `MR0129`, and, after a declared constructor, a name that is not an
    /// instance field is `MR0147`. A field may be assigned twice.
    /// `creation` is how the expression creates the value: the braces
    /// alone that `MR0153` reports, [`Creation::MemberwiseOff`], stay an
    /// error whatever the fields' visibility.
    fn after_constructor(
        &mut self,
        item: ItemId,
        ty: &Ty,
        inits: &[(String, ExprId)],
        creation: Creation,
    ) {
        // A positional struct's constructor is the one its parameters
        // declare (§6.5).
        let positional = self.item_data(item).is_positional;
        for (position, (name, init)) in inits.iter().enumerate() {
            match self.assigned_field(item, name, *init) {
                Some(index) => {
                    if !self.field_visible(item, index) {
                        // Widening the field fixes the assignment only where
                        // the assignment may write it, and once: the first
                        // assignment carries the fix.
                        let fixable = creation != Creation::MemberwiseOff
                            && self.field_writable(item, index)
                            && assigned_before(inits, position).is_none();
                        self.errors.push(TypeError::HiddenField {
                            value: *init,
                            item,
                            index,
                            name: name.clone(),
                            from: self.scope,
                            fixable,
                        });
                    }
                }
                // The positional fields are not instance fields until M2.2,
                // so a name that is no instance field may still be one of
                // them.
                None if positional => {}
                None => {
                    if let Some(error) = self.not_a_field(item, ty, inits, position, false) {
                        self.errors.push(error);
                    }
                }
            }
        }
    }

    /// Whether every name of `inits`, assigned in the creation of the struct
    /// `item`, is an instance field that the body sees and may write once
    /// the value exists, as the assignments after a constructor call need
    /// (§6.9).
    fn writable_after_constructor(&self, item: ItemId, inits: &[(String, ExprId)]) -> bool {
        inits.iter().all(|(name, _)| {
            instance_fields(self.db, item).any(|(index, field)| {
                field.name == *name && field.writable && self.field_visible(item, index)
            })
        })
    }

    /// Checks each value of `inits` against the type of the instance field
    /// of the struct `item` it names, and infers the others, without
    /// looking at the names.
    fn assigned_values(&mut self, item: ItemId, inits: &[(String, ExprId)]) {
        for (name, init) in inits {
            self.assigned_field(item, name, *init);
        }
    }

    /// Checks `value`, assigned to `name` in the creation of the struct
    /// `item`, against the type of the instance field `name`, and gives the
    /// field's index among the value members; infers `value` and gives
    /// `None` when `name` is no instance field.
    fn assigned_field(&mut self, item: ItemId, name: &str, value: ExprId) -> Option<u32> {
        self.assign_field(instance_field_named(self.db, item, name), value)
    }

    /// Checks `value` against the type of `field`, an instance field with
    /// its index among the value members, and gives that index; infers
    /// `value` and gives `None` when there is no such field.
    fn assign_field(&mut self, field: Option<(u32, &Field)>, value: ExprId) -> Option<u32> {
        match field {
            Some((index, field)) => {
                self.check(value, &field.ty);
                Some(index)
            }
            None => {
                self.expr(value, Expect::Later);
                None
            }
        }
    }

    /// The memberwise creation `expr` of the struct `item`, of type `ty`,
    /// with `inits` (§6.9).
    fn memberwise(&mut self, expr: ExprId, item: ItemId, ty: &Ty, inits: &[(String, ExprId)]) {
        // The instance fields, and whether the body sees each, looked up
        // once for all the initializers: the struct's own visibility once
        // for all its fields.
        let values = &self.item_data(item).values;
        let fields: Vec<(u32, &Field)> = instance_fields_in(self.signature(item), values).collect();
        let seen = is_visible(self.db, item, self.scope);
        let visible: Vec<bool> = fields
            .iter()
            .map(|&(index, _)| {
                let visibility = values[index as usize].visibility;
                seen && member_reaches(self.db, item, visibility, self.scope)
            })
            .collect();
        // Memberwise creation needs every required field visible (`MR0144`);
        // where one is hidden, that error replaces `MR0129` for the hidden
        // fields assigned and `MR0143` for those left out.
        let hidden = fields
            .iter()
            .zip(&visible)
            .find(|&(&(_, field), &visible)| field.needs_init && !visible)
            .map(|(&field, _)| field);
        for (position, (name, init)) in inits.iter().enumerate() {
            let found = fields.iter().position(|(_, field)| field.name == *name);
            let field = found.map(|found| fields[found]);
            // A repeated field is reported as repeated (`MR0148`) alone: its
            // first assignment reports it hidden.
            if let Some(index) = self.assign_field(field, *init)
                && hidden.is_none()
                && assigned_before(inits, position).is_none()
                && found.is_some_and(|found| !visible[found])
            {
                self.errors.push(TypeError::HiddenField {
                    value: *init,
                    item,
                    index,
                    name: name.clone(),
                    from: self.scope,
                    fixable: true,
                });
            }
            // A name that is a field is wrong only when assigned before.
            let error = match field {
                Some(_) => repeated_field(ty, inits, position),
                None => self.not_a_field(item, ty, inits, position, true),
            };
            if let Some(error) = error {
                self.errors.push(error);
            }
        }
        if let Some((_, hidden)) = hidden {
            self.errors.push(TypeError::NoMemberwise {
                expr,
                ty: ty.clone(),
                field: hidden.name.clone(),
                visibility: hidden.visibility,
            });
            return;
        }
        let missing = fields.iter().find(|(_, field)| {
            field.needs_init && !inits.iter().any(|(name, _)| *name == field.name)
        });
        if let Some((_, missing)) = missing {
            self.errors.push(TypeError::MissingField {
                expr,
                ty: ty.clone(),
                field: missing.name.clone(),
            });
        }
    }

    /// Whether the instance field at `index` of the struct `item` is
    /// declared `mut` without a write scope, so an assignment after a
    /// constructor may write it (§6.9).
    fn field_writable(&self, item: ItemId, index: u32) -> bool {
        struct_field(self.db, item, index).is_some_and(|(_, field)| field.writable)
    }

    /// Whether the body sees the value member at `index` of the struct
    /// `item`, one of its instance fields (§4.5).
    fn field_visible(&self, item: ItemId, index: u32) -> bool {
        let member = &self.item_data(item).values[index as usize];
        value_visible(self.db, item, member, self.scope)
    }

    /// `MR0147` for the initializer at `position` of `inits`, the creation
    /// of the struct `item` of type `ty`, whose name is not an instance
    /// field; `memberwise` when the creation is memberwise rather than a
    /// constructor call. A name that is no member gets the closest visible
    /// field as a suggestion: one not assigned yet in memberwise creation,
    /// which assigns each field once, and after a constructor one the
    /// assignment may write. `None` for an initializer without a
    /// name, which is a syntax error.
    fn not_a_field(
        &self,
        item: ItemId,
        ty: &Ty,
        inits: &[(String, ExprId)],
        position: usize,
        memberwise: bool,
    ) -> Option<TypeError> {
        let (name, value) = &inits[position];
        if name.is_empty() {
            return None;
        }
        let member = self
            .item_data(item)
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
            // Only a field the use can see is worth suggesting, and after a
            // constructor only one the assignment may write.
            let candidates: Vec<String> = instance_fields(self.db, item)
                .filter(|&(index, field)| {
                    self.field_visible(item, index) && (memberwise || field.writable)
                })
                .map(|(_, field)| field.name.clone())
                .filter(|field| !memberwise || !inits.iter().any(|(name, _)| name == field))
                .collect();
            closest(name, &candidates).map(str::to_owned)
        } else {
            None
        };
        Some(TypeError::NotAField {
            value: *value,
            ty: ty.clone(),
            name: name.clone(),
            member,
            suggestion,
            memberwise,
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
        let (a, b) = match expect {
            Expect::Later => (
                self.expr(then, Expect::Later),
                self.expr(r#else, Expect::Later),
            ),
            _ => self.operands(then, r#else),
        };
        let (Some(a), Some(b)) = (a, b) else {
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

/// `MR0148` for the initializer at `position` of `inits`, in the memberwise
/// creation of a struct of type `ty`, when it assigns a field that one
/// before it assigns too.
fn repeated_field(ty: &Ty, inits: &[(String, ExprId)], position: usize) -> Option<TypeError> {
    let (name, value) = &inits[position];
    Some(TypeError::RepeatedField {
        value: *value,
        first: assigned_before(inits, position)?,
        previous: inits[position - 1].1,
        ty: ty.clone(),
        name: name.clone(),
    })
}

/// The value of the first initializer of `inits` that assigns the name of
/// the one at `position`, when one before it does.
fn assigned_before(inits: &[(String, ExprId)], position: usize) -> Option<ExprId> {
    let (name, _) = &inits[position];
    inits[..position]
        .iter()
        .find(|(earlier, _)| earlier == name)
        .map(|&(_, value)| value)
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

/// The type `op` gives on operands of the known types `l` and `r`, with
/// the numeric type both operands are promoted to when they are numbers,
/// or `None` when it is not defined on them (§5.3, §7.12).
fn operator(op: BinaryOp, l: &Ty, r: &Ty) -> Option<(Ty, Option<Primitive>)> {
    use BinaryOp as B;
    if let (Some(a), Some(b)) = (numeric::as_numeric(l), numeric::as_numeric(r)) {
        let ty = numeric::promote(a, b)?;
        let result = match op {
            B::Eq | B::Ne | B::Lt | B::Gt | B::Le | B::Ge => BOOL,
            _ if numeric::is_integer_only(op) && !ty.is_integer() => return None,
            _ => Ty::Primitive(ty),
        };
        return Some((result, Some(ty)));
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
    let result = match op {
        B::Add => Ty::String,
        _ => BOOL,
    };
    Some((result, None))
}
