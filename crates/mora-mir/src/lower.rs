//! Lowering from checked HIR to MIR (§14.7).
//!
//! [`mir`] lowers the body of one owner, then each of its local functions
//! as a body of its own, reading every rule the front end applied from its
//! records: the conversions, callees, arguments and constants of
//! [`mora_hir::Inference`]. The front end has reported every error of the
//! program and lowering runs only on bodies without one, so anything
//! lowering cannot handle is an internal error: it panics with a message
//! that names the body and the expression.
//!
//! A body's locals are the return slot `_0`, the receiver `this` of a
//! method, the parameters, the locals of the HIR body in the order of the
//! text, then the temporaries. The receiver of a method that is not `move`
//! and a parameter of mode `in` or `ref` hold a borrow of their value,
//! which is read and written through a dereference; an argument for one is
//! a borrow of a place, and a value that is not one is stored in a
//! temporary first (§10.3). Every value is copied: which copies become
//! moves is for the move checks to decide.
//!
//! A local of the HIR body is live from its declaration to the end of its
//! block, and a temporary from the expression that makes it to the end of
//! its statement; `return`, `break` and `continue` end those of the scopes
//! they leave first. No storage is ended where control never gets, as after
//! a `return`, and no jump starts there: such code ends in `unreachable`.

mod calls;
mod control;
mod expr;
mod local;
mod structs;

use std::collections::{HashMap, HashSet};

use mora_diag::line_column;
use mora_hir::{
    BodyOwner, BodyResolution, BodySourceMap, ExprId, Inference, ItemId, ItemIdx, ItemKind, STD,
    Signature, Stmt, StmtId, Ty, body, body_owners, body_resolution, body_source_map, infer,
    is_known, item_tree, owner_name, signature, this_type,
};
use mora_span::{Db, Project, SourceFile, Span, TextRange, line_index};

use crate::types::param_ty;
use crate::{
    Block, BlockId, Body, Const, LocalDecl, LocalId, LocalSource, LocalTy, Mir, Operand, Place,
    Rvalue, Statement, StatementKind, Terminator, TerminatorKind, debug_validate, write_mir,
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
    let shared = Owner::new(db, project, file, owner);
    let mut bodies = vec![Lowerer::lower(&shared, &shared.shape(owner))];
    for function in &shared.functions {
        bodies.push(Lowerer::lower(&shared, &shared.function_shape(function)));
    }
    let mir = Mir { bodies };
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
            for target in kind.successors() {
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
        number_densely(&mut self.body);
        self.body
    }
}

/// Drops the blocks that hold nothing and that nothing jumps to, which
/// code after a jump leaves behind, and numbers the others in order, so
/// the numbers have no gaps. The entry block stays first.
fn number_densely(body: &mut Body) {
    let mut targeted = vec![false; body.blocks.len()];
    for block in &body.blocks {
        for target in block.terminator.kind.successors() {
            targeted[target.index()] = true;
        }
    }
    // The new id of each block, `None` for a block that is dropped.
    let mut new_ids = Vec::with_capacity(body.blocks.len());
    let mut kept = 0;
    for (index, block) in body.blocks.iter().enumerate() {
        let empty = block.statements.is_empty()
            && matches!(block.terminator.kind, TerminatorKind::Unreachable);
        if index == BlockId::ENTRY.index() || targeted[index] || !empty {
            new_ids.push(Some(BlockId(index_u32(kept))));
            kept += 1;
        } else {
            new_ids.push(None);
        }
    }
    body.blocks = std::mem::take(&mut body.blocks)
        .into_iter()
        .zip(&new_ids)
        .filter_map(|(block, id)| id.map(|_| block))
        .collect();
    // Only kept blocks are targets.
    let renumber = |target: &mut BlockId| {
        if let Some(new) = new_ids[target.index()] {
            *target = new;
        }
    };
    for block in &mut body.blocks {
        match &mut block.terminator.kind {
            TerminatorKind::Goto(target) => renumber(target),
            TerminatorKind::Switch {
                targets, otherwise, ..
            } => {
                for (_, target) in targets {
                    renumber(target);
                }
                renumber(otherwise);
            }
            TerminatorKind::Call { next, .. } => {
                if let Some(next) = next {
                    renumber(next);
                }
            }
            TerminatorKind::Return | TerminatorKind::Unreachable => {}
        }
    }
}

/// A position in a body's list of locals or blocks, as the id holds it.
fn index_u32(index: usize) -> u32 {
    match u32::try_from(index) {
        Ok(index) => index,
        Err(_) => panic!("a body with more than u32::MAX locals or blocks"),
    }
}

/// What lowering an expression gives when the expression calls a function
/// that does not return: nothing more of the expression is lowered, and
/// lowering goes on in a block that control never gets to.
#[derive(Debug)]
pub(crate) struct Diverged;

/// The result of lowering part of an expression.
pub(crate) type Lowered<T> = Result<T, Diverged>;

/// What every body of one owner shares: the HIR body, which holds the
/// local functions too, what the front end knows about it, and the local
/// functions.
pub(crate) struct Owner<'a> {
    /// The database the front end's queries read.
    db: &'a dyn Db,
    /// The project the owner is in.
    project: Project,
    /// The file the owner is in.
    file: SourceFile,
    /// The owner's name.
    name: String,
    /// The HIR body.
    hir: &'a mora_hir::Body,
    /// Where the parts of the HIR body are.
    map: &'a BodySourceMap,
    /// What the paths of the HIR body name.
    resolution: &'a BodyResolution,
    /// The types and records of the HIR body.
    inference: &'a Inference,
    /// The local functions, in the order of the text: the one at `i` is
    /// body `i + 1` of the owner's MIR.
    functions: Vec<local::LocalFn<'a>>,
    /// The type of `this`, in a method of a struct.
    this_ty: Option<Ty>,
}

/// What a body is: its name, return type, receiver and parameters, and
/// its code.
struct BodyShape {
    /// The name, `Owner.Local` for a local function.
    name: String,
    /// The type it returns.
    ret: Ty,
    /// The type of `this`, for a method.
    this: Option<LocalTy>,
    /// The locals of the HIR body that are its parameters after `this`,
    /// with their types.
    params: Vec<(mora_hir::LocalId, LocalTy)>,
    /// Its code.
    root: ExprId,
}

impl<'a> Owner<'a> {
    fn new(db: &'a dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Owner<'a> {
        let mut shared = Owner {
            db,
            project,
            file,
            name: owner_name(db, project, file, owner),
            hir: body(db, file, owner),
            map: body_source_map(db, file, owner),
            resolution: body_resolution(db, project, file, owner),
            inference: infer(db, project, file, owner),
            functions: Vec::new(),
            this_ty: None,
        };
        if let BodyOwner::Item(item) = owner {
            shared.this_ty = this_type(db, shared.item(item));
        }
        shared.functions = shared.local_functions(owner);
        shared
    }

    /// The shape of the body of `owner` itself.
    fn shape(&self, owner: BodyOwner) -> BodyShape {
        let name = &self.name;
        let Some(root) = self.hir.root else {
            panic!("internal error lowering {name}: the body has no code");
        };
        let (ret, this, params) = match owner {
            BodyOwner::TopLevel => (Ty::UNIT, None, Vec::new()),
            BodyOwner::Item(item) => self.function_signature(self.item(item)),
            BodyOwner::Value(ty, index) => {
                let Some(field) = structs::field_of_value(self.db, self.item(ty), index) else {
                    panic!(
                        "internal error lowering {name}: only the initializer of a field is lowered"
                    );
                };
                (field.ty.clone(), None, Vec::new())
            }
            _ => panic!(
                "internal error lowering {name}: only functions, methods, field initializers and top-level statements are lowered"
            ),
        };
        let params = self.hir.params.iter().copied().zip(params).collect();
        BodyShape {
            name: name.clone(),
            ret,
            this,
            params,
            root,
        }
    }

    /// The return type, the type of `this` and the types of the
    /// parameters of the function or method `item`.
    fn function_signature(&self, item: ItemId) -> (Ty, Option<LocalTy>, Vec<LocalTy>) {
        let name = &self.name;
        let Signature::Function(function) = signature(self.db, item) else {
            panic!("internal error lowering {name}: the owner is not a function");
        };
        if function.params.len() != self.hir.params.len() {
            panic!(
                "internal error lowering {name}: the signature has {} parameters but the body {}",
                function.params.len(),
                self.hir.params.len()
            )
        }
        if function.is_async || function.is_generic {
            panic!("internal error lowering {name}: generic and async functions are not lowered");
        }
        let this = function.receiver.map(|mode| match &self.this_ty {
            Some(ty) => param_ty(mode, ty.clone()),
            None => {
                let tree = item_tree(self.db, item.file);
                let generic_struct = tree
                    .item(item.item)
                    .parent
                    .is_some_and(|parent| tree.item(parent).kind == ItemKind::Struct);
                if generic_struct {
                    panic!("internal error lowering {name}: a method of a generic struct is not lowered");
                }
                panic!(
                    "internal error lowering {name}: a method of a type other than a struct is not lowered"
                )
            }
        });
        let mut params = Vec::with_capacity(function.params.len());
        for param in &function.params {
            if !is_known(&param.ty) {
                panic!(
                    "internal error lowering {name}: the parameter `{}` has a type that is not lowered",
                    param.name
                );
            }
            params.push(param_ty(param.mode, param.ty.clone()));
        }
        if !is_known(&function.ret) {
            panic!("internal error lowering {name}: the return type is not lowered");
        }
        (function.ret.clone(), this, params)
    }

    /// The span of `expr`.
    fn span(&self, expr: ExprId) -> Span {
        self.span_at(self.map.expr(expr))
    }

    /// The span of `range` in the owner's file.
    fn span_at(&self, range: TextRange) -> Span {
        Span::new(self.file.id(self.db), range)
    }

    /// The item `item` of the owner's file.
    fn item(&self, item: ItemIdx) -> ItemId {
        ItemId {
            project: self.project,
            file: self.file,
            item,
        }
    }

    /// Panics with an internal error, about the code at `range`, in the
    /// body `body`.
    fn ice_at(&self, body: &str, range: TextRange, what: &str) -> ! {
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
        panic!("internal error lowering {body}: {what}: `{code}` at {line}:{column}");
    }
}

/// The lowering of one body: what its owner shares with its other bodies,
/// and the body being built.
pub(crate) struct Lowerer<'a> {
    /// The owner the body belongs to.
    owner: &'a Owner<'a>,
    /// The body's name, for messages.
    name: String,
    /// The MIR local of each parameter and local of the HIR body that the
    /// body uses, its captures among them.
    locals: HashMap<mora_hir::LocalId, LocalId>,
    /// The receiver `this`, in a method.
    this: Option<LocalId>,
    /// The MIR body being built.
    builder: Builder,
    /// The loops around the statement being lowered, outermost first.
    loops: Vec<control::Loop>,
}

impl<'a> Lowerer<'a> {
    /// The MIR body `shape` describes. A local whose declaration control
    /// never gets to is not declared: the body is lowered again without it.
    fn lower(owner: &'a Owner<'a>, shape: &BodyShape) -> Body {
        let (body, declared) = Self::lower_declaring(owner, shape, None);
        let lowered: HashSet<mora_hir::LocalId> = body
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter_map(|statement| match statement.kind {
                StatementKind::StorageLive(local) => body.local(local).source.as_ref()?.hir,
                _ => None,
            })
            .collect();
        if lowered.len() == declared {
            return body;
        }
        Self::lower_declaring(owner, shape, Some(&lowered)).0
    }

    /// The MIR body `shape` describes, with the locals of `only` declared,
    /// or with every local of the body; and how many locals of the body it
    /// declares.
    fn lower_declaring(
        owner: &'a Owner<'a>,
        shape: &BodyShape,
        only: Option<&HashSet<mora_hir::LocalId>>,
    ) -> (Body, usize) {
        let span = owner.span(shape.root);
        let mut lowerer = Lowerer {
            owner,
            name: shape.name.clone(),
            locals: HashMap::new(),
            this: None,
            builder: Builder::new(shape.name.clone(), span, shape.ret.clone()),
            loops: Vec::new(),
        };
        if let Some(ty) = &shape.this {
            let this = lowerer.builder.param(LocalDecl {
                ty: ty.clone(),
                mutable: false,
                source: Some(LocalSource {
                    name: "this".to_owned(),
                    hir: None,
                }),
            });
            lowerer.this = Some(this);
        }
        for (local, ty) in &shape.params {
            let id = lowerer.builder.param(LocalDecl {
                ty: ty.clone(),
                mutable: false,
                source: Some(lowerer.source(*local)),
            });
            lowerer.locals.insert(*local, id);
        }
        let declared = lowerer.declare_locals(shape.root, only);
        lowerer.root(shape.root, &shape.ret);
        let end = lowerer.end_span(shape.root);
        (lowerer.builder.finish(end), declared)
    }

    /// Declares the locals of the HIR body that are in `only`, or all of
    /// them, in the order of the text, and returns how many it declared.
    fn declare_locals(&mut self, root: ExprId, only: Option<&HashSet<mora_hir::LocalId>>) -> usize {
        let mut found = Vec::new();
        if let mora_hir::Expr::Block(stmts) = &self.owner.hir.exprs[root] {
            self.collect_locals(stmts, &mut found);
        }
        found.retain(|(local, _)| only.is_none_or(|only| only.contains(local)));
        let declared = found.len();
        for (local, init) in found {
            let Some(ty) = self.owner.inference.local(local) else {
                if let Some(init) = init {
                    self.reject_external_value(init);
                }
                self.ice_at(
                    self.owner.map.local(local),
                    &format!(
                        "the local `{}` has no type",
                        self.owner.hir.locals[local].name
                    ),
                );
            };
            let decl = LocalDecl {
                ty: LocalTy::Value(ty.clone()),
                mutable: self.owner.hir.locals[local].kind == mora_hir::LocalKind::Var,
                source: Some(self.source(local)),
            };
            let id = self.builder.local(decl);
            self.locals.insert(local, id);
        }
        declared
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
            match &self.owner.hir.stmts[stmt] {
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
            name: self.owner.hir.locals[local].name.clone(),
            hir: Some(local),
        }
    }

    /// The code of the body: a block, whose end returns `()` when control
    /// gets there, or the expression of `=> e`, which is returned.
    fn root(&mut self, root: ExprId, ret: &Ty) {
        if let mora_hir::Expr::Block(stmts) = &self.owner.hir.exprs[root] {
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
        if *ret == Ty::UNIT {
            if self.expr_statement(root).is_ok() {
                self.return_unit(span);
            }
        } else {
            let mark = self.builder.temp_mark();
            let value = self.expr_into(root, Place::local(LocalId::RETURN));
            self.builder.end_temps(mark, span);
            if value.is_ok() {
                self.ret(span);
            }
        }
    }

    /// Where the code of the body ends: its closing brace, or the end of
    /// its last statement or of its expression.
    fn end_span(&self, root: ExprId) -> Span {
        let range = match self.owner.map.ends.get(&root) {
            Some(brace) => *brace,
            None => TextRange::empty(self.owner.map.expr(root).end()),
        };
        self.owner.span_at(range)
    }

    /// The statements `stmts`, up to the first that control cannot get to:
    /// code after a `return`, `break`, `continue` or a call that does not
    /// return is not lowered.
    fn stmts(&mut self, stmts: &[StmtId]) {
        for &stmt in stmts {
            if !self.builder.is_reachable() {
                return;
            }
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        match &self.owner.hir.stmts[stmt] {
            Stmt::Expr(expr) => {
                // A call that does not return ends the statement, and the
                // code after it is not lowered.
                let _ = self.expr_statement(*expr);
            }
            Stmt::Local { local, init } => {
                let id = self.local(*local);
                let at = self.owner.span_at(self.owner.map.local(*local));
                self.builder.storage_live(id, at);
                if let Some(init) = *init {
                    let mark = self.builder.temp_mark();
                    let _ = self.expr_into(init, Place::local(id));
                    self.builder.end_temps(mark, self.span(init));
                }
            }
            Stmt::Block(stmts) => {
                self.builder.enter_scope();
                self.stmts(stmts);
                let Some(&end) = self.owner.map.block_ends.get(&stmt) else {
                    panic!(
                        "internal error lowering {}: a block without its closing brace",
                        self.name
                    );
                };
                self.builder.leave_scope(self.owner.span_at(end));
            }
            Stmt::Return(value) => {
                let span = self.stmt_span(stmt);
                let mark = self.builder.temp_mark();
                match *value {
                    Some(value) => {
                        if self.expr_into(value, Place::local(LocalId::RETURN)).is_ok() {
                            self.ret(span);
                        }
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
            Stmt::Break => self.loop_jump(stmt, control::Jump::Break),
            Stmt::Continue => self.loop_jump(stmt, control::Jump::Continue),
            Stmt::Unsafe(block) => self.stmt(*block),
            // A local function is a body of its own.
            Stmt::Function(_) => {}
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
                self.owner.map.local(local),
                &format!(
                    "`{}` is not a local of the body",
                    self.owner.hir.locals[local].name
                ),
            ),
        }
    }

    /// The span of `expr`.
    fn span(&self, expr: ExprId) -> Span {
        self.owner.span(expr)
    }

    /// The span of the statement `stmt`.
    fn stmt_span(&self, stmt: StmtId) -> Span {
        let Some(&range) = self.owner.map.stmts.get(&stmt) else {
            panic!(
                "internal error lowering {}: a statement without its range",
                self.name
            );
        };
        self.owner.span_at(range)
    }

    /// Panics with an internal error about `expr`.
    fn ice(&self, expr: ExprId, what: &str) -> ! {
        self.ice_at(self.owner.map.expr(expr), what)
    }

    /// Panics with an internal error about the code at `range`.
    fn ice_at(&self, range: TextRange, what: &str) -> ! {
        self.owner.ice_at(&self.name, range, what)
    }
}
