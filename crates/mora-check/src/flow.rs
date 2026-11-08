//! The control flow of one body: which code is reachable, which locals are
//! assigned where, and where loops are (§7.10, §7.11).
//!
//! The walk follows the order of evaluation and keeps one [`State`] for the
//! path it is on. Where paths meet, after `if`, `&&`, `match` or a loop, the
//! states join: a local is definitely assigned when it is on every path that
//! reaches the point, and may be assigned when it is on one.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_hir::{
    Arg, Body, BodyOwner, BodyResolution, BodySourceMap, Expr, ExprId, InterpPart, ItemId, Literal,
    LocalId, LocalKind, MatchArm, Pat, PatId, Resolved, Signature, Stmt, StmtId, Target, Ty,
    UnaryOp, body_resolution, item_tree, signature,
};
use mora_span::{Db, Project, SourceFile, Span, TextRange, TextSize};

/// The names of the standard library's functions of the never type (§11.4),
/// which a path ends at. Until the standard library is loaded they are
/// recognised by name (D-138).
const DIVERGING: [&str; 3] = ["panic", "Unreachable", "Todo"];

/// A set of locals, by their index.
#[derive(Clone, PartialEq, Eq)]
struct Locals(Vec<u64>);

impl Locals {
    fn new(len: usize, full: bool) -> Self {
        Locals(vec![if full { u64::MAX } else { 0 }; len.div_ceil(64)])
    }

    fn contains(&self, local: LocalId) -> bool {
        let index = local_index(local);
        self.0[index / 64] & (1 << (index % 64)) != 0
    }

    fn insert(&mut self, local: LocalId) {
        let index = local_index(local);
        self.0[index / 64] |= 1 << (index % 64);
    }

    fn remove(&mut self, local: LocalId) {
        let index = local_index(local);
        self.0[index / 64] &= !(1 << (index % 64));
    }

    fn intersect(&mut self, other: &Locals) {
        for (word, other) in self.0.iter_mut().zip(&other.0) {
            *word &= other;
        }
    }

    fn union(&mut self, other: &Locals) {
        for (word, other) in self.0.iter_mut().zip(&other.0) {
            *word |= other;
        }
    }
}

/// The position of `local` in its body's arena.
fn local_index(local: LocalId) -> usize {
    u32::from(local.into_raw()) as usize
}

/// What holds on one path through a body.
#[derive(Clone)]
struct State {
    /// Whether the path can be taken at all.
    reachable: bool,
    /// The locals assigned on it, for certain.
    assigned: Locals,
    /// The locals that may be assigned on it.
    maybe: Locals,
}

impl State {
    /// A path that cannot be taken: everything counts as assigned on it, so
    /// unreachable code reports nothing (D-138).
    fn unreachable(len: usize) -> Self {
        State {
            reachable: false,
            assigned: Locals::new(len, true),
            maybe: Locals::new(len, false),
        }
    }

    /// The state where this path and `other` meet.
    fn join(&mut self, other: &State) {
        if !other.reachable {
            return;
        }
        if !self.reachable {
            *self = other.clone();
            return;
        }
        self.assigned.intersect(&other.assigned);
        self.maybe.union(&other.maybe);
    }
}

/// A loop that `break` and `continue` leave.
struct Loop {
    /// Where the paths that `break` out of it meet.
    breaks: State,
    /// Where the paths that `continue` it meet.
    continues: State,
}

/// The function a part of the body returns from.
#[derive(Clone)]
struct Function {
    /// Its name, for messages.
    name: String,
    /// Whether it returns a value, so that each path must end in `return`
    /// with one (`MR0184`).
    returns_value: bool,
}

/// Checks the body of `owner` and pushes its errors to `out`.
pub(crate) fn check(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
    body: &Body,
    map: &BodySourceMap,
    out: &mut Vec<Diagnostic>,
) {
    let Some(root) = body.root else {
        return;
    };
    let len = body.locals.len();
    // A local is assigned where it is declared, unless it is declared
    // without a value: `let x;` and `var x: T;`.
    let mut unassigned = Locals::new(len, false);
    for (_, stmt) in body.stmts.iter() {
        if let Stmt::Local { local, init: None } = stmt {
            unassigned.insert(*local);
        }
    }
    let mut assigned = Locals::new(len, true);
    for (local, _) in body.locals.iter() {
        if unassigned.contains(local) {
            assigned.remove(local);
        }
    }
    let mut flow = Flow {
        db,
        file,
        body,
        map,
        resolution: body_resolution(db, project, file, owner),
        state: State {
            reachable: true,
            maybe: Locals::new(len, false),
            assigned,
        },
        unassigned,
        depth: vec![0; len],
        closure: 0,
        loops: Vec::new(),
        function: function(db, project, file, owner),
        reported: Locals::new(len, false),
        out,
    };
    for &(_, default) in &body.defaults {
        flow.expr(default);
    }
    if let Some(call) = body.this_call {
        flow.expr(call);
    }
    flow.function_body(root);
}

/// The function `owner` is, when it returns a value: an item whose
/// signature gives its return type. Other bodies, whose return types come
/// with their kinds of member, are not checked yet (D-138).
fn function(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Function {
    let BodyOwner::Item(item) = owner else {
        return Function {
            name: String::new(),
            returns_value: false,
        };
    };
    let id = ItemId {
        project,
        file,
        item,
    };
    let returns_value = match signature(db, id) {
        Signature::Function(function) => returns_value(&function.ret),
        _ => false,
    };
    Function {
        name: item_tree(db, file).item(item).name.clone(),
        returns_value,
    }
}

/// Whether a function of return type `ret` returns a value. A type that
/// could not be lowered has its own diagnostic.
fn returns_value(ret: &Ty) -> bool {
    *ret != Ty::UNIT && *ret != Ty::Error
}

/// The walk over one body.
struct Flow<'a> {
    /// The database the names are read from.
    db: &'a dyn Db,
    /// The file of the body, whose text gives the fixes.
    file: SourceFile,
    /// The body.
    body: &'a Body,
    /// Where its parts are.
    map: &'a BodySourceMap,
    /// What its names refer to.
    resolution: &'a BodyResolution,
    /// The state of the path the walk is on.
    state: State,
    /// The locals declared without a value.
    unassigned: Locals,
    /// How many closures each local is declared inside.
    depth: Vec<u32>,
    /// How many closures the walk is inside.
    closure: u32,
    /// The loops around the walk, innermost last.
    loops: Vec<Loop>,
    /// The function the walk is in.
    function: Function,
    /// The locals already reported as read before they are assigned.
    reported: Locals,
    /// The errors.
    out: &'a mut Vec<Diagnostic>,
}

impl Flow<'_> {
    fn span(&self, range: TextRange) -> Span {
        Span::new(self.file.id(self.db), range)
    }

    fn unreachable(&self) -> State {
        State::unreachable(self.body.locals.len())
    }

    /// The body of a function or a lambda: a block, whose end must not be
    /// reachable when the function returns a value, or an expression.
    fn function_body(&mut self, root: ExprId) {
        let Expr::Block(stmts) = &self.body.exprs[root] else {
            self.expr(root);
            return;
        };
        self.block(stmts);
        if self.state.reachable && self.function.returns_value {
            self.end_without_value(root);
        }
    }

    /// `MR0184` at the end of the block `root`.
    fn end_without_value(&mut self, root: ExprId) {
        let range = self.map.expr(root);
        let text = self.file.text(self.db);
        // A block the parser recovered from has no closing brace to point at.
        if !text
            .get(std::ops::Range::<usize>::from(range))
            .is_some_and(|text| text.ends_with('}'))
        {
            return;
        }
        let brace = TextRange::new(range.end() - TextSize::of('}'), range.end());
        let name = &self.function.name;
        self.out.push(
            Diagnostic::error(
                codes::MR0184,
                format!("`{name}` can reach its end without returning a value"),
                Label::new(self.span(brace), "the end is reachable"),
            )
            .with_note(note_mr0184(name)),
        );
    }

    fn block(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        let body = self.body;
        match &body.stmts[stmt] {
            Stmt::Missing => {}
            Stmt::Expr(expr) => self.expr(*expr),
            Stmt::Local { local, init } => {
                self.depth[local_index(*local)] = self.closure;
                match init {
                    Some(init) => self.expr(*init),
                    // Each run of the declaration, as in a loop, starts a
                    // local that is not assigned yet.
                    None if self.state.reachable => {
                        self.state.assigned.remove(*local);
                        self.state.maybe.remove(*local);
                    }
                    None => {}
                }
            }
            Stmt::Deconstruct { pat, init } => {
                self.expr(*init);
                self.pat(*pat);
            }
            Stmt::Function(function) => {
                let ret = function.ret.map_or(Ty::UNIT, |ty| {
                    self.resolution
                        .types
                        .get(u32::from(ty.into_raw()) as usize)
                        .cloned()
                        .unwrap_or(Ty::Error)
                });
                let inner = Function {
                    name: body.locals[function.local].name.clone(),
                    returns_value: returns_value(&ret),
                };
                for &(_, default) in &function.defaults {
                    self.expr(default);
                }
                if let Some(root) = function.body {
                    self.closure(&function.params, root, inner);
                }
            }
            Stmt::Block(stmts) => self.block(stmts),
            Stmt::If {
                condition,
                then,
                r#else,
            } => {
                self.expr(*condition);
                let before = self.state.clone();
                self.stmt(*then);
                let after_then = std::mem::replace(&mut self.state, before);
                if let Some(r#else) = r#else {
                    self.stmt(*r#else);
                }
                self.state.join(&after_then);
            }
            Stmt::While { condition, body } => {
                self.enter_loop(&[*condition], &[*body]);
                self.expr(*condition);
                let exit = (!is_true(self.body, *condition)).then(|| self.state.clone());
                self.loops.push(self.fresh_loop());
                self.stmt(*body);
                self.leave_loop(exit);
            }
            Stmt::DoWhile { body, condition } => {
                self.enter_loop(&[*condition], &[*body]);
                self.loops.push(self.fresh_loop());
                self.stmt(*body);
                let continues = self.continues();
                self.state.join(&continues);
                self.expr(*condition);
                let exit = (!is_true(self.body, *condition)).then(|| self.state.clone());
                self.leave_loop(exit);
            }
            Stmt::For {
                init,
                condition,
                update,
                body: loop_body,
            } => {
                self.block(init);
                let mut exprs: Vec<_> = condition.iter().copied().collect();
                exprs.extend(update);
                self.enter_loop(&exprs, &[*loop_body]);
                let exit = match condition {
                    Some(condition) => {
                        self.expr(*condition);
                        (!is_true(self.body, *condition)).then(|| self.state.clone())
                    }
                    None => None,
                };
                self.loops.push(self.fresh_loop());
                self.stmt(*loop_body);
                let continues = self.continues();
                self.state.join(&continues);
                for &expr in update {
                    self.expr(expr);
                }
                self.leave_loop(exit);
            }
            Stmt::Foreach {
                pat,
                iterable,
                body: loop_body,
                ..
            } => {
                self.expr(*iterable);
                self.enter_loop(&[], &[*loop_body]);
                let exit = Some(self.state.clone());
                self.pat(*pat);
                self.loops.push(self.fresh_loop());
                self.stmt(*loop_body);
                self.leave_loop(exit);
            }
            Stmt::Break | Stmt::Continue => {
                let is_break = matches!(body.stmts[stmt], Stmt::Break);
                match self.loops.last_mut() {
                    Some(target) => {
                        let at = if is_break {
                            &mut target.breaks
                        } else {
                            &mut target.continues
                        };
                        at.join(&self.state);
                    }
                    None => self.outside_loop(stmt, is_break),
                }
                self.state = self.unreachable();
            }
            Stmt::Return(value) => {
                match value {
                    Some(value) => self.expr(*value),
                    None if self.state.reachable && self.function.returns_value => {
                        self.return_without_value(stmt);
                    }
                    None => {}
                }
                self.state = self.unreachable();
            }
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    /// A loop that nothing has left yet.
    fn fresh_loop(&self) -> Loop {
        Loop {
            breaks: self.unreachable(),
            continues: self.unreachable(),
        }
    }

    /// The paths that `continue` the innermost loop.
    fn continues(&mut self) -> State {
        self.loops
            .last()
            .map_or_else(|| self.unreachable(), |target| target.continues.clone())
    }

    /// Before a loop's first iteration: a local it assigns may be assigned
    /// already when an iteration after the first assigns it.
    fn enter_loop(&mut self, exprs: &[ExprId], stmts: &[StmtId]) {
        let mut assigned = Locals::new(self.body.locals.len(), false);
        let mut collect = Assigned {
            body: self.body,
            out: &mut assigned,
        };
        for &expr in exprs {
            collect.expr(expr);
        }
        for &stmt in stmts {
            collect.stmt(stmt);
        }
        self.state.maybe.union(&assigned);
    }

    /// After a loop: the paths that leave it, by its condition (`exit`,
    /// `None` for a loop that never ends by it) or by `break`.
    fn leave_loop(&mut self, exit: Option<State>) {
        let target = self.loops.pop();
        self.state = exit.unwrap_or_else(|| self.unreachable());
        if let Some(target) = target {
            self.state.join(&target.breaks);
        }
    }

    fn r#match(&mut self, scrutinee: ExprId, arms: &[MatchArm]) {
        self.expr(scrutinee);
        let before = self.state.clone();
        // A `match` whose every arm ends the path ends it; whether its arms
        // cover every value is checked elsewhere (D-138).
        let mut after = if arms.is_empty() {
            before.clone()
        } else {
            self.unreachable()
        };
        for arm in arms {
            self.state = before.clone();
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.expr(guard);
            }
            self.expr(arm.body);
            after.join(&self.state);
        }
        self.state = after;
    }

    /// The expressions of a pattern; its bindings are assigned by it.
    fn pat(&mut self, pat: PatId) {
        let body = self.body;
        match &body.pats[pat] {
            Pat::Missing | Pat::Discard | Pat::Slice(None) => {}
            Pat::Bind(local) => self.depth[local_index(*local)] = self.closure,
            Pat::Constant(expr) | Pat::Relational { value: expr, .. } => self.expr(*expr),
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
                self.expr(*path);
                for &(_, field) in fields {
                    self.pat(field);
                }
            }
            Pat::Positional(fields) => {
                for &(_, field) in fields {
                    self.pat(field);
                }
            }
            Pat::Member {
                fields, binding, ..
            } => {
                for &(_, field) in fields {
                    self.pat(field);
                }
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

    /// A lambda or a local function, checked where it is declared (D-138):
    /// it sees the locals assigned there, may not assign a `let` around it,
    /// and has its own loops and returns.
    fn closure(&mut self, params: &[LocalId], root: ExprId, function: Function) {
        let outer = self.state.clone();
        let loops = std::mem::take(&mut self.loops);
        let around = std::mem::replace(&mut self.function, function);
        self.closure += 1;
        for &param in params {
            self.depth[local_index(param)] = self.closure;
        }
        self.function_body(root);
        self.closure -= 1;
        self.function = around;
        self.loops = loops;
        self.state = outer;
    }

    fn args(&mut self, args: &[Arg]) {
        for arg in args {
            self.expr(arg.expr);
        }
    }

    fn expr(&mut self, expr: ExprId) {
        let body = self.body;
        match &body.exprs[expr] {
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Path(_)
            | Expr::This
            | Expr::Type(_)
            | Expr::Default(_)
            | Expr::Sizeof(_)
            | Expr::Nameof(_)
            | Expr::Macro => {}
            Expr::Local(local) => self.read(expr, *local),
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Hole {
                        value, alignment, ..
                    } = part
                    {
                        self.expr(*value);
                        if let Some(alignment) = alignment {
                            self.expr(*alignment);
                        }
                    }
                }
            }
            Expr::Tuple(elems) => {
                for &(_, elem) in elems {
                    self.expr(elem);
                }
            }
            Expr::Collection(elems) => {
                for &(_, elem) in elems {
                    self.expr(elem);
                }
            }
            Expr::Member { receiver, .. } => self.expr(*receiver),
            Expr::Call { callee, args } => {
                self.expr(*callee);
                self.args(args);
                if self.diverges(*callee) {
                    self.state = self.unreachable();
                }
            }
            Expr::Index { base, args, .. } => {
                self.expr(*base);
                self.args(args);
            }
            Expr::Unary {
                op: UnaryOp::PreIncrement | UnaryOp::PreDecrement,
                expr: place,
            }
            | Expr::Postfix { expr: place, .. } => {
                self.expr(*place);
                self.assign(*place);
            }
            Expr::Unary { expr: operand, .. }
            | Expr::Propagate(operand)
            | Expr::Await(operand)
            | Expr::Cast { expr: operand, .. }
            | Expr::As { expr: operand, .. } => self.expr(*operand),
            Expr::Binary { op, lhs, rhs } => {
                self.expr(*lhs);
                if matches!(
                    op,
                    mora_hir::BinaryOp::And | mora_hir::BinaryOp::Or | mora_hir::BinaryOp::Coalesce
                ) {
                    // The right operand may not run.
                    let before = self.state.clone();
                    self.expr(*rhs);
                    self.state.join(&before);
                } else {
                    self.expr(*rhs);
                }
            }
            Expr::Assign { op, lhs, rhs } => {
                // A compound assignment reads its place first.
                if op.is_some() || !matches!(body.exprs[*lhs], Expr::Local(_)) {
                    self.expr(*lhs);
                }
                self.expr(*rhs);
                self.assign(*lhs);
            }
            Expr::Range { start, end } => {
                for &part in start.iter().chain(end) {
                    self.expr(part);
                }
            }
            Expr::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Expr::With { expr: value, inits } => {
                self.expr(*value);
                for &(_, init) in inits {
                    self.expr(init);
                }
            }
            Expr::Is { expr: value, pat } => {
                self.expr(*value);
                self.pat(*pat);
            }
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => {
                self.expr(*condition);
                let before = self.state.clone();
                self.expr(*then);
                let after_then = std::mem::replace(&mut self.state, before);
                self.expr(*r#else);
                self.state.join(&after_then);
            }
            Expr::Lambda(lambda) => {
                for &(_, default) in &lambda.defaults {
                    self.expr(default);
                }
                let function = Function {
                    name: String::new(),
                    returns_value: false,
                };
                self.closure(&lambda.params, lambda.body, function);
            }
            Expr::New { args, inits, .. } => {
                for arg in args.iter().flatten() {
                    self.expr(arg.expr);
                }
                for &(_, init) in inits.iter().flatten() {
                    self.expr(init);
                }
            }
            Expr::NewArray { size, .. } => self.expr(*size),
            Expr::Block(stmts) => self.block(stmts),
        }
    }

    /// Whether a call of `callee` never returns: a call of `panic`,
    /// `Unreachable` or `Todo` of the standard library (§11.4, D-138).
    fn diverges(&self, callee: ExprId) -> bool {
        let Expr::Path(path) = &self.body.exprs[callee] else {
            return false;
        };
        let [name] = path.segments.as_slice() else {
            return false;
        };
        DIVERGING.contains(&name.as_str())
            && self
                .resolution
                .paths
                .get(&callee)
                .is_some_and(|resolution| {
                    matches!(resolution.target, Target::Item(Resolved::External))
                })
    }

    /// A read of `local` at `expr`: `MR0181` when it may not be assigned.
    fn read(&mut self, expr: ExprId, local: LocalId) {
        if self.state.assigned.contains(local) || self.reported.contains(local) {
            return;
        }
        self.reported.insert(local);
        // Reported once; the rest of the path treats it as assigned.
        self.state.assigned.insert(local);
        let name = &self.body.locals[local].name;
        let in_closure = self.depth[local_index(local)] < self.closure;
        let label = if in_closure {
            format!("the closure reads `{name}`, which is not assigned where it is declared")
        } else {
            format!("`{name}` may not be assigned here")
        };
        self.out.push(
            Diagnostic::error(
                codes::MR0181,
                format!("`{name}` is used before it is assigned"),
                Label::new(self.span(self.map.expr(expr)), label),
            )
            .with_secondary(Label::new(
                self.span(self.map.local(local)),
                "declared here without a value",
            ))
            .with_note(
                "a local declared without a value must be assigned on every path before it is read; \
                 a closure sees the locals assigned where it is declared",
            ),
        );
    }

    /// An assignment to the place `place`: it assigns a local, which may
    /// not be a `let` that is or may be assigned already (`MR0182`).
    fn assign(&mut self, place: ExprId) {
        let Expr::Local(local) = self.body.exprs[place] else {
            return;
        };
        let data = &self.body.locals[local];
        let in_closure = self.depth[local_index(local)] < self.closure;
        let twice = data.kind == LocalKind::Let
            && (in_closure || !self.unassigned.contains(local) || self.state.maybe.contains(local));
        if twice && self.state.reachable {
            self.reassigned(place, local, in_closure);
        }
        if !in_closure {
            self.state.assigned.insert(local);
            self.state.maybe.insert(local);
        }
    }

    /// `MR0182` for the assignment `place` of the `let` local `local`, with
    /// the fix that declares it with `var`.
    fn reassigned(&mut self, place: ExprId, local: LocalId, in_closure: bool) {
        let name = &self.body.locals[local].name;
        let message = if in_closure {
            format!("`{name}` is declared with `let` and cannot be assigned in a closure")
        } else if self.unassigned.contains(local) {
            format!("`{name}` is declared with `let` and may be assigned already")
        } else {
            format!("`{name}` is declared with `let`, so it cannot be assigned again")
        };
        let declared = self.map.local(local);
        let mut diagnostic = Diagnostic::error(
            codes::MR0182,
            message,
            Label::new(self.span(self.map.expr(place)), "assigned again here"),
        )
        .with_secondary(Label::new(self.span(declared), "declared with `let` here"))
        .with_note(
            "a `let` local is assigned exactly once, and a closure may run more than once; \
             declare a local that changes with `var`",
        );
        // The `let` before the name, when it is written there.
        let text = self.file.text(self.db);
        let before = text
            .get(..usize::from(declared.start()))
            .unwrap_or_default();
        if let Some(start) = before.trim_end().strip_suffix("let").map(str::len) {
            let at = TextRange::at(TextSize::from(start as u32), TextSize::of("let"));
            diagnostic = diagnostic.with_fix(Fix::new(
                "declare it with `var`",
                vec![TextEdit::replace(self.span(at), "var")],
            ));
        }
        self.out.push(diagnostic);
    }

    /// `MR0183` for `break` or `continue` outside a loop.
    fn outside_loop(&mut self, stmt: StmtId, is_break: bool) {
        // Lowering records the range of every jump.
        let range = self.map.jumps.get(&stmt).copied().unwrap_or_default();
        let word = if is_break { "break" } else { "continue" };
        let note = if self.closure > 0 {
            "a lambda or a local function does not see the loops around it, so its `break` and `continue` have no loop to leave"
        } else {
            "`break` and `continue` act on the innermost `while`, `do`, `for` or `foreach` around them"
        };
        self.out.push(
            Diagnostic::error(
                codes::MR0183,
                format!("`{word}` outside a loop"),
                Label::new(self.span(range), "no loop to leave"),
            )
            .with_note(note),
        );
    }

    /// `MR0184` for `return;` in a function that returns a value.
    fn return_without_value(&mut self, stmt: StmtId) {
        // Lowering records the range of every jump.
        let range = self.map.jumps.get(&stmt).copied().unwrap_or_default();
        let name = &self.function.name;
        self.out.push(
            Diagnostic::error(
                codes::MR0184,
                format!("`return` without a value in `{name}`"),
                Label::new(self.span(range), "returns no value"),
            )
            .with_note(note_mr0184(name)),
        );
    }
}

/// The note of `MR0184` for the function `name`.
fn note_mr0184(name: &str) -> String {
    format!(
        "`{name}` returns a value, so every path through it ends in `return` with a value, \
         a call of `panic`, `Unreachable` or `Todo`, or a loop that never ends"
    )
}

/// Whether `condition` is the literal `true`, which makes a loop end only
/// by `break` (D-138).
fn is_true(body: &Body, condition: ExprId) -> bool {
    matches!(body.exprs[condition], Expr::Literal(Literal::Bool(true)))
}

/// The locals that a part of a body assigns, outside the closures in it.
struct Assigned<'a> {
    /// The body.
    body: &'a Body,
    /// The locals found so far.
    out: &'a mut Locals,
}

impl Assigned<'_> {
    fn stmt(&mut self, stmt: StmtId) {
        let body = self.body;
        match &body.stmts[stmt] {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
            Stmt::Function(_) => {}
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) | Stmt::Deconstruct { init: expr, .. } => {
                self.expr(*expr);
            }
            Stmt::Local { init, .. } => {
                if let Some(init) = init {
                    self.expr(*init);
                }
            }
            Stmt::Block(stmts) => {
                for &stmt in stmts {
                    self.stmt(stmt);
                }
            }
            Stmt::If {
                condition,
                then,
                r#else,
            } => {
                self.expr(*condition);
                self.stmt(*then);
                if let Some(r#else) = r#else {
                    self.stmt(*r#else);
                }
            }
            Stmt::While { condition, body } | Stmt::DoWhile { body, condition } => {
                self.expr(*condition);
                self.stmt(*body);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                for &stmt in init {
                    self.stmt(stmt);
                }
                for &expr in condition.iter().chain(update) {
                    self.expr(expr);
                }
                self.stmt(*body);
            }
            Stmt::Foreach { iterable, body, .. } => {
                self.expr(*iterable);
                self.stmt(*body);
            }
            Stmt::Match { scrutinee, arms } => {
                self.expr(*scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.expr(arm.body);
                }
            }
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    fn expr(&mut self, expr: ExprId) {
        let body = self.body;
        match &body.exprs[expr] {
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Local(_)
            | Expr::Path(_)
            | Expr::This
            | Expr::Type(_)
            | Expr::Default(_)
            | Expr::Sizeof(_)
            | Expr::Nameof(_)
            | Expr::Macro
            // A closure's assignments are checked as its own.
            | Expr::Lambda(_) => {}
            Expr::Assign { lhs, rhs, .. } => {
                self.place(*lhs);
                self.expr(*rhs);
            }
            Expr::Unary {
                op: UnaryOp::PreIncrement | UnaryOp::PreDecrement,
                expr: place,
            }
            | Expr::Postfix { expr: place, .. } => self.place(*place),
            Expr::Unary { expr: inner, .. }
            | Expr::Propagate(inner)
            | Expr::Await(inner)
            | Expr::Cast { expr: inner, .. }
            | Expr::As { expr: inner, .. }
            | Expr::Member { receiver: inner, .. }
            | Expr::NewArray { size: inner, .. }
            | Expr::Is { expr: inner, .. } => self.expr(*inner),
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Hole {
                        value, alignment, ..
                    } = part
                    {
                        self.expr(*value);
                        if let Some(alignment) = alignment {
                            self.expr(*alignment);
                        }
                    }
                }
            }
            Expr::Tuple(elems) => {
                for &(_, elem) in elems {
                    self.expr(elem);
                }
            }
            Expr::Collection(elems) => {
                for &(_, elem) in elems {
                    self.expr(elem);
                }
            }
            Expr::Call { callee: base, args } | Expr::Index { base, args, .. } => {
                self.expr(*base);
                for arg in args {
                    self.expr(arg.expr);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(*lhs);
                self.expr(*rhs);
            }
            Expr::Range { start, end } => {
                for &part in start.iter().chain(end) {
                    self.expr(part);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.expr(*scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.expr(arm.body);
                }
            }
            Expr::With { expr: value, inits } => {
                self.expr(*value);
                for &(_, init) in inits {
                    self.expr(init);
                }
            }
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => {
                self.expr(*condition);
                self.expr(*then);
                self.expr(*r#else);
            }
            Expr::New { args, inits, .. } => {
                for arg in args.iter().flatten() {
                    self.expr(arg.expr);
                }
                for &(_, init) in inits.iter().flatten() {
                    self.expr(init);
                }
            }
            Expr::Block(stmts) => {
                for &stmt in stmts {
                    self.stmt(stmt);
                }
            }
        }
    }

    /// An assigned place: a local is assigned, and any other place is read.
    fn place(&mut self, place: ExprId) {
        match self.body.exprs[place] {
            Expr::Local(local) => self.out.insert(local),
            _ => self.expr(place),
        }
    }
}
