//! Local functions and what they take from the bodies around them (§7.10).
//!
//! A local function is not a value and cannot outlive the body it is
//! declared in, so what it uses of the bodies around it can be passed to
//! it: each local it reads, and `this` in a method, with an `in` borrow,
//! each it assigns with a `ref` borrow. A local function also takes what
//! the local functions it calls take, unless it declares it itself. A
//! local is inside a local function when the function's code declares it:
//! its parameters, its locals and those of the code nested in it.

use std::collections::{BTreeMap, HashSet};

use mora_span::{Db, Project, SourceFile};

use crate::arena::arena_index;
use crate::bodies::{
    ArgMode, Body, BodyOwner, BodyResolution, CaptureTarget, Expr, ExprId, InterpPart, LocalId,
    LocalKind, Pat, PatId, Stmt, StmtId, Target, UnaryOp, body, body_resolution,
};
use crate::defs::ItemId;
use crate::inference::{Callee, Inference, infer};
use crate::items::item_tree;
use crate::signature::signature;
use crate::ty::{ParamMode, Signature};

/// A local function of a body and what it takes from the bodies around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFunctionCaptures {
    /// The statement that declares it.
    pub stmt: StmtId,
    /// The local its name declares.
    pub local: LocalId,
    /// The innermost local function it is declared in, by its position in
    /// the list [`local_functions`] gives.
    pub outer: Option<usize>,
    /// What it takes, each with the mode it takes it by, `in` or `ref`:
    /// `this` first, then the locals in the order of their declarations.
    pub captures: Vec<(Captured, ParamMode)>,
}

/// What a local function takes from the bodies around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Captured {
    /// The receiver of the method it is in.
    This,
    /// A local of a body around it.
    Local(LocalId),
}

/// The local functions of the body of `owner`, in the order of the text,
/// with what each takes from the bodies around it. `this` is taken `ref`
/// in a `mut` method, or where the function assigns through it.
#[salsa::tracked(returns(ref))]
pub fn local_functions(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Vec<LocalFunctionCaptures> {
    let hir = body(db, file, owner);
    let receiver = match owner {
        BodyOwner::Item(item) => match signature(
            db,
            ItemId {
                project,
                file,
                item,
            },
        ) {
            Signature::Function(function) => function.receiver,
            _ => None,
        },
        _ => None,
    };
    let mut walk = Walk {
        db,
        hir,
        resolution: body_resolution(db, project, file, owner),
        inference: infer(db, project, file, owner),
        this_by_ref: receiver == Some(ParamMode::Ref),
        open: Vec::new(),
        functions: Vec::new(),
    };
    if let Some(root) = hir.root {
        walk.expr(root);
    }
    walk.finish()
}

/// A local function while the body is walked.
struct Found {
    stmt: StmtId,
    local: LocalId,
    outer: Option<usize>,
    /// The locals declared inside it.
    declared: HashSet<LocalId>,
    /// What it takes itself, keyed by `this` first, then the position of
    /// the local.
    captures: BTreeMap<(u8, usize), (Captured, ParamMode)>,
    /// The local functions it calls that are not inside it.
    calls: Vec<LocalId>,
}

/// The walk over a body, in the order of the text.
struct Walk<'a> {
    db: &'a dyn Db,
    hir: &'a Body,
    resolution: &'a BodyResolution,
    inference: &'a Inference,
    /// Whether the body's receiver is a `ref` borrow.
    this_by_ref: bool,
    /// The local functions the code being walked is in, outermost first,
    /// by their position in `functions`.
    open: Vec<usize>,
    functions: Vec<Found>,
}

impl Walk<'_> {
    /// The local functions, each with what the ones it calls take too. A
    /// local function is declared before the code that calls it, so it
    /// comes first in the text, and one pass in that order is enough.
    fn finish(mut self) -> Vec<LocalFunctionCaptures> {
        for caller in 0..self.functions.len() {
            for callee in self.functions[caller].calls.clone() {
                let Some(callee) = self.functions.iter().position(|f| f.local == callee) else {
                    continue;
                };
                let inherited: Vec<_> = self.functions[callee].captures.values().copied().collect();
                let caller = &mut self.functions[caller];
                for (captured, mode) in inherited {
                    if let Captured::Local(local) = captured
                        && caller.declared.contains(&local)
                    {
                        continue;
                    }
                    capture(&mut caller.captures, captured, mode);
                }
            }
        }
        self.functions
            .into_iter()
            .map(|found| LocalFunctionCaptures {
                stmt: found.stmt,
                local: found.local,
                outer: found.outer,
                captures: found.captures.into_values().collect(),
            })
            .collect()
    }

    /// Records that `local` is declared in the code being walked.
    fn declare(&mut self, local: LocalId) {
        for &open in &self.open {
            self.functions[open].declared.insert(local);
        }
    }

    /// Records a use of `local`, by `mode`, in the code being walked.
    fn local(&mut self, local: LocalId, mode: ParamMode) {
        let function = self.hir.locals[local].kind == LocalKind::Function;
        for &open in &self.open {
            let found = &mut self.functions[open];
            if found.declared.contains(&local) {
                continue;
            }
            if function {
                found.calls.push(local);
            } else {
                capture(&mut found.captures, Captured::Local(local), mode);
            }
        }
    }

    /// Records a use of `this`, which assigns through it when `assigned`.
    fn this(&mut self, assigned: bool) {
        let mode = if assigned || self.this_by_ref {
            ParamMode::Ref
        } else {
            ParamMode::In
        };
        for &open in &self.open {
            capture(&mut self.functions[open].captures, Captured::This, mode);
        }
    }

    /// Records an assignment to the place `place`, or a `ref` borrow of it.
    fn assigned(&mut self, place: ExprId) {
        match &self.hir.exprs[place] {
            Expr::Local(local) => self.local(*local, ParamMode::Ref),
            Expr::This => self.this(true),
            Expr::Path(_) if self.names_field(place) => self.this(true),
            Expr::Member { receiver, .. } => self.assigned(*receiver),
            _ => {}
        }
    }

    /// Whether the path `path` names a field of `this`.
    fn names_field(&self, path: ExprId) -> bool {
        self.resolution
            .paths
            .get(&path)
            .is_some_and(|resolution| match resolution.target {
                Target::Value(value) => item_tree(self.db, value.ty.file)
                    .item(value.ty.item)
                    .values
                    .get(value.index)
                    .is_some_and(|member| member.is_instance_field()),
                _ => false,
            })
    }

    fn stmts(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        let hir = self.hir;
        match &hir.stmts[stmt] {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Return(None) => {}
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => self.expr(*expr),
            Stmt::Local { local, init } => {
                self.declare(*local);
                if let Some(init) = init {
                    self.expr(*init);
                }
            }
            Stmt::Deconstruct { pat, init } => {
                self.expr(*init);
                self.pat(*pat);
            }
            Stmt::Function(function) => {
                self.declare(function.local);
                let index = self.functions.len();
                self.functions.push(Found {
                    stmt,
                    local: function.local,
                    outer: self.open.last().copied(),
                    declared: HashSet::new(),
                    captures: BTreeMap::new(),
                    calls: Vec::new(),
                });
                self.open.push(index);
                for &param in &function.params {
                    self.declare(param);
                }
                for &(_, default) in &function.defaults {
                    self.expr(default);
                }
                if let Some(body) = function.body {
                    self.expr(body);
                }
                self.open.pop();
            }
            Stmt::Block(stmts) => self.stmts(stmts),
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
                self.stmts(init);
                if let Some(condition) = condition {
                    self.expr(*condition);
                }
                for &update in update {
                    self.expr(update);
                }
                self.stmt(*body);
            }
            Stmt::Foreach {
                pat,
                iterable,
                body,
                ..
            } => {
                self.expr(*iterable);
                self.pat(*pat);
                self.stmt(*body);
            }
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Stmt::Unsafe(stmt) => self.stmt(*stmt),
        }
    }

    fn r#match(&mut self, scrutinee: ExprId, arms: &[crate::bodies::MatchArm]) {
        self.expr(scrutinee);
        for arm in arms {
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.expr(guard);
            }
            self.expr(arm.body);
        }
    }

    fn pat(&mut self, pat: PatId) {
        let hir = self.hir;
        match &hir.pats[pat] {
            Pat::Missing | Pat::Discard | Pat::Slice(None) => {}
            Pat::Constant(expr) | Pat::Relational { value: expr, .. } => self.expr(*expr),
            Pat::Bind(local) => self.declare(*local),
            Pat::Type { binding, .. } => {
                if let Some(binding) = binding {
                    self.pat(*binding);
                }
            }
            Pat::Not(inner) | Pat::Slice(Some(inner)) => self.pat(*inner),
            Pat::And(a, b) | Pat::Or(a, b) => {
                self.pat(*a);
                self.pat(*b);
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
            Pat::List(pats) => {
                for &pat in pats {
                    self.pat(pat);
                }
            }
        }
    }

    fn exprs(&mut self, exprs: impl IntoIterator<Item = ExprId>) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: ExprId) {
        let hir = self.hir;
        match &hir.exprs[expr] {
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Type(_)
            | Expr::Discard
            | Expr::Default(_)
            | Expr::Sizeof(_)
            | Expr::Macro => {}
            Expr::Local(local) => self.local(*local, ParamMode::In),
            Expr::This => self.this(false),
            Expr::Path(_) => {
                if self.names_field(expr) {
                    self.this(false);
                }
            }
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Hole {
                        value, alignment, ..
                    } = part
                    {
                        self.expr(*value);
                        self.exprs(alignment.iter().copied());
                    }
                }
            }
            Expr::Tuple(elems) => self.exprs(elems.iter().map(|&(_, elem)| elem)),
            Expr::Collection(elems) => self.exprs(elems.iter().map(|&(_, elem)| elem)),
            Expr::Member { receiver, .. } => self.expr(*receiver),
            Expr::Call { callee, args } => {
                self.expr(*callee);
                self.exprs(args.iter().map(|arg| arg.expr));
                for arg in args.iter().filter(|arg| arg.mode == Some(ArgMode::Ref)) {
                    self.assigned(arg.expr);
                }
                self.receiver(expr, *callee);
            }
            Expr::Index { base, args, .. } => {
                self.expr(*base);
                self.exprs(args.iter().map(|arg| arg.expr));
            }
            Expr::Unary { op, expr: operand } => {
                self.expr(*operand);
                if matches!(op, UnaryOp::PreIncrement | UnaryOp::PreDecrement) {
                    self.assigned(*operand);
                }
            }
            Expr::Postfix { expr: operand, .. } => {
                self.expr(*operand);
                self.assigned(*operand);
            }
            Expr::Binary { lhs, rhs, .. } => self.exprs([*lhs, *rhs]),
            Expr::Assign { lhs, rhs, .. } => {
                self.exprs([*lhs, *rhs]);
                self.assigned(*lhs);
            }
            Expr::Propagate(inner)
            | Expr::Cast { expr: inner, .. }
            | Expr::Await(inner)
            | Expr::As { expr: inner, .. }
            | Expr::Nameof(inner)
            | Expr::NewArray { size: inner, .. } => self.expr(*inner),
            Expr::Range { start, end } => self.exprs(start.iter().chain(end).copied()),
            Expr::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Expr::With { expr: value, inits } => {
                self.expr(*value);
                self.exprs(inits.iter().map(|&(_, init)| init));
            }
            Expr::Is { expr: value, pat } => {
                self.expr(*value);
                self.pat(*pat);
            }
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => self.exprs([*condition, *then, *r#else]),
            Expr::Lambda(lambda) => {
                for capture in &lambda.captures {
                    match capture.target {
                        CaptureTarget::This => self.this(false),
                        CaptureTarget::Local(local) => self.local(local, ParamMode::In),
                        CaptureTarget::Unresolved(_) => {}
                    }
                }
                for &param in &lambda.params {
                    self.declare(param);
                }
                self.exprs(lambda.defaults.iter().map(|&(_, default)| default));
                self.expr(lambda.body);
            }
            Expr::New { args, inits, .. } => {
                self.exprs(args.iter().flatten().map(|arg| arg.expr));
                self.exprs(inits.iter().flatten().map(|&(_, init)| init));
            }
            Expr::Block(stmts) => self.stmts(stmts),
        }
    }

    /// Records what the call `call` through `callee` does to its receiver:
    /// a method named alone uses `this`, and a `mut` method assigns
    /// through its receiver.
    fn receiver(&mut self, call: ExprId, callee: ExprId) {
        let Some(Callee::Item(item)) = self.inference.callee(call) else {
            return;
        };
        let Signature::Function(function) = signature(self.db, item) else {
            return;
        };
        let Some(mode) = function.receiver else {
            return;
        };
        match &self.hir.exprs[callee] {
            Expr::Member { receiver, .. } if mode == ParamMode::Ref => self.assigned(*receiver),
            Expr::Path(_) => self.this(mode == ParamMode::Ref),
            _ => {}
        }
    }
}

/// Records that a local function takes `captured` by `mode`: `ref` when
/// any use needs it.
fn capture(
    captures: &mut BTreeMap<(u8, usize), (Captured, ParamMode)>,
    captured: Captured,
    mode: ParamMode,
) {
    let key = match captured {
        Captured::This => (0, 0),
        Captured::Local(local) => (1, arena_index(local)),
    };
    let (_, recorded) = captures.entry(key).or_insert((captured, mode));
    if mode == ParamMode::Ref {
        *recorded = ParamMode::Ref;
    }
}

#[cfg(test)]
mod tests {
    use crate::test_db::{TestDb, app, codes, file_named};
    use crate::*;

    /// Each local function of the owner `owner` of `text`, by name, with
    /// its enclosing local function and its captures as `name:mode`.
    fn summary(text: &str, owner: &str) -> Vec<(String, Option<usize>, Vec<String>)> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(
            codes(diagnostics(&db, project)),
            Vec::<String>::new(),
            "{text}"
        );
        let tree = item_tree(&db, file);
        let (item, _) = tree
            .items()
            .find(|(_, item)| item.name == owner)
            .expect("the owner");
        let owner = BodyOwner::Item(item);
        let body = body(&db, file, owner);
        local_functions(&db, project, file, owner)
            .iter()
            .map(|function| {
                let captures = function
                    .captures
                    .iter()
                    .map(|(captured, mode)| {
                        let name = match captured {
                            Captured::This => "this",
                            Captured::Local(local) => body.locals[*local].name.as_str(),
                        };
                        let mode = match mode {
                            ParamMode::Ref => "ref",
                            _ => "in",
                        };
                        format!("{name}:{mode}")
                    })
                    .collect();
                (
                    body.locals[function.local].name.clone(),
                    function.outer,
                    captures,
                )
            })
            .collect()
    }

    fn entry(
        name: &str,
        outer: Option<usize>,
        captures: &[&str],
    ) -> (String, Option<usize>, Vec<String>) {
        (
            name.to_owned(),
            outer,
            captures.iter().map(|&capture| capture.to_owned()).collect(),
        )
    }

    #[test]
    fn a_local_function_captures_what_it_reads_and_assigns() {
        let text = "F(n: int) {\n    var total = 0;\n    A() { total += n; }\n    B() { A(); }\n    \
                    E() {\n        let k = 1;\n        G(): int => k + n;\n        let j = G();\n    }\n    \
                    H() {\n        var count = 0;\n        count++;\n        H();\n    }\n}\n";
        assert_eq!(
            summary(text, "F"),
            [
                entry("A", None, &["n:in", "total:ref"]),
                entry("B", None, &["n:in", "total:ref"]),
                entry("E", None, &["n:in"]),
                entry("G", Some(2), &["n:in", "k:in"]),
                entry("H", None, &[]),
            ]
        );
    }

    #[test]
    fn a_local_function_of_a_method_captures_this() {
        let text = "struct P {\n    public mut X: int;\n    \
                    Get(): int {\n        G(): int => X;\n        return G();\n    }\n    \
                    mut Set() {\n        H() { X = 2; }\n        H();\n    }\n    \
                    mut Read(): int {\n        K(): int => this.X;\n        return K();\n    }\n    \
                    Twice(): int {\n        L(): int => Get();\n        M(): int => L();\n        \
                    return M();\n    }\n}\n";
        assert_eq!(summary(text, "Get"), [entry("G", None, &["this:in"])]);
        // An assignment through `this` in a method that is not `mut` takes
        // it by `ref`; a static is not reached through `this`.
        let assigns = "struct P {\n    public mut X: int;\n    public static mut S: int = 0;\n    \
                       A() {\n        G() { this.X = 1; }\n    }\n    \
                       B() {\n        H() { X = 1; }\n    }\n    \
                       C() {\n        K() { S = 1; }\n    }\n}\n";
        assert_eq!(summary(assigns, "A"), [entry("G", None, &["this:ref"])]);
        assert_eq!(summary(assigns, "B"), [entry("H", None, &["this:ref"])]);
        assert_eq!(summary(assigns, "C"), [entry("K", None, &[])]);
        assert_eq!(summary(text, "Set"), [entry("H", None, &["this:ref"])]);
        // The receiver of a `mut` method is lifted as `ref`.
        assert_eq!(summary(text, "Read"), [entry("K", None, &["this:ref"])]);
        assert_eq!(
            summary(text, "Twice"),
            [
                entry("L", None, &["this:in"]),
                entry("M", None, &["this:in"])
            ]
        );
    }

    #[test]
    fn a_capture_takes_ref_for_every_kind_of_assignment() {
        let text = "struct P {\n    public mut X: int;\n    public mut Scale(k: int) { X *= k; }\n    \
                    public Get(): int => X;\n}\nBump(ref n: int) { n += 1; }\nF() {\n    \
                    var a = 0;\n    var b = 0;\n    var c = 0;\n    let d = 0;\n    \
                    var p = new P { X = 1 };\n    var q = new P { X = 1 };\n    let r = new P { X = 1 };\n    \
                    G() {\n        a++;\n        --b;\n        Bump(ref c);\n        p.Scale(2);\n        \
                    q.X = 3;\n        let n = r.Get() + d;\n    }\n}\n";
        assert_eq!(
            summary(text, "F"),
            [entry(
                "G",
                None,
                &["a:ref", "b:ref", "c:ref", "d:in", "p:ref", "q:ref", "r:in"]
            )]
        );
    }

    #[test]
    fn a_capture_is_found_in_every_kind_of_code() {
        let text = "struct P {\n    public X: int;\n}\n\
                    F(a: int, b: bool, c: int, d: int, p: P, e: int, f: int, g: int, h: int, \
                    i: int, j: int, k: int, l: int, m: int, n: int, o: bool, q: int, r: bool) {\n    \
                    G(z: int = q) {\n        \
                    let s = $\"{a,c}\";\n        \
                    while (b) { break; }\n        \
                    do { } while (r);\n        \
                    for (var x = 0; x < d; x++) { }\n        \
                    let w = p with { X = e };\n        \
                    let t = p is P;\n        \
                    foreach (let y in [f]) { let yy = y; }\n        \
                    let u = match (g) { 0 => h, _ => 1 };\n        \
                    let v = [i] () => 1;\n        \
                    let nm = nameof(j);\n        \
                    let range = k..l;\n        \
                    let (o1, o2) = (m, m);\n        let oo = o1 + o2;\n        \
                    match (n) { 0 => { }, _ => { } }\n        \
                    if (o) { }\n        \
                    unsafe { let kk = (long)a; }\n        \
                    return;\n    }\n}\n";
        let all = [
            "a:in", "b:in", "c:in", "d:in", "p:in", "e:in", "f:in", "g:in", "h:in", "i:in", "j:in",
            "k:in", "l:in", "m:in", "n:in", "o:in", "q:in", "r:in",
        ];
        assert_eq!(summary(text, "F"), [entry("G", None, &all)]);
    }
}
