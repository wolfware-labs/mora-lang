//! The control flow of one body: which code is reachable, which locals are
//! assigned where, and where loops are (§7.10, §7.11).
//!
//! The walk follows the order of evaluation and keeps one [`State`] for the
//! path it is on. Where paths meet, after `if`, `&&`, `match` or a loop, the
//! states join: a local is definitely assigned when it is on every path that
//! reaches the point, and may be assigned when it is on one.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_hir::{
    Arg, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, BodySourceMap, Expr, ExprId,
    InterpPart, ItemId, Literal, LocalId, LocalKind, MatchArm, Pat, PatId, Resolved, STD,
    Signature, Stmt, StmtId, Target, Ty, UnaryOp, arena_index, body_resolution, item_tree,
    signature,
};
use mora_span::{Db, Project, SourceFile, Span, TextRange};

/// The names of the standard library's functions of the never type (§11.4),
/// which a path ends at. Until the standard library is loaded they are
/// recognised by name (D-138).
const DIVERGING: [&str; 3] = ["panic", "Unreachable", "Todo"];

/// A set of locals, by their index.
#[derive(Clone, PartialEq, Eq)]
struct Locals(Vec<u64>);

impl Locals {
    /// No local of a body with `len` locals.
    fn empty(len: usize) -> Self {
        Locals(vec![0; len.div_ceil(64)])
    }

    /// Every local of a body with `len` locals.
    fn full(len: usize) -> Self {
        Locals(vec![u64::MAX; len.div_ceil(64)])
    }

    fn contains(&self, local: LocalId) -> bool {
        let index = arena_index(local);
        self.0[index / 64] & (1 << (index % 64)) != 0
    }

    fn insert(&mut self, local: LocalId) {
        let index = arena_index(local);
        self.0[index / 64] |= 1 << (index % 64);
    }

    fn remove(&mut self, local: LocalId) {
        let index = arena_index(local);
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
            assigned: Locals::full(len),
            maybe: Locals::empty(len),
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

/// A statement that leaves the innermost loop.
#[derive(Clone, Copy)]
enum Jump {
    Break,
    Continue,
}

impl Jump {
    /// The keyword that writes it.
    fn keyword(self) -> &'static str {
        match self {
            Jump::Break => "break",
            Jump::Continue => "continue",
        }
    }
}

/// A loop that `break` and `continue` leave.
struct Loop {
    /// Where the paths that `break` out of it meet.
    breaks: State,
    /// Where the paths that `continue` it meet.
    continues: State,
}

/// The function a part of the body returns from. The default is one that
/// returns no value, such as a lambda.
#[derive(Clone, Default)]
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
    let mut unassigned = Locals::empty(len);
    let mut assigned = Locals::full(len);
    for (_, stmt) in body.stmts.iter() {
        if let Stmt::Local { local, init: None } = stmt {
            unassigned.insert(*local);
            assigned.remove(*local);
        }
    }
    let resolution = body_resolution(db, project, file, owner);
    let mut flow = Flow {
        db,
        file,
        body,
        map,
        resolution,
        state: State {
            reachable: true,
            maybe: Locals::empty(len),
            assigned,
        },
        unassigned,
        depth: vec![0; len],
        closure: 0,
        loops: Vec::new(),
        dry: false,
        function: function(db, project, file, owner, body, resolution),
        reported: Locals::empty(len),
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
/// signature gives its return type, or a function that implements an
/// interface member or extends a type, whose body holds it. Other bodies,
/// whose return types come with their kinds of member, are not checked yet
/// (D-138).
fn function(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
    body: &Body,
    resolution: &BodyResolution,
) -> Function {
    match owner {
        BodyOwner::Item(item) => {
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
        BodyOwner::Member(..) | BodyOwner::Extension(..) if body.kind == BodyKind::Function => {
            Function {
                name: body.name.clone(),
                returns_value: body
                    .ret
                    .is_some_and(|ret| returns_value(&resolution.written(ret))),
            }
        }
        _ => Function::default(),
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
    /// The file of the body, which the spans name.
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
    /// Whether the walk is a first pass over a loop, which only finds what
    /// its iterations may assign and reports nothing.
    dry: bool,
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
        // A block the parser recovered from has no closing brace to point at.
        let Some(&brace) = self.map.ends.get(&root) else {
            return;
        };
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
                self.depth[arena_index(*local)] = self.closure;
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
                let inner = Function {
                    name: body.locals[function.local].name.clone(),
                    returns_value: returns_value(&self.resolution.return_type(function)),
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
            } => self.branches(
                *condition,
                |flow| flow.stmt(*then),
                |flow| {
                    if let Some(r#else) = r#else {
                        flow.stmt(*r#else);
                    }
                },
            ),
            Stmt::While { condition, body } => self.r#loop(|flow| {
                let exit = flow.head(Some(*condition));
                flow.stmt(*body);
                let back = flow.back();
                (exit, back)
            }),
            Stmt::DoWhile { body, condition } => self.r#loop(|flow| {
                flow.stmt(*body);
                let end = flow.back();
                flow.state = end;
                let (when_true, when_false) = flow.condition(*condition);
                let body = flow.body;
                let exit = (!is_bool(body, *condition, true)).then_some(when_false);
                // `do { } while (false);` runs its body once.
                let back = if is_bool(body, *condition, false) {
                    flow.unreachable()
                } else {
                    when_true
                };
                (exit, back)
            }),
            Stmt::For {
                init,
                condition,
                update,
                body: loop_body,
            } => {
                self.block(init);
                self.r#loop(|flow| {
                    let exit = flow.head(*condition);
                    flow.stmt(*loop_body);
                    flow.state = flow.back();
                    for &expr in update {
                        flow.expr(expr);
                    }
                    (exit, flow.state.clone())
                });
            }
            Stmt::Foreach {
                pat,
                iterable,
                body: loop_body,
                ..
            } => {
                self.expr(*iterable);
                self.r#loop(|flow| {
                    let exit = Some(flow.state.clone());
                    flow.pat(*pat);
                    flow.stmt(*loop_body);
                    let back = flow.back();
                    (exit, back)
                });
            }
            Stmt::Break => self.jump(stmt, Jump::Break),
            Stmt::Continue => self.jump(stmt, Jump::Continue),
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

    /// `break` or `continue` at `stmt`: the path goes to the innermost loop.
    fn jump(&mut self, stmt: StmtId, jump: Jump) {
        match self.loops.last_mut() {
            Some(target) => {
                let at = match jump {
                    Jump::Break => &mut target.breaks,
                    Jump::Continue => &mut target.continues,
                };
                at.join(&self.state);
            }
            None => self.outside_loop(stmt, jump),
        }
        self.state = self.unreachable();
    }

    /// A loop, whose iteration `iterate` walks from the loop's start: it
    /// returns the state that leaves the loop by its condition, `None` for a
    /// loop that never ends by it, and the state that goes back to the start.
    ///
    /// A first pass finds what an iteration may assign before it goes back,
    /// so that the second, which reports, knows what may be assigned already
    /// at the start. A loop inside a first pass is walked once: what its
    /// iterations assign is on its way out as well.
    fn r#loop(&mut self, iterate: impl Fn(&mut Self) -> (Option<State>, State)) {
        if self.dry {
            let (exit, back) = self.iteration(&iterate);
            self.state = exit;
            self.state.maybe.union(&back.maybe);
            return;
        }
        let entry = self.state.clone();
        let reported = self.reported.clone();
        let len = self.out.len();
        self.dry = true;
        let back = self.iteration(&iterate).1;
        self.dry = false;
        self.out.truncate(len);
        self.reported = reported;
        self.state = entry;
        self.state.maybe.union(&back.maybe);
        self.state = self.iteration(&iterate).0;
    }

    /// One walk of a loop's iteration: the state after the loop, and the
    /// state that goes back to its start.
    fn iteration(
        &mut self,
        iterate: &impl Fn(&mut Self) -> (Option<State>, State),
    ) -> (State, State) {
        self.loops.push(Loop {
            breaks: self.unreachable(),
            continues: self.unreachable(),
        });
        let (exit, back) = iterate(self);
        let target = self.loops.pop();
        let mut after = exit.unwrap_or_else(|| self.unreachable());
        if let Some(target) = target {
            after.join(&target.breaks);
        }
        (after, back)
    }

    /// The start of an iteration of a loop with the condition `condition`:
    /// the walk goes on where it holds, and the state where it does not,
    /// which leaves the loop, is returned. `None` for a loop that never ends
    /// by its condition: one with none or with the literal `true` (D-138).
    fn head(&mut self, condition: Option<ExprId>) -> Option<State> {
        let condition = condition?;
        let (when_true, when_false) = self.condition(condition);
        self.state = when_true;
        (!is_bool(self.body, condition, true)).then_some(when_false)
    }

    /// Where the end of an iteration and the paths that `continue` it meet.
    fn back(&self) -> State {
        let mut back = self.state.clone();
        if let Some(target) = self.loops.last() {
            back.join(&target.continues);
        }
        back
    }

    /// The condition `condition`: the states where it is `true` and where it
    /// is `false`. The operands of `&&`, `||` and `!` lead to one or the
    /// other.
    fn condition(&mut self, condition: ExprId) -> (State, State) {
        let body = self.body;
        match &body.exprs[condition] {
            Expr::Binary {
                op: BinaryOp::And,
                lhs,
                rhs,
            } => {
                let (when_true, mut when_false) = self.condition(*lhs);
                self.state = when_true;
                let (when_true, rhs_false) = self.condition(*rhs);
                when_false.join(&rhs_false);
                (when_true, when_false)
            }
            Expr::Binary {
                op: BinaryOp::Or,
                lhs,
                rhs,
            } => {
                let (mut when_true, when_false) = self.condition(*lhs);
                self.state = when_false;
                let (rhs_true, when_false) = self.condition(*rhs);
                when_true.join(&rhs_true);
                (when_true, when_false)
            }
            Expr::Unary {
                op: UnaryOp::Not,
                expr,
            } => {
                let (when_true, when_false) = self.condition(*expr);
                (when_false, when_true)
            }
            _ => {
                self.expr(condition);
                (self.state.clone(), self.state.clone())
            }
        }
    }

    /// `then` where `condition` holds and `r#else` where it does not, of an
    /// `if` or `c ? a : b`; the paths meet after them.
    fn branches(
        &mut self,
        condition: ExprId,
        then: impl FnOnce(&mut Self),
        r#else: impl FnOnce(&mut Self),
    ) {
        let (when_true, when_false) = self.condition(condition);
        self.state = when_true;
        then(self);
        let after_then = std::mem::replace(&mut self.state, when_false);
        r#else(self);
        self.state.join(&after_then);
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
        // An arm is reached as well where an arm before it matched but its
        // guard did not hold.
        let mut entry = before;
        for arm in arms {
            self.state = entry.clone();
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                let (when_true, when_false) = self.condition(guard);
                entry.join(&when_false);
                self.state = when_true;
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
            Pat::Bind(local) => self.depth[arena_index(*local)] = self.closure,
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
            self.depth[arena_index(param)] = self.closure;
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
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => {
                let (when_true, when_false) = self.condition(expr);
                self.state = when_true;
                self.state.join(&when_false);
            }
            Expr::Binary { op, lhs, rhs } => {
                self.expr(*lhs);
                if *op == BinaryOp::Coalesce {
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
            } => self.branches(
                *condition,
                |flow| flow.expr(*then),
                |flow| flow.expr(*r#else),
            ),
            Expr::Lambda(lambda) => {
                for &(_, default) in &lambda.defaults {
                    self.expr(default);
                }
                self.closure(&lambda.params, lambda.body, Function::default());
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
    /// `Unreachable` or `Todo` of the standard library, by its name or as
    /// `Std.panic` (§11.4, D-138).
    fn diverges(&self, callee: ExprId) -> bool {
        let body = self.body;
        // The path that has to name the standard library: the function's own
        // name, or `Std` before it.
        let (name, path) = match &body.exprs[callee] {
            Expr::Path(path) => match path.segments.as_slice() {
                [name] => (name, callee),
                _ => return false,
            },
            Expr::Member { receiver, name, .. } => match &body.exprs[*receiver] {
                Expr::Path(path) if path.segments == [STD] => (name, *receiver),
                _ => return false,
            },
            _ => return false,
        };
        DIVERGING.contains(&name.as_str())
            && self.resolution.paths.get(&path).is_some_and(|resolution| {
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
        let in_closure = self.depth[arena_index(local)] < self.closure;
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
        let in_closure = self.depth[arena_index(local)] < self.closure;
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
        if let Some(&at) = self.map.lets.get(&local) {
            diagnostic = diagnostic.with_fix(Fix::new(
                "declare it with `var`",
                vec![TextEdit::replace(self.span(at), "var")],
            ));
        }
        self.out.push(diagnostic);
    }

    /// Where the `break`, `continue` or `return` statement `stmt` is.
    fn jump_span(&self, stmt: StmtId) -> Span {
        // Lowering records the range of every jump.
        self.span(self.map.jumps.get(&stmt).copied().unwrap_or_default())
    }

    /// `MR0183` for `break` or `continue` outside a loop.
    fn outside_loop(&mut self, stmt: StmtId, jump: Jump) {
        let word = jump.keyword();
        let note = if self.closure > 0 {
            "a lambda or a local function does not see the loops around it, so its `break` and `continue` have no loop to leave"
        } else {
            "`break` and `continue` act on the innermost `while`, `do`, `for` or `foreach` around them"
        };
        self.out.push(
            Diagnostic::error(
                codes::MR0183,
                format!("`{word}` outside a loop"),
                Label::new(self.jump_span(stmt), "no loop to leave"),
            )
            .with_note(note),
        );
    }

    /// `MR0184` for `return;` in a function that returns a value.
    fn return_without_value(&mut self, stmt: StmtId) {
        let name = &self.function.name;
        self.out.push(
            Diagnostic::error(
                codes::MR0184,
                format!("`return` without a value in `{name}`"),
                Label::new(self.jump_span(stmt), "returns no value"),
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

/// Whether `condition` is the literal `value`: a loop whose condition is
/// `true` ends only by `break`, and one whose condition is `false` after its
/// body does not go back (D-138).
fn is_bool(body: &Body, condition: ExprId, value: bool) -> bool {
    matches!(body.exprs[condition], Expr::Literal(Literal::Bool(literal)) if literal == value)
}
