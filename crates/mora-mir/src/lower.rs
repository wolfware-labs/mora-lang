//! Lowering from checked HIR to MIR (§14.7).
//!
//! [`mir`] lowers the body of one owner, reading every rule the front end
//! applied from its records: the conversions, callees, arguments and
//! constants of [`mora_hir::Inference`]. The front end has reported every
//! error of the program and lowering runs only on bodies without one, so
//! anything lowering cannot handle is an internal error: it panics with a
//! message that names the body and the expression.
//!
//! A body's locals are the return slot `_0`, the parameters, the locals of
//! the HIR body in the order of the text, then the temporaries. A
//! parameter of mode `in` or `ref` holds a borrow of its value, which is
//! read and written through a dereference; an argument for it is a borrow
//! of a place, and a value that is not one is stored in a temporary first
//! (§10.3). Every value is copied: which copies become moves is for the
//! move checks to decide.
//!
//! A local of the HIR body is live from its declaration to the end of its
//! block, and a temporary from the expression that makes it to the end of
//! its statement; `return`, `break` and `continue` end those of the scopes
//! they leave first. No storage is ended where control never gets, as after
//! a `return`, and no jump starts there: such code ends in `unreachable`.

mod control;
mod expr;

use std::collections::HashMap;

use mora_diag::line_column;
use mora_hir::{
    BodyOwner, BodyResolution, BodySourceMap, ExprId, Inference, ItemId, ParamMode, STD, Signature,
    Stmt, StmtId, Ty, body, body_owners, body_resolution, body_source_map, infer, owner_name,
    signature,
};
use mora_span::{Db, Project, SourceFile, Span, TextRange, line_index};

use crate::types::has_receiver;
use crate::{
    Block, BlockId, Body, BorrowMode, Const, LocalDecl, LocalId, LocalSource, LocalTy, Mir,
    Operand, Place, Rvalue, Statement, StatementKind, Terminator, TerminatorKind, debug_validate,
    write_mir,
};

#[cfg(test)]
mod tests;

/// The MIR of the body of `owner` in `file`. It is checked by
/// [`debug_validate`] before it is returned.
///
/// # Panics
///
/// When the body holds what lowering cannot handle: what the front end
/// should have reported, or what lowering does not handle yet.
#[salsa::tracked(returns(ref))]
pub fn mir(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Mir {
    let mir = Mir {
        bodies: vec![Lowerer::lower(db, project, file, owner)],
    };
    debug_validate(db, &mir, std_loaded(db, project));
    mir
}

/// The text of the MIR of every body of `file` that has code, in the order
/// of [`body_owners`], one blank line between them.
///
/// # Panics
///
/// As [`mir`] does.
pub fn mir_dump(db: &dyn Db, project: Project, file: SourceFile) -> String {
    let mut out = String::new();
    for owner in body_owners(db, file) {
        if body(db, file, owner).root.is_none() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        write_mir(&mut out, db, Some(file), mir(db, project, file, owner));
    }
    out
}

/// Whether the standard library is part of the program: `project` depends
/// on the project whose root namespace is `Std`.
fn std_loaded(db: &dyn Db, project: Project) -> bool {
    project
        .dependencies(db)
        .iter()
        .any(|dependency| dependency.project.root_namespace(db) == STD)
}

/// A body under construction: its locals and blocks, the block statements
/// go to, and the storage to end when scopes and statements end.
///
/// A block is reachable when a reachable block jumps to it; the entry
/// block is. Code is still lowered into a block control never gets to, as
/// after a `return`, but no storage is ended there.
pub(crate) struct Builder {
    /// The body, whose blocks end in `unreachable` until they are ended.
    body: Body,
    /// Whether each block has its terminator.
    ended: Vec<bool>,
    /// Whether each block is reachable.
    reachable: Vec<bool>,
    /// The block statements go to.
    current: BlockId,
    /// The HIR locals of each open scope, outermost first, in the order
    /// of their declarations.
    scopes: Vec<Vec<LocalId>>,
    /// The temporaries of the open statements, in the order they were made.
    temps: Vec<LocalId>,
}

impl Builder {
    /// A body named `name` returning `ret`, with its return slot, an entry
    /// block and one open scope.
    pub(crate) fn new(name: String, span: Span, ret: Ty) -> Builder {
        let mut builder = Builder {
            body: Body {
                name,
                params: 0,
                locals: Vec::new(),
                blocks: Vec::new(),
                span,
            },
            ended: Vec::new(),
            reachable: Vec::new(),
            current: BlockId::ENTRY,
            scopes: vec![Vec::new()],
            temps: Vec::new(),
        };
        builder.local(LocalDecl {
            ty: LocalTy::Value(ret),
            mutable: false,
            source: None,
        });
        builder.new_block();
        builder.reachable[BlockId::ENTRY.index()] = true;
        builder
    }

    /// Declares a parameter. Parameters come right after the return slot.
    pub(crate) fn param(&mut self, decl: LocalDecl) -> LocalId {
        assert_eq!(
            self.body.locals.len(),
            self.body.params + 1,
            "body {}: a parameter after another local",
            self.body.name
        );
        self.body.params += 1;
        self.local(decl)
    }

    /// Declares a local.
    pub(crate) fn local(&mut self, decl: LocalDecl) -> LocalId {
        let id = LocalId(index_u32(self.body.locals.len()));
        self.body.locals.push(decl);
        id
    }

    /// The body so far, to look types up in.
    pub(crate) fn body(&self) -> &Body {
        &self.body
    }

    /// A temporary of type `ty`, live from here to the end of the
    /// statement.
    pub(crate) fn temp(&mut self, ty: LocalTy, span: Span) -> LocalId {
        let id = self.local(LocalDecl {
            ty,
            mutable: false,
            source: None,
        });
        self.push(StatementKind::StorageLive(id), span);
        self.temps.push(id);
        id
    }

    /// Appends a statement to the current block.
    pub(crate) fn push(&mut self, kind: StatementKind, span: Span) {
        let current = self.current.index();
        assert!(
            !self.ended[current],
            "body {}: a statement after the end of bb{current}",
            self.body.name
        );
        self.body.blocks[current]
            .statements
            .push(Statement { kind, span });
    }

    /// Appends `place = rvalue`.
    pub(crate) fn assign(&mut self, place: Place, rvalue: Rvalue, span: Span) {
        self.push(StatementKind::Assign(place, rvalue), span);
    }

    /// A new block, which nothing jumps to yet.
    pub(crate) fn new_block(&mut self) -> BlockId {
        let id = BlockId(index_u32(self.body.blocks.len()));
        self.body.blocks.push(Block {
            statements: Vec::new(),
            terminator: Terminator {
                kind: TerminatorKind::Unreachable,
                span: self.body.span,
            },
        });
        self.ended.push(false);
        self.reachable.push(false);
        id
    }

    /// Ends the current block with `kind` and returns it. The blocks it
    /// jumps to are reachable when it is. Statements go nowhere until
    /// [`Builder::switch_to`] picks the next block.
    pub(crate) fn terminate(&mut self, kind: TerminatorKind, span: Span) -> BlockId {
        let current = self.current;
        assert!(
            !self.ended[current.index()],
            "body {}: bb{} ended twice",
            self.body.name,
            current.0
        );
        if self.reachable[current.index()] {
            for target in successors(&kind) {
                self.reachable[target.index()] = true;
            }
        }
        self.body.blocks[current.index()].terminator = Terminator { kind, span };
        self.ended[current.index()] = true;
        current
    }

    /// Makes `block` the block statements go to.
    pub(crate) fn switch_to(&mut self, block: BlockId) {
        self.current = block;
    }

    /// Ends the current block with `kind`, after which control does not
    /// continue, and goes on in a new block that control never gets to.
    pub(crate) fn terminate_dead_end(&mut self, kind: TerminatorKind, span: Span) {
        self.terminate(kind, span);
        let dead = self.new_block();
        self.switch_to(dead);
    }

    /// A jump that the code after it does not follow, as `return` or
    /// `break`: [`Builder::terminate_dead_end`] with `kind` where control
    /// can get to, nothing where it cannot, so that unreachable code ends in
    /// `unreachable`.
    pub(crate) fn jump(&mut self, kind: TerminatorKind, span: Span) {
        if self.is_reachable() {
            self.terminate_dead_end(kind, span);
        }
    }

    /// Ends the current block with a jump to `target` where control can get
    /// to it, and with `unreachable` where it cannot.
    pub(crate) fn goto(&mut self, target: BlockId, span: Span) {
        let kind = if self.is_reachable() {
            TerminatorKind::Goto(target)
        } else {
            TerminatorKind::Unreachable
        };
        self.terminate(kind, span);
    }

    /// Whether control can get to the current block.
    pub(crate) fn is_reachable(&self) -> bool {
        self.reachable[self.current.index()]
    }

    /// How many scopes are open.
    pub(crate) fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Opens a scope for the HIR locals declared next.
    pub(crate) fn enter_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    /// Starts the storage of the HIR local `local` in the innermost scope.
    pub(crate) fn storage_live(&mut self, local: LocalId, span: Span) {
        self.push(StatementKind::StorageLive(local), span);
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(local);
        }
    }

    /// Closes the innermost scope, ending the storage of its locals in the
    /// reverse order of their declarations.
    ///
    /// # Panics
    ///
    /// When no scope is open.
    pub(crate) fn leave_scope(&mut self, span: Span) {
        let Some(locals) = self.scopes.pop() else {
            panic!("body {}: a scope left that is not open", self.body.name);
        };
        self.storage_dead(locals.into_iter().rev(), span);
    }

    /// Where the temporaries of a statement that starts now begin.
    pub(crate) fn temp_mark(&self) -> usize {
        self.temps.len()
    }

    /// Ends the storage of the temporaries made since `mark`, the last
    /// first, at the end of their statement.
    pub(crate) fn end_temps(&mut self, mark: usize, span: Span) {
        let temps = self.take_temps(mark);
        self.end_storage(&temps, span);
    }

    /// The temporaries made since `mark`, which are no longer the open
    /// statement's to end: the caller ends them with
    /// [`Builder::end_storage`], once on each path control takes.
    pub(crate) fn take_temps(&mut self, mark: usize) -> Vec<LocalId> {
        self.temps.split_off(mark)
    }

    /// Ends the storage of `locals`, the last first.
    pub(crate) fn end_storage(&mut self, locals: &[LocalId], span: Span) {
        self.storage_dead(locals.iter().rev().copied(), span);
    }

    /// Ends the storage of everything that a jump out of the scopes from
    /// `depth` on leaves behind: the temporaries made since `mark`, the
    /// last first, then the locals of those scopes, innermost first. The
    /// scopes and temporaries stay open for the code after the jump.
    pub(crate) fn exit_scopes(&mut self, depth: usize, mark: usize, span: Span) {
        let temps = self.temps[mark..].iter().rev();
        let locals = self.scopes[depth..]
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev());
        let dead: Vec<LocalId> = temps.chain(locals).copied().collect();
        self.storage_dead(dead, span);
    }

    /// Ends the storage of each of `locals` where control can get to.
    fn storage_dead(&mut self, locals: impl IntoIterator<Item = LocalId>, span: Span) {
        if !self.is_reachable() {
            return;
        }
        for local in locals {
            self.push(StatementKind::StorageDead(local), span);
        }
    }

    /// The body. The current block, which control must not get to, ends in
    /// `unreachable` if nothing ended it.
    ///
    /// # Panics
    ///
    /// When the current block has no terminator and is reachable, or
    /// another block has no terminator.
    pub(crate) fn finish(mut self, span: Span) -> Body {
        if !self.ended[self.current.index()] {
            assert!(
                !self.is_reachable(),
                "body {}: the end of the body is reachable",
                self.body.name
            );
            self.terminate(TerminatorKind::Unreachable, span);
        }
        if let Some(open) = self.ended.iter().position(|ended| !ended) {
            panic!("body {}: bb{open} has no terminator", self.body.name);
        }
        self.body
    }
}

/// The blocks control may go to after `kind`.
fn successors(kind: &TerminatorKind) -> Vec<BlockId> {
    match kind {
        TerminatorKind::Goto(target) => vec![*target],
        TerminatorKind::Switch {
            targets, otherwise, ..
        } => targets
            .iter()
            .map(|(_, target)| *target)
            .chain([*otherwise])
            .collect(),
        TerminatorKind::Call { next, .. } => next.iter().copied().collect(),
        TerminatorKind::Return | TerminatorKind::Unreachable => Vec::new(),
    }
}

/// A position in a body's list of locals or blocks, as the id holds it.
fn index_u32(index: usize) -> u32 {
    match u32::try_from(index) {
        Ok(index) => index,
        Err(_) => panic!("a body with more than u32::MAX locals or blocks"),
    }
}

/// The lowering of one body: what the front end knows about it, and the
/// body being built.
pub(crate) struct Lowerer<'a> {
    /// The database the front end's queries read.
    db: &'a dyn Db,
    /// The file the body is in.
    file: SourceFile,
    /// The body's name, for messages.
    name: String,
    /// The HIR body.
    hir: &'a mora_hir::Body,
    /// Where the parts of the HIR body are.
    map: &'a BodySourceMap,
    /// What the paths of the HIR body name.
    resolution: &'a BodyResolution,
    /// The types and records of the HIR body.
    inference: &'a Inference,
    /// The MIR local of each parameter and local of the HIR body.
    locals: HashMap<mora_hir::LocalId, LocalId>,
    /// The MIR body being built.
    builder: Builder,
    /// The loops around the statement being lowered, outermost first.
    loops: Vec<control::Loop>,
}

impl<'a> Lowerer<'a> {
    /// The MIR body of `owner`.
    fn lower(db: &'a dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Body {
        let hir = body(db, file, owner);
        let map = body_source_map(db, file, owner);
        let name = owner_name(db, project, file, owner);
        let Some(root) = hir.root else {
            panic!("internal error lowering {name}: the body has no code");
        };
        let (ret, params) = match owner {
            BodyOwner::TopLevel => (Ty::UNIT, Vec::new()),
            BodyOwner::Item(item) => {
                let item = ItemId {
                    project,
                    file,
                    item,
                };
                match signature(db, item) {
                    Signature::Function(function) if function.params.len() != hir.params.len() => {
                        panic!(
                            "internal error lowering {name}: the signature has {} parameters but the body {}",
                            function.params.len(),
                            hir.params.len()
                        )
                    }
                    Signature::Function(function)
                        if !function.is_async
                            && !function.is_generic
                            && !has_receiver(db, item) =>
                    {
                        let params = function
                            .params
                            .iter()
                            .map(|param| match param.mode {
                                ParamMode::In => LocalTy::Borrow(BorrowMode::In, param.ty.clone()),
                                ParamMode::Ref => {
                                    LocalTy::Borrow(BorrowMode::Ref, param.ty.clone())
                                }
                                ParamMode::Move => LocalTy::Value(param.ty.clone()),
                            })
                            .collect();
                        (function.ret.clone(), params)
                    }
                    _ => panic!(
                        "internal error lowering {name}: methods, generic functions and async functions are not lowered"
                    ),
                }
            }
            _ => panic!(
                "internal error lowering {name}: only functions and top-level statements are lowered"
            ),
        };
        let span = Span::new(file.id(db), map.expr(root));
        let mut lowerer = Lowerer {
            db,
            file,
            builder: Builder::new(name.clone(), span, ret.clone()),
            name,
            hir,
            map,
            resolution: body_resolution(db, project, file, owner),
            inference: infer(db, project, file, owner),
            locals: HashMap::new(),
            loops: Vec::new(),
        };
        for (&local, ty) in hir.params.iter().zip(params) {
            let id = lowerer.builder.param(LocalDecl {
                ty,
                mutable: false,
                source: Some(lowerer.source(local)),
            });
            lowerer.locals.insert(local, id);
        }
        lowerer.declare_locals(root);
        lowerer.root(root, &ret);
        let end = lowerer.end_span(root);
        lowerer.builder.finish(end)
    }

    /// Declares the locals of the HIR body, in the order of the text.
    fn declare_locals(&mut self, root: ExprId) {
        let mut found = Vec::new();
        if let mora_hir::Expr::Block(stmts) = &self.hir.exprs[root] {
            self.collect_locals(stmts, &mut found);
        }
        for (local, init) in found {
            let Some(ty) = self.inference.local(local) else {
                if let Some(init) = init {
                    self.reject_external_value(init);
                }
                self.ice_at(
                    self.map.local(local),
                    &format!("the local `{}` has no type", self.hir.locals[local].name),
                );
            };
            let decl = LocalDecl {
                ty: LocalTy::Value(ty.clone()),
                mutable: self.hir.locals[local].kind == mora_hir::LocalKind::Var,
                source: Some(self.source(local)),
            };
            let id = self.builder.local(decl);
            self.locals.insert(local, id);
        }
    }

    /// The locals `stmts` and the statements inside them declare, each with
    /// its initializer, in the order of the text. A local function's locals
    /// are its own.
    fn collect_locals(
        &self,
        stmts: &[StmtId],
        found: &mut Vec<(mora_hir::LocalId, Option<ExprId>)>,
    ) {
        for &stmt in stmts {
            match &self.hir.stmts[stmt] {
                Stmt::Local { local, init } => found.push((*local, *init)),
                Stmt::Block(inner) => self.collect_locals(inner, found),
                Stmt::If { then, r#else, .. } => {
                    self.collect_locals(&[*then], found);
                    self.collect_locals(r#else.as_slice(), found);
                }
                Stmt::While { body, .. } | Stmt::DoWhile { body, .. } | Stmt::Unsafe(body) => {
                    self.collect_locals(&[*body], found);
                }
                Stmt::For { init, body, .. } => {
                    self.collect_locals(init, found);
                    self.collect_locals(&[*body], found);
                }
                _ => {}
            }
        }
    }

    /// Where `local` comes from.
    fn source(&self, local: mora_hir::LocalId) -> LocalSource {
        LocalSource {
            name: self.hir.locals[local].name.clone(),
            hir: local,
        }
    }

    /// The code of the body: a block, whose end returns `()` when control
    /// gets there, or the expression of `=> e`, which is returned.
    fn root(&mut self, root: ExprId, ret: &Ty) {
        if let mora_hir::Expr::Block(stmts) = &self.hir.exprs[root] {
            self.stmts(stmts);
            if self.builder.is_reachable() {
                if *ret != Ty::UNIT {
                    self.ice(root, "the end of a body that returns a value is reachable");
                }
                self.return_unit(self.end_span(root));
            }
            return;
        }
        let span = self.span(root);
        let mark = self.builder.temp_mark();
        if *ret == Ty::UNIT {
            self.effect(root);
            self.builder.end_temps(mark, span);
            if self.builder.is_reachable() {
                self.return_unit(span);
            }
        } else {
            self.expr_into(root, Place::local(LocalId::RETURN));
            self.builder.end_temps(mark, span);
            self.ret(span);
        }
    }

    /// Where the code of the body ends: its closing brace, or the end of
    /// its last statement or of its expression.
    fn end_span(&self, root: ExprId) -> Span {
        let range = match self.map.ends.get(&root) {
            Some(brace) => *brace,
            None => TextRange::empty(self.map.expr(root).end()),
        };
        Span::new(self.file.id(self.db), range)
    }

    fn stmts(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        match &self.hir.stmts[stmt] {
            Stmt::Expr(expr) => {
                let mark = self.builder.temp_mark();
                self.effect(*expr);
                self.builder.end_temps(mark, self.span(*expr));
            }
            Stmt::Local { local, init } => {
                let id = self.local(*local);
                let at = Span::new(self.file.id(self.db), self.map.local(*local));
                self.builder.storage_live(id, at);
                if let Some(init) = *init {
                    let mark = self.builder.temp_mark();
                    self.expr_into(init, Place::local(id));
                    self.builder.end_temps(mark, self.span(init));
                }
            }
            Stmt::Block(stmts) => {
                self.builder.enter_scope();
                self.stmts(stmts);
                let Some(&end) = self.map.block_ends.get(&stmt) else {
                    panic!(
                        "internal error lowering {}: a block without its closing brace",
                        self.name
                    );
                };
                self.builder
                    .leave_scope(Span::new(self.file.id(self.db), end));
            }
            Stmt::Return(value) => {
                let range = self.map.jumps.get(&stmt).copied().unwrap_or_default();
                let span = Span::new(self.file.id(self.db), range);
                let mark = self.builder.temp_mark();
                match *value {
                    Some(value) => {
                        self.expr_into(value, Place::local(LocalId::RETURN));
                        self.ret(span);
                    }
                    None => self.return_unit(span),
                }
                self.builder.end_temps(mark, span);
            }
            Stmt::If {
                condition,
                then,
                r#else,
            } => self.r#if(stmt, *condition, *then, *r#else),
            Stmt::While { condition, body } => self.r#while(stmt, *condition, *body),
            Stmt::DoWhile { body, condition } => self.do_while(stmt, *body, *condition),
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => self.r#for(stmt, init, *condition, update, *body),
            Stmt::Break => self.loop_jump(stmt, true),
            Stmt::Continue => self.loop_jump(stmt, false),
            Stmt::Unsafe(block) => self.stmt(*block),
            other => panic!(
                "internal error lowering {}: the statement {other:?} is not lowered",
                self.name
            ),
        }
    }

    /// `return` of `()`.
    fn return_unit(&mut self, span: Span) {
        self.builder.assign(
            Place::local(LocalId::RETURN),
            Rvalue::Use(Operand::Const(Const::Unit)),
            span,
        );
        self.ret(span);
    }

    /// `return` of the value in `_0`: the storage of everything live ends
    /// first. What follows goes to a block control never gets to.
    fn ret(&mut self, span: Span) {
        self.builder.exit_scopes(0, 0, span);
        self.builder.jump(TerminatorKind::Return, span);
    }

    /// The MIR local of the HIR local `local`.
    fn local(&self, local: mora_hir::LocalId) -> LocalId {
        match self.locals.get(&local) {
            Some(id) => *id,
            None => self.ice_at(
                self.map.local(local),
                &format!(
                    "`{}` is not a local of the body",
                    self.hir.locals[local].name
                ),
            ),
        }
    }

    /// The span of `expr`.
    fn span(&self, expr: ExprId) -> Span {
        Span::new(self.file.id(self.db), self.map.expr(expr))
    }

    /// The span of the statement `stmt`.
    fn stmt_span(&self, stmt: StmtId) -> Span {
        let Some(&range) = self.map.stmts.get(&stmt) else {
            panic!(
                "internal error lowering {}: a statement without its range",
                self.name
            );
        };
        Span::new(self.file.id(self.db), range)
    }

    /// Panics with an internal error about `expr`.
    fn ice(&self, expr: ExprId, what: &str) -> ! {
        self.ice_at(self.map.expr(expr), what)
    }

    /// Panics with an internal error about the code at `range`.
    fn ice_at(&self, range: TextRange, what: &str) -> ! {
        let text = self.file.text(self.db);
        let code = text
            .get(std::ops::Range::<usize>::from(range))
            .unwrap_or_default();
        let (line, column) = line_column(
            text,
            line_index(self.db, self.file),
            usize::from(range.start()),
        )
        .unwrap_or_default();
        panic!(
            "internal error lowering {}: {what}: `{code}` at {line}:{column}",
            self.name
        );
    }
}
