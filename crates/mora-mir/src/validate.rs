//! The MIR validator: every body is well formed and typed.
//!
//! [`validate`] returns every problem of a [`Mir`] instead of stopping at the
//! first, and never panics on a malformed body: each place is checked before
//! its type is read.

use std::rc::Rc;

use mora_hir::{
    Primitive, Signature, Ty, converts_implicitly, instance_fields, is_known, signature, this_type,
};
use mora_span::Db;

use crate::types::param_ty;
use crate::{
    AggregateKind, BinOp, Block, BlockId, Body, Callee, CastKind, Const, InterpPiece, LocalId,
    LocalTy, Mir, Operand, Place, Projection, Rvalue, StatementKind, TerminatorKind, UnOp,
};

/// Checks every body of `mir` and returns every problem found, ordered by
/// body, then block, then statement, with the terminator last. Each problem
/// names its place as `fn <body> bb<block>[<statement>]`, or `[term]` for
/// the terminator.
///
/// When `std_loaded` is true the standard library's code is part of the
/// program, so a call to [`Callee::External`] and an
/// [`Rvalue::Interpolate`] are problems: both are replaced by calls of items.
pub fn validate(db: &dyn Db, mir: &Mir, std_loaded: bool) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for body in &mir.bodies {
        Checker {
            db,
            mir,
            body,
            std_loaded,
            errors: &mut errors,
        }
        .body();
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Runs [`validate`] and panics with every problem in debug builds; does
/// nothing in release builds.
///
/// # Panics
///
/// In a debug build, when the MIR is invalid.
pub fn debug_validate(db: &dyn Db, mir: &Mir, std_loaded: bool) {
    if cfg!(debug_assertions)
        && let Err(problems) = validate(db, mir, std_loaded)
    {
        panic!("invalid MIR:\n{}", problems.join("\n"));
    }
}

/// The checks of one body.
struct Checker<'a> {
    db: &'a dyn Db,
    mir: &'a Mir,
    body: &'a Body,
    std_loaded: bool,
    errors: &'a mut Vec<String>,
}

impl Checker<'_> {
    fn error(&mut self, at: &str, message: impl std::fmt::Display) {
        self.errors.push(format!("{at}: {message}"));
    }

    fn body(&mut self) {
        let body = self.body;
        let at = format!("fn {}", body.name);
        if body.locals.is_empty() {
            self.error(&at, "no return slot _0");
        } else if body.params >= body.locals.len() {
            self.error(
                &at,
                format!(
                    "{} parameters need {} locals but the body has {}",
                    body.params,
                    body.params + 1,
                    body.locals.len()
                ),
            );
        }
        if body.blocks.is_empty() {
            self.error(&at, "no blocks");
        }
        for (index, decl) in body.locals.iter().enumerate() {
            let ty = decl.ty.ty();
            if !is_known(ty) {
                self.error(
                    &at,
                    format!("local _{index} has type {ty:?}, which is not a concrete type"),
                );
            }
        }
        for id in body.block_ids() {
            self.block(id, body.block(id));
        }
        self.storage_pairs();
    }

    // Storage markers on every path.

    /// Checks that the storage markers pair on every path from the entry
    /// block: a local is made live only when it is not, ended only when it
    /// is, nothing is live at `return`, and every path into a block brings
    /// the same live locals. A block's live locals are those of the first
    /// path that gets there, in breadth-first order from the entry; each
    /// other path into it is compared with them. Blocks no path gets to are
    /// not checked. The problems are reported in the order of the blocks.
    fn storage_pairs(&mut self) {
        let mut problems = Vec::new();
        self.storage_entries(&mut problems);
        problems.sort_by_key(|&(block, _)| block);
        self.errors
            .extend(problems.into_iter().map(|(_, problem)| problem));
    }

    /// The storage on entry to each block that a path from the entry block
    /// gets to, with each problem of the markers and the block it is in.
    /// Each block is walked once, from the state of the first path into
    /// it, and a state holds only the locals live on that path, so the
    /// work grows with the blocks and their live locals, not with every
    /// local of the body.
    fn storage_entries(&self, problems: &mut Vec<(BlockId, String)>) -> Vec<Option<PathStorage>> {
        let body = self.body;
        let mut entries: Vec<Option<PathStorage>> = vec![None; body.blocks.len()];
        let Some(entry) = entries.first_mut() else {
            return entries;
        };
        *entry = Some(PathStorage::default());
        let mut queue = std::collections::VecDeque::from([BlockId::ENTRY]);
        while let Some(id) = queue.pop_front() {
            let Some(mut path) = entries[id.index()].clone() else {
                continue;
            };
            self.storage_walk(id, &mut path, problems);
            let at = format!("fn {} bb{}[term]", body.name, id.0);
            let kind = &body.block(id).terminator.kind;
            if matches!(kind, TerminatorKind::Return) {
                for local in &path.live {
                    problems.push((id, format!("{at}: return while _{} is live", local.0)));
                }
            }
            for target in kind.successors() {
                match entries.get_mut(target.index()) {
                    Some(slot @ None) => {
                        *slot = Some(path.clone());
                        queue.push_back(target);
                    }
                    Some(Some(entry)) => {
                        if let Some(problem) = join_problem(&path.live, &entry.live) {
                            problems.push((
                                id,
                                format!("{at}: bb{} is entered with {problem}", target.0),
                            ));
                        }
                    }
                    // A missing block is reported by `target`.
                    None => {}
                }
            }
        }
        entries
    }

    /// Applies the storage markers of block `id` to `path`, with each one
    /// that does not pair.
    fn storage_walk(
        &self,
        id: BlockId,
        path: &mut PathStorage,
        problems: &mut Vec<(BlockId, String)>,
    ) {
        let body = self.body;
        let mut ended = Vec::new();
        for (index, statement) in body.block(id).statements.iter().enumerate() {
            let (local, starts) = match statement.kind {
                StatementKind::StorageLive(local) => (local, true),
                StatementKind::StorageDead(local) => (local, false),
                _ => continue,
            };
            // A marker on a local that is not one of the body's, or on the
            // return slot or a parameter, is reported by `storage`.
            if local.index() >= body.locals.len() || local.index() <= body.params {
                continue;
            }
            let problem = match (starts, path.live.binary_search(&local)) {
                (true, Ok(_)) => Some("which is already live"),
                (true, Err(at)) => {
                    path.live.insert(at, local);
                    None
                }
                (false, Ok(at)) => {
                    path.live.remove(at);
                    ended.push(local);
                    None
                }
                (false, Err(_)) if ended.contains(&local) || path.ended_before(local) => {
                    Some("which is already dead")
                }
                (false, Err(_)) => Some("which is not live"),
            };
            if let Some(problem) = problem {
                let marker = if starts {
                    "storage_live"
                } else {
                    "storage_dead"
                };
                problems.push((
                    id,
                    format!(
                        "fn {} bb{}[{index}]: {marker} of _{}, {problem}",
                        body.name, id.0, local.0
                    ),
                ));
            }
        }
        if !ended.is_empty() {
            path.ended = Some(Rc::new(Ended {
                locals: ended,
                earlier: path.ended.take(),
            }));
        }
    }

    fn block(&mut self, id: BlockId, block: &Block) {
        let prefix = format!("fn {} bb{}", self.body.name, id.0);
        for (index, statement) in block.statements.iter().enumerate() {
            let at = format!("{prefix}[{index}]");
            match &statement.kind {
                StatementKind::Assign(dest, rvalue) => self.assign(&at, dest, rvalue),
                StatementKind::StorageLive(local) | StatementKind::StorageDead(local) => {
                    self.storage(&at, *local);
                }
                StatementKind::Nop => {}
            }
        }
        self.terminator(&format!("{prefix}[term]"), &block.terminator.kind);
    }

    fn storage(&mut self, at: &str, local: LocalId) {
        if local.index() >= self.body.locals.len() {
            self.error(at, format!("no local _{}", local.0));
        } else if local.index() <= self.body.params {
            self.error(
                at,
                format!(
                    "storage statement names _{}, which is the return slot or a parameter",
                    local.0
                ),
            );
        }
    }

    /// Reports a target that is not a block of the body.
    fn target(&mut self, at: &str, what: &str, target: BlockId) {
        if target.index() >= self.body.blocks.len() {
            self.error(at, format!("{what} targets missing block bb{}", target.0));
        }
    }

    // Places and operands.

    /// The type at `place`, or `None` after reporting what is wrong with it.
    fn place(&mut self, at: &str, place: &Place) -> Option<Ty> {
        let Some(decl) = self.body.locals.get(place.local.index()) else {
            self.error(at, format!("no local _{}", place.local.0));
            return None;
        };
        let mut borrowed = matches!(decl.ty, LocalTy::Borrow(..));
        let mut prefix = Place::local(place.local);
        for projection in &place.projections {
            match *projection {
                Projection::Deref => {
                    if !borrowed {
                        self.error(
                            at,
                            format!("dereference of a value local _{}", place.local.0),
                        );
                        return None;
                    }
                    borrowed = false;
                }
                Projection::Field(n) => {
                    if borrowed {
                        self.error(
                            at,
                            format!("field of a borrow _{} without a dereference", place.local.0),
                        );
                        return None;
                    }
                    // The prefix is valid, so looking its type up is safe.
                    let ty = self.body.place_ty(&prefix, self.db);
                    if let Some(message) = self.field_problem(&ty, n) {
                        self.error(at, message);
                        return None;
                    }
                }
            }
            prefix.projections.push(*projection);
        }
        Some(self.body.place_ty(&prefix, self.db))
    }

    /// What is wrong with taking field `n` of `ty`.
    fn field_problem(&self, ty: &Ty, n: u32) -> Option<String> {
        let (len, what) = match ty {
            Ty::Struct(item) => match signature(self.db, *item) {
                Signature::Struct(_) => (instance_fields(self.db, *item).count(), "a struct of"),
                _ => return Some("field of a struct type whose item is not a struct".into()),
            },
            Ty::Tuple(elems) => (elems.len(), "a tuple of"),
            _ => {
                return Some(format!(
                    "field {n} of a type that is not a struct or a tuple"
                ));
            }
        };
        (n as usize >= len).then(|| format!("field {n} out of range of {what} {len} fields"))
    }

    fn operand(&mut self, at: &str, operand: &Operand) -> Option<Ty> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.place(at, place),
            Operand::Const(constant) => Some(self.constant(at, constant)),
        }
    }

    /// The type of `constant`, after reporting a primitive type that is not
    /// of its kind.
    fn constant(&mut self, at: &str, constant: &Const) -> Ty {
        match *constant {
            Const::Int(n, primitive) if !primitive.is_integer() => self.error(
                at,
                format!("integer constant {n} of type {primitive:?}, which is not an integer type"),
            ),
            Const::Real(_, primitive) if !primitive.is_floating_point() => self.error(
                at,
                format!("real constant of type {primitive:?}, which is not a floating-point type"),
            ),
            _ => {}
        }
        constant.ty()
    }

    /// The type of the local-like value at `place`: a whole borrow local keeps
    /// its borrow, and any path into a local is a value.
    fn place_lt(&mut self, at: &str, place: &Place) -> Option<LocalTy> {
        let ty = self.place(at, place)?;
        if place.projections.is_empty() {
            Some(self.body.local(place.local).ty.clone())
        } else {
            Some(LocalTy::Value(ty))
        }
    }

    /// Like [`Checker::place_lt`], for an operand.
    fn operand_lt(&mut self, at: &str, operand: &Operand) -> Option<LocalTy> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.place_lt(at, place),
            Operand::Const(constant) => Some(LocalTy::Value(self.constant(at, constant))),
        }
    }

    /// The types of the operands; every operand is checked even after one
    /// fails.
    fn operands<'o>(
        &mut self,
        at: &str,
        operands: impl IntoIterator<Item = &'o Operand>,
    ) -> Option<Vec<Ty>> {
        let mut tys = Some(Vec::new());
        for operand in operands {
            let ty = self.operand(at, operand);
            tys = match (tys, ty) {
                (Some(mut all), Some(ty)) => {
                    all.push(ty);
                    Some(all)
                }
                _ => None,
            };
        }
        tys
    }

    // Statements.

    fn assign(&mut self, at: &str, dest: &Place, rvalue: &Rvalue) {
        let dest_ty = self.place_lt(at, dest);
        let dest_value = dest_ty.as_ref().map(LocalTy::ty);
        if dest_value == Some(&Ty::Never) {
            self.error(at, "assignment to a place of the never type");
        }
        let value_ty = self.rvalue(at, rvalue, dest_value);
        if let (Some(dest_ty), Some(value_ty)) = (dest_ty, value_ty)
            && dest_ty != value_ty
        {
            self.error(
                at,
                format!("assignment of {value_ty:?} to a place of {dest_ty:?}"),
            );
        }
    }

    /// The type of `rvalue`, keeping a borrow a borrow. A tuple takes the
    /// element names of `dest_ty`, since the names are not part of what its
    /// operands carry.
    fn rvalue(&mut self, at: &str, rvalue: &Rvalue, dest_ty: Option<&Ty>) -> Option<LocalTy> {
        let value = match rvalue {
            Rvalue::Use(operand) => return self.operand_lt(at, operand),
            Rvalue::Borrow(mode, place) => {
                return self.place(at, place).map(|ty| LocalTy::Borrow(*mode, ty));
            }
            Rvalue::Unary(op, operand) => self.unary(at, *op, operand),
            Rvalue::Binary(op, left, right) => self.binary(at, *op, left, right),
            Rvalue::Cast(kind, operand, ty) => self.cast(at, *kind, operand, ty),
            Rvalue::Aggregate(AggregateKind::Tuple, operands) => {
                let tys = self.operands(at, operands)?;
                let names = match dest_ty {
                    Some(Ty::Tuple(elems)) if elems.len() == tys.len() => {
                        elems.iter().map(|e| e.name.clone()).collect()
                    }
                    _ => vec![None; tys.len()],
                };
                Some(Ty::Tuple(
                    tys.into_iter()
                        .zip(names)
                        .map(|(ty, name)| mora_hir::TupleElem { name, ty })
                        .collect(),
                ))
            }
            Rvalue::Aggregate(AggregateKind::Struct(item), operands) => {
                let tys = self.operands(at, operands)?;
                let Signature::Struct(_) = signature(self.db, *item) else {
                    self.error(at, "aggregate of an item that is not a struct");
                    return None;
                };
                let fields: Vec<&mora_hir::Field> = instance_fields(self.db, *item)
                    .map(|(_, field)| field)
                    .collect();
                if fields.len() != tys.len() {
                    self.error(
                        at,
                        format!(
                            "struct has {} fields but {} operands",
                            fields.len(),
                            tys.len()
                        ),
                    );
                    return None;
                }
                let mut fine = true;
                for (n, (field, ty)) in fields.iter().zip(&tys).enumerate() {
                    if field.ty != *ty {
                        self.error(
                            at,
                            format!(
                                "operand for field {n} has type {ty:?}, the field has type {:?}",
                                field.ty
                            ),
                        );
                        fine = false;
                    }
                }
                fine.then_some(Ty::Struct(*item))
            }
            Rvalue::Interpolate(pieces) => {
                if self.std_loaded {
                    self.error(at, "interpolation is not lowered once std is loaded");
                }
                let mut fine = true;
                for piece in pieces {
                    let InterpPiece::Operand {
                        value, alignment, ..
                    } = piece
                    else {
                        continue;
                    };
                    fine &= self.operand(at, value).is_some();
                    let Some(alignment) = alignment else { continue };
                    match self.operand(at, alignment) {
                        Some(ty) if is_integer(&ty) => {}
                        Some(ty) => {
                            self.error(at, format!("alignment of type {ty:?}, not an integer"));
                            fine = false;
                        }
                        None => fine = false,
                    }
                }
                fine.then_some(Ty::String)
            }
        };
        value.map(LocalTy::Value)
    }

    fn binary(&mut self, at: &str, op: BinOp, left: &Operand, right: &Operand) -> Option<Ty> {
        let left = self.operand(at, left);
        let right = self.operand(at, right);
        let (left, right) = (left?, right?);
        match op {
            BinOp::Shl | BinOp::Shr | BinOp::UShr => {
                if !is_integer(&left) || !is_integer(&right) {
                    self.error(
                        at,
                        format!("{op:?} of {left:?} by {right:?} needs two integers"),
                    );
                    return None;
                }
                return Some(left);
            }
            _ if left != right => {
                self.error(
                    at,
                    format!("operands of {op:?} have types {left:?} and {right:?}"),
                );
                return None;
            }
            _ => {}
        }
        let fits = match op {
            BinOp::Eq | BinOp::Ne => return Some(Ty::Primitive(Primitive::Bool)),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if is_numeric(&left) || left == Ty::Primitive(Primitive::Char) {
                    return Some(Ty::Primitive(Primitive::Bool));
                }
                false
            }
            BinOp::Add => is_numeric(&left) || left == Ty::String,
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                is_integer(&left) || left == Ty::Primitive(Primitive::Bool)
            }
            _ => is_numeric(&left),
        };
        if !fits {
            self.error(at, format!("{op:?} does not apply to {left:?}"));
            return None;
        }
        Some(left)
    }

    fn unary(&mut self, at: &str, op: UnOp, operand: &Operand) -> Option<Ty> {
        let ty = self.operand(at, operand)?;
        let fits = match op {
            UnOp::Neg => is_numeric(&ty),
            UnOp::Not => ty == Ty::Primitive(Primitive::Bool),
            UnOp::BitNot => is_integer(&ty),
        };
        if !fits {
            self.error(at, format!("{op:?} does not apply to {ty:?}"));
            return None;
        }
        Some(ty)
    }

    fn cast(&mut self, at: &str, kind: CastKind, operand: &Operand, to: &Ty) -> Option<Ty> {
        let from = self.operand(at, operand)?;
        let fits = match kind {
            CastKind::Numeric => is_numeric(&from) && is_numeric(to),
            CastKind::Implicit => widens(&from, to),
        };
        if !fits {
            self.error(at, format!("{kind:?} cast from {from:?} to {to:?}"));
            return None;
        }
        Some(to.clone())
    }

    // Terminators.

    fn terminator(&mut self, at: &str, kind: &TerminatorKind) {
        match kind {
            TerminatorKind::Goto(target) => self.target(at, "goto", *target),
            TerminatorKind::Return | TerminatorKind::Unreachable => {}
            TerminatorKind::Switch {
                discr,
                targets,
                otherwise,
            } => {
                let discr_ty = self.operand(at, discr);
                if let Some(ty) = &discr_ty
                    && *ty != Ty::Primitive(Primitive::Bool)
                    && !is_integer(ty)
                {
                    self.error(
                        at,
                        format!("switch on a value that is not a bool or an integer: {ty:?}"),
                    );
                }
                for (constant, target) in targets {
                    let constant_ty = self.constant(at, constant);
                    if let Some(ty) = &discr_ty
                        && constant_ty != *ty
                    {
                        self.error(
                                at,
                                format!(
                                    "switch constant {constant:?} does not have the type {ty:?} of the discriminant"
                                ),
                            );
                    }
                    self.target(at, "switch", *target);
                }
                self.target(at, "switch", *otherwise);
            }
            TerminatorKind::Call {
                callee,
                args,
                dest,
                next,
            } => {
                let args: Vec<_> = args.iter().map(|arg| self.operand_lt(at, arg)).collect();
                let dest_ty = dest.as_ref().and_then(|dest| self.place(at, dest));
                if dest_ty == Some(Ty::Never) {
                    self.error(at, "call result stored in a place of the never type");
                }
                if let Some(next) = next {
                    self.target(at, "call", *next);
                }
                self.call(at, callee, &args, dest.is_some(), dest_ty);
            }
        }
    }

    fn call(
        &mut self,
        at: &str,
        callee: &Callee,
        args: &[Option<LocalTy>],
        has_dest: bool,
        dest_ty: Option<Ty>,
    ) {
        let Some((params, ret)) = self.callee_shape(at, callee) else {
            if matches!(callee, Callee::External(_)) {
                if self.std_loaded {
                    self.error(at, "external call once std is loaded");
                }
                if has_dest {
                    self.error(at, "external call has a destination");
                }
            }
            return;
        };
        if args.len() != params.len() {
            self.error(
                at,
                format!(
                    "call passes {} arguments but the callee takes {}",
                    args.len(),
                    params.len()
                ),
            );
        }
        for (index, (arg, param)) in args.iter().zip(&params).enumerate() {
            if let Some(arg) = arg
                && arg != param
            {
                self.error(
                    at,
                    format!("argument {index} is {arg:?} but the parameter takes {param:?}"),
                );
            }
        }
        if let Some(dest_ty) = dest_ty
            && dest_ty != ret
        {
            self.error(
                at,
                format!("call destination has type {dest_ty:?} but the callee returns {ret:?}"),
            );
        }
    }

    /// What the callee takes, in order, and what it returns; `None` for an
    /// external callee or after reporting a callee that is not valid. A
    /// method takes its receiver first.
    fn callee_shape(&mut self, at: &str, callee: &Callee) -> Option<(Vec<LocalTy>, Ty)> {
        match callee {
            Callee::Item(item) => match signature(self.db, *item) {
                Signature::Function(function) => {
                    let receiver = match function.receiver {
                        Some(mode) => match this_type(self.db, *item) {
                            Some(ty) => Some(param_ty(mode, ty)),
                            None => {
                                self.error(
                                    at,
                                    "receiver of a method of a type that is not a struct",
                                );
                                return None;
                            }
                        },
                        None => None,
                    };
                    let params = receiver
                        .into_iter()
                        .chain(
                            function
                                .params
                                .iter()
                                .map(|param| param_ty(param.mode, param.ty.clone())),
                        )
                        .collect();
                    Some((params, function.ret.clone()))
                }
                _ => {
                    self.error(at, "callee item is not a function");
                    None
                }
            },
            Callee::Local(index) => {
                let Some(target) = self.mir.bodies.get(*index) else {
                    self.error(at, format!("no local function {index}"));
                    return None;
                };
                if target.locals.len() <= target.params {
                    self.error(
                        at,
                        format!("local function {index} has fewer locals than parameters"),
                    );
                    return None;
                }
                let params = target.locals[1..=target.params]
                    .iter()
                    .map(|local| local.ty.clone())
                    .collect();
                Some((params, target.locals[0].ty.ty().clone()))
            }
            Callee::Initializer(item, n) => {
                let Signature::Struct(_) = signature(self.db, *item) else {
                    self.error(at, "initializer of an item that is not a struct");
                    return None;
                };
                match instance_fields(self.db, *item).nth(*n as usize) {
                    Some((_, field)) => Some((Vec::new(), field.ty.clone())),
                    None => {
                        self.error(at, format!("no field {n} to initialize"));
                        None
                    }
                }
            }
            Callee::External(_) => None,
        }
    }
}

/// The storage of the locals on a path from the entry block.
#[derive(Clone, Debug, Default)]
struct PathStorage {
    /// The locals that are live, in order.
    live: Vec<LocalId>,
    /// The locals ended on the path, most recent blocks first, which tell
    /// a second `storage_dead` from one of a local never made live.
    ended: Option<Rc<Ended>>,
}

impl PathStorage {
    /// Whether the path ended `local` in a block before the current one.
    fn ended_before(&self, local: LocalId) -> bool {
        let mut node = self.ended.as_deref();
        while let Some(ended) = node {
            if ended.locals.contains(&local) {
                return true;
            }
            node = ended.earlier.as_deref();
        }
        false
    }
}

/// The locals one block of a path ends, and those the blocks before it
/// end. Paths that share blocks share these.
#[derive(Debug)]
struct Ended {
    /// The locals the block ends.
    locals: Vec<LocalId>,
    /// What the blocks before it end.
    earlier: Option<Rc<Ended>>,
}

/// How the live locals `here`, on the path being walked, differ from
/// `there`, those of the first path into the same block, both in order;
/// `None` when they are the same.
fn join_problem(here: &[LocalId], there: &[LocalId]) -> Option<String> {
    if here == there {
        return None;
    }
    let names = |locals: &[LocalId], other: &[LocalId]| -> Vec<String> {
        locals
            .iter()
            .filter(|local| other.binary_search(local).is_err())
            .map(|local| format!("_{}", local.0))
            .collect()
    };
    let mut parts = Vec::new();
    let only_here = names(here, there);
    if !only_here.is_empty() {
        parts.push(format!(
            "{} live here but not on another path",
            only_here.join(", ")
        ));
    }
    let only_there = names(there, here);
    if !only_there.is_empty() {
        parts.push(format!(
            "{} live on another path but not here",
            only_there.join(", ")
        ));
    }
    Some(parts.join(", and "))
}

/// Whether `ty` is an integer or floating-point type.
fn is_numeric(ty: &Ty) -> bool {
    matches!(ty, Ty::Primitive(primitive) if primitive.is_numeric())
}

/// Whether `from` converts implicitly to `to`, by the rule inference uses.
fn widens(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Primitive(from), Ty::Primitive(to)) => converts_implicitly(*from, *to),
        _ => false,
    }
}

/// Whether `ty` is one of the integer types.
fn is_integer(ty: &Ty) -> bool {
    matches!(ty, Ty::Primitive(primitive) if primitive.is_integer())
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, app, file_named, item_named};
    use mora_hir::{Construct, ItemId, Primitive, TupleElem, Ty, Unsupported};
    use mora_span::{FileId, Span, TextRange, TextSize};

    use super::{Checker, debug_validate, validate};
    use crate::{
        AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const,
        InterpPiece, LocalDecl, LocalId, LocalTy, Mir, Operand, Place, Projection, Rvalue,
        Statement, StatementKind, Terminator, TerminatorKind, UnOp,
    };

    const SOURCE: &str = "struct P {\n    public A: int = 1;\n    public B: bool;\n}\n\
        Add(a: int, b: int): int { return a; }\n\
        Nop() {}\n\
        class C {\n    M(a: int): int { return a; }\n    static S(a: int): int { return a; }\n}\n\
        Mix(in a: int, ref b: int, move c: int): int { return c; }\n\
        struct Q {\n    public V: int;\n    Get(): int { return V; }\n    mut Set(v: int) {}\n    \
        move Take(): int { return V; }\n}\n\
        struct R {\n    public V: int;\n    public static S: int = 1;\n    public const K: int = 2;\n}\n";

    /// The items of [`SOURCE`].
    struct Fx {
        db: TestDb,
        p: ItemId,
        add: ItemId,
        nop: ItemId,
        method: ItemId,
        static_method: ItemId,
        mix: ItemId,
        get: ItemId,
        set: ItemId,
        take: ItemId,
        q: ItemId,
        r: ItemId,
    }

    fn fx() -> Fx {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", SOURCE)]);
        let file = file_named(&db, project, "src/Program.mr");
        let find = |name: &str| item_named(&db, project, file, name);
        let (p, add, nop, method, static_method) =
            (find("P"), find("Add"), find("Nop"), find("M"), find("S"));
        let (mix, get, set) = (find("Mix"), find("Get"), find("Set"));
        let (take, q, r) = (find("Take"), find("Q"), find("R"));
        Fx {
            db,
            p,
            add,
            nop,
            method,
            static_method,
            mix,
            get,
            set,
            take,
            q,
            r,
        }
    }

    fn span() -> Span {
        Span::new(FileId::new(0), TextRange::empty(TextSize::new(0)))
    }

    fn int() -> Ty {
        Ty::Primitive(Primitive::Int)
    }

    fn boolean() -> Ty {
        Ty::Primitive(Primitive::Bool)
    }

    fn val(ty: Ty) -> LocalDecl {
        LocalDecl {
            ty: LocalTy::Value(ty),
            mutable: true,
            source: None,
        }
    }

    fn borrow(ty: Ty) -> LocalDecl {
        LocalDecl {
            ty: LocalTy::Borrow(BorrowMode::In, ty),
            mutable: false,
            source: None,
        }
    }

    fn borrow_ref(ty: Ty) -> LocalDecl {
        LocalDecl {
            ty: LocalTy::Borrow(BorrowMode::Ref, ty),
            mutable: false,
            source: None,
        }
    }

    fn place(local: u32) -> Place {
        Place::local(LocalId(local))
    }

    fn proj(local: u32, projections: Vec<Projection>) -> Place {
        Place {
            local: LocalId(local),
            projections,
        }
    }

    fn copy(local: u32) -> Operand {
        Operand::Copy(place(local))
    }

    fn int_const(n: i128) -> Operand {
        Operand::Const(Const::Int(n, Primitive::Int))
    }

    fn assign(dest: Place, rvalue: Rvalue) -> Statement {
        Statement {
            kind: StatementKind::Assign(dest, rvalue),
            span: span(),
        }
    }

    fn stmt(kind: StatementKind) -> Statement {
        Statement { kind, span: span() }
    }

    fn term(kind: TerminatorKind) -> Terminator {
        Terminator { kind, span: span() }
    }

    fn block(statements: Vec<Statement>, kind: TerminatorKind) -> Block {
        Block {
            statements,
            terminator: term(kind),
        }
    }

    fn body(name: &str, params: usize, locals: Vec<LocalDecl>, blocks: Vec<Block>) -> Body {
        Body {
            name: name.into(),
            params,
            locals,
            blocks,
            span: span(),
        }
    }

    /// `_0: int`, `_1: int` (parameter), `_2: int`, `_3: bool`, `_4: in int`, `_5: ref int`,
    /// `_6: in bool`; one block.
    fn simple(statements: Vec<Statement>, kind: TerminatorKind) -> Mir {
        let locals = vec![
            val(int()),
            val(int()),
            val(int()),
            val(boolean()),
            borrow(int()),
            borrow_ref(int()),
            borrow(boolean()),
        ];
        Mir {
            bodies: vec![body("F", 1, locals, vec![block(statements, kind)])],
        }
    }

    fn with_stmt(statement: Statement) -> Mir {
        simple(vec![statement], TerminatorKind::Return)
    }

    fn call(callee: Callee, args: Vec<Operand>, dest: Option<Place>) -> Mir {
        simple(
            vec![],
            TerminatorKind::Call {
                callee,
                args,
                dest,
                next: Some(BlockId(1)),
            },
        )
        .with_block(block(vec![], TerminatorKind::Return))
    }

    impl Mir {
        fn with_block(mut self, extra: Block) -> Mir {
            self.bodies[0].blocks.push(extra);
            self
        }
    }

    fn errors(fx: &Fx, mir: &Mir, std_loaded: bool) -> Vec<String> {
        validate(&fx.db, mir, std_loaded).expect_err("the body is invalid")
    }

    fn rejects(fx: &Fx, mir: &Mir, std_loaded: bool, needle: &str) {
        let found = errors(fx, mir, std_loaded);
        assert!(
            found.iter().any(|e| e.contains(needle)),
            "no error contains {needle:?}: {found:?}"
        );
    }

    fn missing_target() -> Mir {
        simple(vec![], TerminatorKind::Goto(BlockId(5)))
    }

    fn type_mismatch() -> Mir {
        with_stmt(assign(
            place(2),
            Rvalue::Use(Operand::Const(Const::Bool(true))),
        ))
    }

    fn switch_on_struct(fx: &Fx) -> Mir {
        let locals = vec![val(int()), val(Ty::Struct(fx.p))];
        Mir {
            bodies: vec![body(
                "F",
                0,
                locals,
                vec![
                    block(
                        vec![],
                        TerminatorKind::Switch {
                            discr: copy(1),
                            targets: vec![],
                            otherwise: BlockId(1),
                        },
                    ),
                    block(vec![], TerminatorKind::Return),
                ],
            )],
        }
    }

    fn wrong_argument_count(fx: &Fx) -> Mir {
        call(Callee::Item(fx.add), vec![int_const(1)], Some(place(2)))
    }

    fn deref_of_value() -> Mir {
        with_stmt(assign(
            place(2),
            Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Deref]))),
        ))
    }

    fn field_out_of_range(fx: &Fx) -> Mir {
        let locals = vec![val(int()), val(Ty::Struct(fx.p)), val(int())];
        Mir {
            bodies: vec![body(
                "F",
                0,
                locals,
                vec![block(
                    vec![assign(
                        place(2),
                        Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Field(2)]))),
                    )],
                    TerminatorKind::Return,
                )],
            )],
        }
    }

    fn external() -> Mir {
        call(
            Callee::External(vec!["Std".into(), "Print".into()]),
            vec![],
            None,
        )
    }

    fn interpolate() -> Mir {
        with_stmt(assign(
            place(2),
            Rvalue::Interpolate(vec![InterpPiece::Text("x".into())]),
        ))
    }

    fn field_on_borrow(fx: &Fx) -> Mir {
        let locals = vec![val(int()), borrow(Ty::Struct(fx.p)), val(int())];
        Mir {
            bodies: vec![body(
                "F",
                1,
                locals,
                vec![block(
                    vec![assign(
                        place(2),
                        Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Field(0)]))),
                    )],
                    TerminatorKind::Return,
                )],
            )],
        }
    }

    #[test]
    fn accepts_a_valid_body() {
        let fx = fx();
        let mir = simple(
            vec![
                stmt(StatementKind::StorageLive(LocalId(2))),
                assign(place(2), Rvalue::Use(copy(1))),
                assign(place(2), Rvalue::Binary(BinOp::Add, copy(1), int_const(2))),
                assign(place(3), Rvalue::Binary(BinOp::Lt, copy(1), copy(2))),
                assign(place(0), Rvalue::Use(copy(2))),
                stmt(StatementKind::StorageDead(LocalId(2))),
                stmt(StatementKind::Nop),
            ],
            TerminatorKind::Return,
        );
        assert_eq!(validate(&fx.db, &mir, true), Ok(()));
        debug_validate(&fx.db, &mir, true);
        assert_eq!(validate(&fx.db, &Mir { bodies: vec![] }, true), Ok(()));
    }

    #[test]
    fn rejects_missing_target() {
        let fx = fx();
        let found = errors(&fx, &missing_target(), true);
        assert_eq!(
            found,
            vec!["fn F bb0[term]: goto targets missing block bb5"]
        );
    }

    #[test]
    fn rejects_missing_target_of_switch_and_call() {
        let fx = fx();
        let switch = simple(
            vec![],
            TerminatorKind::Switch {
                discr: copy(3),
                targets: vec![(Const::Bool(true), BlockId(7))],
                otherwise: BlockId(8),
            },
        );
        let found = errors(&fx, &switch, true);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("bb7") && found[1].contains("bb8"));
        let call = simple(
            vec![],
            TerminatorKind::Call {
                callee: Callee::Item(fx.nop),
                args: vec![],
                dest: None,
                next: Some(BlockId(9)),
            },
        );
        rejects(&fx, &call, true, "missing block bb9");
    }

    #[test]
    fn rejects_missing_local_in_place() {
        let fx = fx();
        let mir = with_stmt(assign(place(2), Rvalue::Use(copy(9))));
        let found = errors(&fx, &mir, true);
        assert_eq!(found, vec!["fn F bb0[0]: no local _9"]);
    }

    #[test]
    fn rejects_type_mismatch_in_assign() {
        let fx = fx();
        let found = errors(&fx, &type_mismatch(), true);
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("fn F bb0[0]: "), "{found:?}");
        assert!(found[0].contains("assignment"), "{found:?}");
    }

    #[test]
    fn rejects_non_bool_switch_on_struct() {
        let fx = fx();
        rejects(
            &fx,
            &switch_on_struct(&fx),
            true,
            "switch on a value that is not",
        );
    }

    #[test]
    fn rejects_switch_constant_of_the_wrong_type() {
        let fx = fx();
        let mir = simple(
            vec![],
            TerminatorKind::Switch {
                discr: copy(1),
                targets: vec![(Const::Bool(true), BlockId(0))],
                otherwise: BlockId(0),
            },
        );
        rejects(&fx, &mir, true, "switch constant");
    }

    #[test]
    fn accepts_switch_on_bool_and_integer() {
        let fx = fx();
        let on_bool = simple(
            vec![],
            TerminatorKind::Switch {
                discr: copy(3),
                targets: vec![(Const::Bool(true), BlockId(0))],
                otherwise: BlockId(0),
            },
        );
        assert_eq!(validate(&fx.db, &on_bool, true), Ok(()));
        let on_int = simple(
            vec![],
            TerminatorKind::Switch {
                discr: copy(1),
                targets: vec![(Const::Int(1, Primitive::Int), BlockId(0))],
                otherwise: BlockId(0),
            },
        );
        assert_eq!(validate(&fx.db, &on_int, true), Ok(()));
        let on_char = simple(
            vec![],
            TerminatorKind::Switch {
                discr: Operand::Const(Const::Char('a')),
                targets: vec![],
                otherwise: BlockId(0),
            },
        );
        rejects(&fx, &on_char, true, "switch on a value that is not");
    }

    #[test]
    fn accepts_switch_on_every_integer_type() {
        let fx = fx();
        for primitive in [
            Primitive::Byte,
            Primitive::SByte,
            Primitive::Short,
            Primitive::UShort,
            Primitive::Int,
            Primitive::UInt,
            Primitive::Long,
            Primitive::ULong,
            Primitive::NInt,
            Primitive::NUInt,
        ] {
            let mir = simple(
                vec![],
                TerminatorKind::Switch {
                    discr: Operand::Const(Const::Int(0, primitive)),
                    targets: vec![],
                    otherwise: BlockId(0),
                },
            );
            assert_eq!(validate(&fx.db, &mir, true), Ok(()), "{primitive:?}");
        }
        for primitive in [Primitive::Float, Primitive::Double, Primitive::Char] {
            let mir = simple(
                vec![],
                TerminatorKind::Switch {
                    discr: Operand::Const(Const::Real(0, primitive)),
                    targets: vec![],
                    otherwise: BlockId(0),
                },
            );
            assert!(validate(&fx.db, &mir, true).is_err(), "{primitive:?}");
        }
    }

    #[test]
    fn rejects_wrong_argument_count() {
        let fx = fx();
        rejects(
            &fx,
            &wrong_argument_count(&fx),
            true,
            "call passes 1 arguments but the callee takes 2",
        );
    }

    #[test]
    fn accepts_calls_with_matching_counts() {
        let fx = fx();
        let add = call(Callee::Item(fx.add), vec![copy(4), copy(4)], Some(place(2)));
        assert_eq!(validate(&fx.db, &add, true), Ok(()));
        let nop = call(Callee::Item(fx.nop), vec![], None);
        assert_eq!(validate(&fx.db, &nop, true), Ok(()));
        // A call that returns int may drop its result.
        let dropped = call(Callee::Item(fx.add), vec![copy(4), copy(4)], None);
        assert_eq!(validate(&fx.db, &dropped, true), Ok(()));
    }

    #[test]
    fn a_static_method_takes_no_receiver() {
        let fx = fx();
        assert_eq!(
            validate(
                &fx.db,
                &call(
                    Callee::Item(fx.static_method),
                    vec![copy(4)],
                    Some(place(2))
                ),
                true
            ),
            Ok(())
        );
        rejects(
            &fx,
            &call(
                Callee::Item(fx.static_method),
                vec![copy(4), copy(4)],
                Some(place(2)),
            ),
            true,
            "takes 1",
        );
    }

    #[test]
    fn a_receiver_of_a_method_of_a_class_is_not_known() {
        let fx = fx();
        rejects(
            &fx,
            &call(
                Callee::Item(fx.method),
                vec![copy(4), copy(4)],
                Some(place(2)),
            ),
            true,
            "receiver of a method of a type that is not a struct",
        );
    }

    #[test]
    fn rejects_call_of_a_struct_item() {
        let fx = fx();
        rejects(
            &fx,
            &call(Callee::Item(fx.p), vec![], None),
            true,
            "is not a function",
        );
    }

    #[test]
    fn rejects_call_destination_of_the_wrong_type() {
        let fx = fx();
        rejects(
            &fx,
            &call(
                Callee::Item(fx.add),
                vec![int_const(1), int_const(2)],
                Some(place(3)),
            ),
            true,
            "returns",
        );
        rejects(
            &fx,
            &call(Callee::Item(fx.nop), vec![], Some(place(2))),
            true,
            "returns",
        );
    }

    #[test]
    fn checks_local_calls() {
        let fx = fx();
        let callee = body(
            "F.Inner",
            2,
            vec![val(boolean()), val(int()), val(int())],
            vec![block(vec![], TerminatorKind::Return)],
        );
        let mut mir = call(
            Callee::Local(1),
            vec![int_const(1), int_const(2)],
            Some(place(3)),
        );
        mir.bodies.push(callee);
        assert_eq!(validate(&fx.db, &mir, true), Ok(()));
        mir.bodies[0].blocks[0].terminator = term(TerminatorKind::Call {
            callee: Callee::Local(1),
            args: vec![int_const(1)],
            dest: Some(place(2)),
            next: Some(BlockId(1)),
        });
        let found = errors(&fx, &mir, true);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("passes 1 arguments but the callee takes 2"));
        assert!(found[1].contains("returns"));
        mir.bodies[0].blocks[0].terminator = term(TerminatorKind::Call {
            callee: Callee::Local(4),
            args: vec![],
            dest: None,
            next: None,
        });
        rejects(&fx, &mir, true, "no local function 4");
    }

    #[test]
    fn checks_initializer_calls() {
        let fx = fx();
        assert_eq!(
            validate(
                &fx.db,
                &call(Callee::Initializer(fx.p, 0), vec![], Some(place(2))),
                true
            ),
            Ok(())
        );
        rejects(
            &fx,
            &call(
                Callee::Initializer(fx.p, 0),
                vec![int_const(1)],
                Some(place(2)),
            ),
            true,
            "takes 0",
        );
        rejects(
            &fx,
            &call(Callee::Initializer(fx.p, 0), vec![], Some(place(3))),
            true,
            "returns",
        );
        rejects(
            &fx,
            &call(Callee::Initializer(fx.p, 5), vec![], None),
            true,
            "no field 5",
        );
        rejects(
            &fx,
            &call(Callee::Initializer(fx.nop, 0), vec![], None),
            true,
            "is not a struct",
        );
    }

    #[test]
    fn rejects_deref_of_value_local() {
        let fx = fx();
        rejects(
            &fx,
            &deref_of_value(),
            true,
            "dereference of a value local _1",
        );
    }

    #[test]
    fn rejects_field_out_of_range() {
        let fx = fx();
        rejects(&fx, &field_out_of_range(&fx), true, "field 2 out of range");
    }

    #[test]
    fn rejects_a_local_of_no_concrete_type() {
        let fx = fx();
        let array = Ty::Unsupported(Unsupported {
            construct: Construct::Array,
            item: None,
            parts: vec![int()],
        });
        let pair = |ty: Ty| {
            Ty::Tuple(vec![
                TupleElem {
                    name: None,
                    ty: int(),
                },
                TupleElem { name: None, ty },
            ])
        };
        for (decl, shown) in [
            (val(Ty::Error), "Error"),
            (val(array.clone()), "Unsupported"),
            (borrow(array), "Unsupported"),
            (val(pair(Ty::Error)), "Error"),
        ] {
            let mut mir = with_stmt(stmt(StatementKind::Nop));
            mir.bodies[0].locals.push(decl);
            let found = errors(&fx, &mir, true);
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(
                found[0].starts_with("fn F: local _7 has type ")
                    && found[0].contains(shown)
                    && found[0].ends_with(", which is not a concrete type"),
                "{found:?}"
            );
        }
        // The never type is concrete: a local of it is declared by a `let`
        // whose value does not return.
        for ty in [Ty::Never, pair(Ty::Never), pair(Ty::String)] {
            let mut mir = with_stmt(stmt(StatementKind::Nop));
            mir.bodies[0].locals.push(val(ty));
            assert_eq!(validate(&fx.db, &mir, true), Ok(()));
        }
    }

    #[test]
    fn rejects_an_assignment_to_a_place_of_the_never_type() {
        let fx = fx();
        let mut mir = with_stmt(assign(place(7), Rvalue::Use(copy(8))));
        mir.bodies[0]
            .locals
            .extend([val(Ty::Never), val(Ty::Never)]);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb0[0]: assignment to a place of the never type"]
        );
        let mut call = call(Callee::Item(fx.nop), vec![], Some(place(7)));
        call.bodies[0].locals.push(val(Ty::Never));
        rejects(
            &fx,
            &call,
            true,
            "call result stored in a place of the never type",
        );
    }

    #[test]
    fn rejects_a_constant_whose_primitive_is_not_of_its_kind() {
        let fx = fx();
        let use_of = |constant: Const| {
            let mut mir = with_stmt(assign(
                place(7),
                Rvalue::Use(Operand::Const(constant.clone())),
            ));
            mir.bodies[0].locals.push(val(constant.ty()));
            mir
        };
        for (constant, problem) in [
            (
                Const::Int(1, Primitive::Double),
                "integer constant 1 of type Double, which is not an integer type",
            ),
            (
                Const::Int(0, Primitive::Bool),
                "integer constant 0 of type Bool, which is not an integer type",
            ),
            (
                Const::Real(0, Primitive::Int),
                "real constant of type Int, which is not a floating-point type",
            ),
        ] {
            assert_eq!(
                errors(&fx, &use_of(constant), true),
                [format!("fn F bb0[0]: {problem}")]
            );
        }
        for constant in [
            Const::Int(1, Primitive::Byte),
            Const::Int(-1, Primitive::NInt),
            Const::Real(0, Primitive::Float),
            Const::Real(0, Primitive::Double),
        ] {
            assert_eq!(validate(&fx.db, &use_of(constant), true), Ok(()));
        }
        // A switch's constants are checked too.
        let mir = simple(
            vec![],
            TerminatorKind::Switch {
                discr: Operand::Const(Const::Real(0, Primitive::Double)),
                targets: vec![(Const::Real(0, Primitive::Long), BlockId(0))],
                otherwise: BlockId(0),
            },
        );
        rejects(
            &fx,
            &mir,
            true,
            "real constant of type Long, which is not a floating-point type",
        );
    }

    #[test]
    fn a_field_projection_counts_the_instance_fields_only() {
        // `R` stores `V` alone: its static field and its constant are not
        // fields of a value.
        let fx = fx();
        let mut mir = with_stmt(assign(
            place(1),
            Rvalue::Use(Operand::Copy(proj(7, vec![Projection::Field(0)]))),
        ));
        mir.bodies[0].locals.push(val(Ty::Struct(fx.r)));
        assert_eq!(validate(&fx.db, &mir, true), Ok(()));
        let mut bad = mir.clone();
        bad.bodies[0].blocks[0].statements[0] = assign(
            place(1),
            Rvalue::Use(Operand::Copy(proj(7, vec![Projection::Field(1)]))),
        );
        rejects(
            &fx,
            &bad,
            true,
            "field 1 out of range of a struct of 1 fields",
        );
        // An aggregate of `R` takes one operand.
        let mut built = with_stmt(assign(
            place(7),
            Rvalue::Aggregate(AggregateKind::Struct(fx.r), vec![int_const(1)]),
        ));
        built.bodies[0].locals.push(val(Ty::Struct(fx.r)));
        assert_eq!(validate(&fx.db, &built, true), Ok(()));
    }

    #[test]
    fn rejects_field_on_borrow_without_deref() {
        let fx = fx();
        rejects(
            &fx,
            &field_on_borrow(&fx),
            true,
            "field of a borrow _1 without a dereference",
        );
        // With the dereference it is fine.
        let mut ok = field_on_borrow(&fx);
        ok.bodies[0].blocks[0].statements = vec![assign(
            place(2),
            Rvalue::Use(Operand::Copy(proj(
                1,
                vec![Projection::Deref, Projection::Field(0)],
            ))),
        )];
        assert_eq!(validate(&fx.db, &ok, true), Ok(()));
    }

    #[test]
    fn rejects_field_of_a_non_aggregate_and_of_a_non_struct_item() {
        let fx = fx();
        let mir = with_stmt(assign(
            place(2),
            Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Field(0)]))),
        ));
        rejects(
            &fx,
            &mir,
            true,
            "field 0 of a type that is not a struct or a tuple",
        );
        let locals = vec![val(int()), val(Ty::Struct(fx.nop)), val(int())];
        let mir = Mir {
            bodies: vec![body(
                "F",
                0,
                locals,
                vec![block(
                    vec![assign(
                        place(2),
                        Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Field(0)]))),
                    )],
                    TerminatorKind::Return,
                )],
            )],
        };
        rejects(&fx, &mir, true, "whose item is not a struct");
    }

    #[test]
    fn tuple_fields_are_checked_and_typed() {
        let fx = fx();
        let tuple = Ty::Tuple(vec![
            TupleElem {
                name: None,
                ty: int(),
            },
            TupleElem {
                name: Some("Ok".into()),
                ty: boolean(),
            },
        ]);
        let locals = vec![val(int()), val(tuple), val(boolean())];
        let mk = |n: u32, dest: u32| Mir {
            bodies: vec![body(
                "F",
                0,
                locals.clone(),
                vec![block(
                    vec![assign(
                        place(dest),
                        Rvalue::Use(Operand::Copy(proj(1, vec![Projection::Field(n)]))),
                    )],
                    TerminatorKind::Return,
                )],
            )],
        };
        assert_eq!(validate(&fx.db, &mk(1, 2), true), Ok(()));
        assert_eq!(validate(&fx.db, &mk(0, 0), true), Ok(()));
        rejects(&fx, &mk(2, 2), true, "field 2 out of range of a tuple of 2");
        assert!(validate(&fx.db, &mk(0, 2), true).is_err());
    }

    #[test]
    fn rejects_external_when_std_loaded() {
        let fx = fx();
        rejects(&fx, &external(), true, "external call");
        assert_eq!(validate(&fx.db, &external(), false), Ok(()));
    }

    #[test]
    fn rejects_interpolate_when_std_loaded() {
        let fx = fx();
        rejects(&fx, &interpolate_into_string(), true, "interpolation");
        assert_eq!(validate(&fx.db, &interpolate_into_string(), false), Ok(()));
        // Once std is loaded the rvalue is rejected whatever its type.
        rejects(&fx, &interpolate(), true, "interpolation");
    }

    fn interpolate_into_string() -> Mir {
        let mut mir = interpolate();
        mir.bodies[0].locals[2] = val(Ty::String);
        mir
    }

    #[test]
    fn interpolate_gives_string_from_operands_of_any_type() {
        let fx = fx();
        let mut mir = with_stmt(assign(
            place(2),
            Rvalue::Interpolate(vec![
                InterpPiece::Text("n=".into()),
                InterpPiece::Operand {
                    value: copy(1),
                    alignment: Some(copy(1)),
                    format: Some("x".into()),
                },
                InterpPiece::Operand {
                    value: copy(3),
                    alignment: Some(Operand::Const(Const::Int(-4, Primitive::Long))),
                    format: None,
                },
            ]),
        ));
        mir.bodies[0].locals[2] = val(Ty::String);
        assert_eq!(validate(&fx.db, &mir, false), Ok(()));
        // An int destination does not hold a string.
        mir.bodies[0].locals[2] = val(int());
        assert!(validate(&fx.db, &mir, false).is_err());
        // Bad operands inside are reported.
        let bad = with_stmt(assign(
            place(2),
            Rvalue::Interpolate(vec![InterpPiece::Operand {
                value: copy(9),
                alignment: None,
                format: None,
            }]),
        ));
        rejects(&fx, &bad, false, "no local _9");
        // The bad operand is the one problem: the string's type is unknown.
        let mut into_int = bad.clone();
        into_int.bodies[0].locals[2] = val(int());
        assert_eq!(
            validate(&fx.db, &into_int, false).map_err(|problems| problems.len()),
            Err(1)
        );
        let bad = with_stmt(assign(
            place(2),
            Rvalue::Interpolate(vec![InterpPiece::Operand {
                value: copy(1),
                alignment: Some(copy(8)),
                format: None,
            }]),
        ));
        rejects(&fx, &bad, false, "no local _8");
        let bad = with_stmt(assign(
            place(2),
            Rvalue::Interpolate(vec![InterpPiece::Operand {
                value: copy(1),
                alignment: Some(Operand::Const(Const::Bool(true))),
                format: None,
            }]),
        ));
        let mut fine = bad.clone();
        fine.bodies[0].locals[2] = val(Ty::String);
        rejects(&fx, &fine, false, "alignment of type");
    }

    #[test]
    fn rejects_external_call_with_dest() {
        let fx = fx();
        let mir = call(
            Callee::External(vec!["Std".into(), "Print".into()]),
            vec![],
            Some(place(2)),
        );
        rejects(&fx, &mir, false, "external call has a destination");
        let without = call(Callee::External(vec!["Std".into()]), vec![], None);
        assert_eq!(validate(&fx.db, &without, false), Ok(()));
    }

    #[test]
    fn storage_statements_name_non_parameter_locals() {
        let fx = fx();
        for local in [0, 1] {
            for kind in [StatementKind::StorageLive, StatementKind::StorageDead] {
                let mir = with_stmt(stmt(kind(LocalId(local))));
                rejects(&fx, &mir, true, "storage statement names");
            }
        }
        let paired = simple(
            vec![
                stmt(StatementKind::StorageLive(LocalId(2))),
                stmt(StatementKind::StorageDead(LocalId(2))),
            ],
            TerminatorKind::Return,
        );
        assert_eq!(validate(&fx.db, &paired, true), Ok(()));
        for kind in [StatementKind::StorageLive, StatementKind::StorageDead] {
            rejects(&fx, &with_stmt(stmt(kind(LocalId(9)))), true, "no local _9");
        }
    }

    #[test]
    fn checks_the_shape_of_a_body() {
        let fx = fx();
        let no_locals = Mir {
            bodies: vec![body(
                "F",
                0,
                vec![],
                vec![block(vec![], TerminatorKind::Return)],
            )],
        };
        rejects(&fx, &no_locals, true, "no return slot");
        let too_many = Mir {
            bodies: vec![body(
                "F",
                2,
                vec![val(int()), val(int())],
                vec![block(vec![], TerminatorKind::Return)],
            )],
        };
        rejects(
            &fx,
            &too_many,
            true,
            "2 parameters need 3 locals but the body has 2",
        );
        let no_blocks = Mir {
            bodies: vec![body("F", 0, vec![val(int())], vec![])],
        };
        rejects(&fx, &no_blocks, true, "no blocks");
    }

    #[test]
    fn binary_operators_type_their_results() {
        let fx = fx();
        for op in [
            BinOp::Add,
            BinOp::Sub,
            BinOp::Mul,
            BinOp::Div,
            BinOp::Rem,
            BinOp::WrappingAdd,
            BinOp::WrappingSub,
            BinOp::WrappingMul,
            BinOp::BitAnd,
            BinOp::BitOr,
            BinOp::BitXor,
        ] {
            let ok = with_stmt(assign(place(2), Rvalue::Binary(op, copy(1), int_const(1))));
            assert_eq!(validate(&fx.db, &ok, true), Ok(()), "{op:?}");
            let into_bool = with_stmt(assign(place(3), Rvalue::Binary(op, copy(1), int_const(1))));
            assert!(validate(&fx.db, &into_bool, true).is_err(), "{op:?}");
            let unequal = with_stmt(assign(
                place(2),
                Rvalue::Binary(op, copy(1), Operand::Const(Const::Bool(true))),
            ));
            rejects(&fx, &unequal, true, "operands of");
        }
        for op in [
            BinOp::Eq,
            BinOp::Ne,
            BinOp::Lt,
            BinOp::Le,
            BinOp::Gt,
            BinOp::Ge,
        ] {
            let ok = with_stmt(assign(place(3), Rvalue::Binary(op, copy(1), int_const(1))));
            assert_eq!(validate(&fx.db, &ok, true), Ok(()), "{op:?}");
            let into_int = with_stmt(assign(place(2), Rvalue::Binary(op, copy(1), int_const(1))));
            assert!(validate(&fx.db, &into_int, true).is_err(), "{op:?}");
            let unequal = with_stmt(assign(
                place(3),
                Rvalue::Binary(op, copy(1), Operand::Const(Const::Bool(true))),
            ));
            rejects(&fx, &unequal, true, "operands of");
        }
    }

    #[test]
    fn ordering_comparisons_need_numeric_or_char_operands() {
        let fx = fx();
        let s = || Operand::Const(Const::String("a".into()));
        let b = || Operand::Const(Const::Bool(true));
        let c = || Operand::Const(Const::Char('a'));
        let f = || Operand::Const(Const::Real(0, Primitive::Double));
        let cmp = |op: BinOp, l: Operand, r: Operand| {
            validate(
                &fx.db,
                &with_stmt(assign(place(3), Rvalue::Binary(op, l, r))),
                true,
            )
        };
        for op in [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            assert_eq!(cmp(op, int_const(1), int_const(2)), Ok(()), "{op:?}");
            assert_eq!(cmp(op, f(), f()), Ok(()), "{op:?}");
            assert_eq!(cmp(op, c(), c()), Ok(()), "{op:?}");
            assert!(cmp(op, b(), b()).is_err(), "{op:?}");
            assert!(cmp(op, s(), s()).is_err(), "{op:?}");
            assert!(cmp(op, copy(4), copy(4)).is_ok(), "{op:?}");
        }
        // Equality is open to any equal types.
        for op in [BinOp::Eq, BinOp::Ne] {
            assert_eq!(cmp(op, b(), b()), Ok(()), "{op:?}");
            assert_eq!(cmp(op, s(), s()), Ok(()), "{op:?}");
            assert_eq!(cmp(op, c(), c()), Ok(()), "{op:?}");
        }
    }

    #[test]
    fn shifts_take_any_integer_on_the_right() {
        let fx = fx();
        let long = Operand::Const(Const::Int(1, Primitive::Long));
        for op in [BinOp::Shl, BinOp::Shr, BinOp::UShr] {
            let ok = with_stmt(assign(place(2), Rvalue::Binary(op, copy(1), long.clone())));
            assert_eq!(validate(&fx.db, &ok, true), Ok(()), "{op:?}");
            // The result has the type of the left operand.
            let wrong = with_stmt(assign(place(3), Rvalue::Binary(op, copy(1), long.clone())));
            assert!(validate(&fx.db, &wrong, true).is_err(), "{op:?}");
            let not_integer = with_stmt(assign(
                place(2),
                Rvalue::Binary(op, copy(1), Operand::Const(Const::Bool(true))),
            ));
            rejects(&fx, &not_integer, true, "needs two integers");
        }
    }

    #[test]
    fn unary_cast_borrow_and_use_are_typed() {
        let fx = fx();
        for op in [UnOp::Neg, UnOp::BitNot] {
            let ok = with_stmt(assign(place(2), Rvalue::Unary(op, copy(1))));
            assert_eq!(validate(&fx.db, &ok, true), Ok(()));
            let bad = with_stmt(assign(place(3), Rvalue::Unary(op, copy(1))));
            assert!(validate(&fx.db, &bad, true).is_err());
        }
        let not = with_stmt(assign(place(3), Rvalue::Unary(UnOp::Not, copy(3))));
        assert_eq!(validate(&fx.db, &not, true), Ok(()));
        let cast = |dest: u32, ty: Ty| {
            with_stmt(assign(
                place(dest),
                Rvalue::Cast(CastKind::Numeric, copy(1), ty),
            ))
        };
        let long = Ty::Primitive(Primitive::Long);
        let mut ok = cast(2, long.clone());
        ok.bodies[0].locals[2] = val(long);
        assert_eq!(validate(&fx.db, &ok, true), Ok(()));
        assert!(validate(&fx.db, &cast(2, boolean()), true).is_err());
        let moved = with_stmt(assign(place(2), Rvalue::Use(Operand::Move(place(1)))));
        assert_eq!(validate(&fx.db, &moved, true), Ok(()));
        let borrowed = with_stmt(assign(place(4), Rvalue::Borrow(BorrowMode::In, place(1))));
        assert_eq!(validate(&fx.db, &borrowed, true), Ok(()));
        let wrong = with_stmt(assign(place(3), Rvalue::Borrow(BorrowMode::In, place(1))));
        assert!(validate(&fx.db, &wrong, true).is_err());
        let missing = with_stmt(assign(place(2), Rvalue::Borrow(BorrowMode::In, place(9))));
        rejects(&fx, &missing, true, "no local _9");
        let missing_dest = with_stmt(assign(place(9), Rvalue::Use(copy(1))));
        rejects(&fx, &missing_dest, true, "no local _9");
        let bad_cast = with_stmt(assign(
            place(2),
            Rvalue::Cast(CastKind::Implicit, copy(9), int()),
        ));
        rejects(&fx, &bad_cast, true, "no local _9");
        let bad_unary = with_stmt(assign(place(2), Rvalue::Unary(UnOp::Neg, copy(9))));
        rejects(&fx, &bad_unary, true, "no local _9");
    }

    #[test]
    fn aggregates_are_checked_against_their_parts() {
        let fx = fx();
        let pair = Ty::Tuple(vec![
            TupleElem {
                name: Some("X".into()),
                ty: int(),
            },
            TupleElem {
                name: None,
                ty: boolean(),
            },
        ]);
        let tuple = |dest_ty: Ty, ops: Vec<Operand>| {
            let mut mir = with_stmt(assign(
                place(2),
                Rvalue::Aggregate(AggregateKind::Tuple, ops),
            ));
            mir.bodies[0].locals[2] = val(dest_ty);
            mir
        };
        assert_eq!(
            validate(&fx.db, &tuple(pair.clone(), vec![copy(1), copy(3)]), true),
            Ok(())
        );
        assert!(validate(&fx.db, &tuple(pair.clone(), vec![copy(3), copy(1)]), true).is_err());
        assert!(validate(&fx.db, &tuple(pair.clone(), vec![copy(1)]), true).is_err());
        // Extra operands are not dropped to fit a shorter destination.
        let single = Ty::Tuple(vec![TupleElem {
            name: Some("X".into()),
            ty: int(),
        }]);
        assert!(validate(&fx.db, &tuple(single, vec![copy(1), copy(3)]), true).is_err());
        assert!(validate(&fx.db, &tuple(int(), vec![copy(1), copy(3)]), true).is_err());
        assert_eq!(validate(&fx.db, &tuple(Ty::UNIT, vec![]), true), Ok(()));
        rejects(
            &fx,
            &tuple(pair, vec![copy(9), copy(3)]),
            true,
            "no local _9",
        );

        let make = |dest_ty: Ty, ops: Vec<Operand>, item: ItemId| {
            let mut mir = with_stmt(assign(
                place(2),
                Rvalue::Aggregate(AggregateKind::Struct(item), ops),
            ));
            mir.bodies[0].locals[2] = val(dest_ty);
            mir
        };
        let p = Ty::Struct(fx.p);
        assert_eq!(
            validate(&fx.db, &make(p.clone(), vec![copy(1), copy(3)], fx.p), true),
            Ok(())
        );
        rejects(
            &fx,
            &make(p.clone(), vec![copy(1)], fx.p),
            true,
            "2 fields but 1 operands",
        );
        rejects(
            &fx,
            &make(p.clone(), vec![copy(3), copy(3)], fx.p),
            true,
            "field 0",
        );
        assert!(validate(&fx.db, &make(int(), vec![copy(1), copy(3)], fx.p), true).is_err());
        rejects(&fx, &make(p, vec![], fx.nop), true, "is not a struct");
    }

    #[test]
    fn reports_every_problem_in_block_then_statement_order() {
        let fx = fx();
        let locals = vec![val(int()), val(int()), val(boolean())];
        let mir = Mir {
            bodies: vec![
                body(
                    "App.Max",
                    1,
                    locals.clone(),
                    vec![
                        block(
                            vec![
                                assign(place(2), Rvalue::Use(int_const(1))),
                                assign(place(2), Rvalue::Use(int_const(1))),
                            ],
                            TerminatorKind::Goto(BlockId(4)),
                        ),
                        block(vec![], TerminatorKind::Return),
                        block(
                            vec![
                                stmt(StatementKind::Nop),
                                assign(place(2), Rvalue::Use(int_const(1))),
                            ],
                            TerminatorKind::Unreachable,
                        ),
                    ],
                ),
                body(
                    "App.Other",
                    0,
                    vec![val(int())],
                    vec![block(vec![], TerminatorKind::Goto(BlockId(3)))],
                ),
            ],
        };
        let found = errors(&fx, &mir, true);
        assert_eq!(found.len(), 5, "{found:?}");
        assert!(found[0].starts_with("fn App.Max bb0[0]: "));
        assert!(found[1].starts_with("fn App.Max bb0[1]: "));
        assert!(found[2].starts_with("fn App.Max bb0[term]: "));
        assert!(found[3].starts_with("fn App.Max bb2[1]: "));
        assert!(found[4].starts_with("fn App.Other bb0[term]: "));
        assert_eq!(found, errors(&fx, &mir, true));
    }

    #[test]
    fn a_bad_place_is_reported_once_not_twice() {
        let fx = fx();
        let mir = with_stmt(assign(
            place(2),
            Rvalue::Binary(BinOp::Add, copy(9), copy(8)),
        ));
        let found = errors(&fx, &mir, true);
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn no_bad_body_panics() {
        let fx = fx();
        let bad: Vec<(Mir, bool)> = vec![
            (missing_target(), true),
            (type_mismatch(), true),
            (switch_on_struct(&fx), true),
            (wrong_argument_count(&fx), true),
            (deref_of_value(), true),
            (field_out_of_range(&fx), true),
            (external(), true),
            (interpolate(), true),
            (field_on_borrow(&fx), true),
            (
                call(Callee::External(vec![]), vec![], Some(place(2))),
                false,
            ),
            (with_stmt(assign(place(7), Rvalue::Use(copy(8)))), true),
            (
                with_stmt(assign(
                    place(2),
                    Rvalue::Use(Operand::Copy(proj(
                        1,
                        vec![Projection::Deref, Projection::Deref],
                    ))),
                )),
                true,
            ),
            (
                with_stmt(assign(
                    place(2),
                    Rvalue::Use(Operand::Copy(proj(
                        1,
                        vec![Projection::Field(0), Projection::Field(0)],
                    ))),
                )),
                true,
            ),
            (
                call(Callee::Local(99), vec![copy(77)], Some(place(66))),
                true,
            ),
            (call(Callee::Initializer(fx.add, 3), vec![], None), true),
            (
                with_stmt(stmt(StatementKind::StorageLive(LocalId(40)))),
                true,
            ),
            (
                Mir {
                    bodies: vec![body("E", 3, vec![], vec![])],
                },
                true,
            ),
        ];
        for (index, (mir, std_loaded)) in bad.iter().enumerate() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                validate(&fx.db, mir, *std_loaded)
            }));
            let result = result.unwrap_or_else(|_| panic!("body {index} made validate panic"));
            assert!(result.is_err(), "body {index} was accepted");
        }
    }

    fn args(list: &[Operand]) -> Vec<Operand> {
        list.to_vec()
    }

    #[test]
    fn call_arguments_must_match_parameter_modes() {
        let fx = fx();
        let ok = |callee: Callee, list: &[Operand]| {
            validate(&fx.db, &call(callee, args(list), Some(place(2))), true)
        };
        // in, ref, move
        assert_eq!(
            ok(Callee::Item(fx.mix), &[copy(4), copy(5), copy(1)]),
            Ok(())
        );
        let bad = |list: &[Operand], needle: &str| {
            rejects(
                &fx,
                &call(Callee::Item(fx.mix), args(list), Some(place(2))),
                true,
                needle,
            );
        };
        // A constant or value for an `in` parameter.
        bad(&[int_const(1), copy(5), copy(1)], "argument 0");
        bad(&[copy(1), copy(5), copy(1)], "argument 0");
        // The wrong mode.
        bad(&[copy(5), copy(5), copy(1)], "argument 0");
        bad(&[copy(4), copy(4), copy(1)], "argument 1");
        // The wrong type.
        bad(&[copy(6), copy(5), copy(1)], "argument 0");
        // A borrow where a value is moved, and a value of the wrong type.
        bad(&[copy(4), copy(5), copy(4)], "argument 2");
        bad(&[copy(4), copy(5), copy(3)], "argument 2");
        // The value behind a borrow moves into a `move` parameter, but is not
        // a borrow for an `in` parameter.
        let behind = Operand::Copy(proj(4, vec![Projection::Deref]));
        assert_eq!(
            ok(Callee::Item(fx.mix), &[copy(4), copy(5), behind.clone()]),
            Ok(())
        );
        bad(&[behind, copy(5), copy(1)], "argument 0");
    }

    /// [`call`] with `_7: in Q`, `_8: ref Q`, `_9: Q` and `_10: in P` after
    /// the locals of [`simple`].
    fn call_with_q(fx: &Fx, callee: Callee, list: &[Operand], dest: Option<Place>) -> Mir {
        let mut mir = call(callee, args(list), dest);
        let q = Ty::Struct(fx.q);
        mir.bodies[0].locals.extend([
            borrow(q.clone()),
            borrow_ref(q.clone()),
            val(q),
            borrow(Ty::Struct(fx.p)),
        ]);
        mir
    }

    #[test]
    fn receivers_have_the_mode_and_type_of_the_method() {
        let fx = fx();
        // Q.Get(): an `in` borrow of a Q, and nothing else.
        let get = |list: &[Operand]| call_with_q(&fx, Callee::Item(fx.get), list, Some(place(2)));
        assert_eq!(validate(&fx.db, &get(&[copy(7)]), true), Ok(()));
        rejects(&fx, &get(&[]), true, "takes 1");
        rejects(&fx, &get(&[copy(7), copy(7)]), true, "takes 1");
        rejects(&fx, &get(&[copy(8)]), true, "argument 0");
        rejects(&fx, &get(&[copy(9)]), true, "argument 0");
        rejects(&fx, &get(&[copy(10)]), true, "argument 0");
        rejects(&fx, &get(&[copy(4)]), true, "argument 0");
        // Q.Set(v) is `mut`: a `ref` borrow of a Q, then an `in int`.
        let set = |list: &[Operand]| call_with_q(&fx, Callee::Item(fx.set), list, None);
        assert_eq!(validate(&fx.db, &set(&[copy(8), copy(4)]), true), Ok(()));
        rejects(&fx, &set(&[copy(7), copy(4)]), true, "argument 0");
        rejects(&fx, &set(&[copy(8)]), true, "takes 2");
        rejects(&fx, &set(&[copy(8), copy(1)]), true, "argument 1");
        // Q.Take() is `move`: a Q by value.
        let take = |list: &[Operand]| call_with_q(&fx, Callee::Item(fx.take), list, Some(place(2)));
        assert_eq!(validate(&fx.db, &take(&[copy(9)]), true), Ok(()));
        rejects(&fx, &take(&[copy(7)]), true, "argument 0");
    }

    #[test]
    fn local_callees_take_the_parameter_types_of_their_body() {
        let fx = fx();
        let callee = body(
            "F.Inner",
            2,
            vec![val(boolean()), borrow(int()), val(int())],
            vec![block(vec![], TerminatorKind::Return)],
        );
        let mk = |list: &[Operand]| {
            let mut mir = call(Callee::Local(1), args(list), None);
            mir.bodies.push(callee.clone());
            mir
        };
        assert_eq!(validate(&fx.db, &mk(&[copy(4), copy(1)]), true), Ok(()));
        rejects(&fx, &mk(&[copy(1), copy(1)]), true, "argument 0");
        rejects(&fx, &mk(&[copy(5), copy(1)]), true, "argument 0");
        rejects(&fx, &mk(&[copy(4), copy(4)]), true, "argument 1");
        rejects(&fx, &mk(&[copy(4), copy(3)]), true, "argument 1");
    }

    #[test]
    fn whole_local_assignments_compare_local_types() {
        let fx = fx();
        let one = |dest: u32, rvalue: Rvalue| with_stmt(assign(place(dest), rvalue));
        // A borrow copied between borrow locals of the same kind is fine.
        assert_eq!(
            validate(&fx.db, &one(4, Rvalue::Use(copy(4))), true),
            Ok(())
        );
        // A borrow local read into a value local.
        rejects(&fx, &one(2, Rvalue::Use(copy(4))), true, "assignment");
        // A value into a borrow local.
        rejects(&fx, &one(4, Rvalue::Use(copy(1))), true, "assignment");
        rejects(&fx, &one(4, Rvalue::Use(int_const(1))), true, "assignment");
        // A borrow of the right mode and type.
        assert_eq!(
            validate(
                &fx.db,
                &one(4, Rvalue::Borrow(BorrowMode::In, place(1))),
                true
            ),
            Ok(())
        );
        assert_eq!(
            validate(
                &fx.db,
                &one(5, Rvalue::Borrow(BorrowMode::Ref, place(2))),
                true
            ),
            Ok(())
        );
        // The wrong mode, the wrong inner type, a value destination.
        rejects(
            &fx,
            &one(4, Rvalue::Borrow(BorrowMode::Ref, place(1))),
            true,
            "assignment",
        );
        rejects(
            &fx,
            &one(5, Rvalue::Borrow(BorrowMode::In, place(1))),
            true,
            "assignment",
        );
        rejects(
            &fx,
            &one(4, Rvalue::Borrow(BorrowMode::In, place(3))),
            true,
            "assignment",
        );
        rejects(
            &fx,
            &one(2, Rvalue::Borrow(BorrowMode::In, place(1))),
            true,
            "assignment",
        );
        // A deref reads the value behind the borrow.
        let deref = Operand::Copy(proj(4, vec![Projection::Deref]));
        assert_eq!(validate(&fx.db, &one(2, Rvalue::Use(deref)), true), Ok(()));
    }

    #[test]
    fn operators_need_fitting_operand_types() {
        let fx = fx();
        let s = || Operand::Const(Const::String("a".into()));
        let b = || Operand::Const(Const::Bool(true));
        let f = || Operand::Const(Const::Real(0, Primitive::Double));
        let into = |ty: Ty, rvalue: Rvalue| {
            let mut mir = with_stmt(assign(place(2), rvalue));
            mir.bodies[0].locals[2] = val(ty);
            mir
        };
        let bin = |op: BinOp, l: Operand, r: Operand, ty: Ty| {
            validate(&fx.db, &into(ty, Rvalue::Binary(op, l, r)), true)
        };
        let double = Ty::Primitive(Primitive::Double);
        // Arithmetic: numeric, and string only for Add.
        assert_eq!(bin(BinOp::Add, s(), s(), Ty::String), Ok(()));
        assert_eq!(bin(BinOp::Mul, f(), f(), double.clone()), Ok(()));
        for op in [
            BinOp::Sub,
            BinOp::Mul,
            BinOp::Div,
            BinOp::Rem,
            BinOp::WrappingAdd,
        ] {
            assert!(bin(op, s(), s(), Ty::String).is_err(), "{op:?}");
            assert!(bin(op, b(), b(), boolean()).is_err(), "{op:?}");
        }
        assert!(bin(BinOp::Add, b(), b(), boolean()).is_err());
        // Bitwise: integer, or bool.
        for op in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert_eq!(bin(op, b(), b(), boolean()), Ok(()), "{op:?}");
            assert_eq!(bin(op, int_const(1), int_const(1), int()), Ok(()), "{op:?}");
            assert!(bin(op, f(), f(), double.clone()).is_err(), "{op:?}");
            assert!(bin(op, s(), s(), Ty::String).is_err(), "{op:?}");
        }
        // Shifts: integers only.
        for op in [BinOp::Shl, BinOp::Shr, BinOp::UShr] {
            assert!(bin(op, b(), int_const(1), boolean()).is_err(), "{op:?}");
            assert!(
                bin(op, f(), int_const(1), double.clone()).is_err(),
                "{op:?}"
            );
        }
        // Unary.
        let un =
            |op: UnOp, o: Operand, ty: Ty| validate(&fx.db, &into(ty, Rvalue::Unary(op, o)), true);
        assert_eq!(un(UnOp::Not, b(), boolean()), Ok(()));
        assert!(un(UnOp::Not, int_const(1), int()).is_err());
        assert_eq!(un(UnOp::BitNot, int_const(1), int()), Ok(()));
        assert!(un(UnOp::BitNot, b(), boolean()).is_err());
        assert!(un(UnOp::BitNot, f(), double.clone()).is_err());
        assert_eq!(un(UnOp::Neg, f(), double.clone()), Ok(()));
        assert_eq!(un(UnOp::Neg, int_const(1), int()), Ok(()));
        assert!(un(UnOp::Neg, b(), boolean()).is_err());
        assert!(un(UnOp::Neg, s(), Ty::String).is_err());
    }

    #[test]
    fn casts_follow_their_kind() {
        let fx = fx();
        // A constant 1 of the type.
        let one = |ty: Primitive| {
            if ty.is_floating_point() {
                Const::Real(1f64.to_bits(), ty)
            } else {
                Const::Int(1, ty)
            }
        };
        let cast = |kind: CastKind, from: Primitive, to: Primitive| {
            let mut mir = with_stmt(assign(
                place(2),
                Rvalue::Cast(kind, Operand::Const(one(from)), Ty::Primitive(to)),
            ));
            mir.bodies[0].locals[2] = val(Ty::Primitive(to));
            validate(&fx.db, &mir, true).is_ok()
        };
        use CastKind::{Implicit, Numeric};
        use Primitive::*;
        // Implicit casts widen.
        for (from, to) in [
            (Byte, UShort),
            (Byte, Short),
            (SByte, Int),
            (Short, Long),
            (UShort, UInt),
            (UInt, Long),
            (Int, Long),
            (Byte, ULong),
            (Short, Float),
            (UShort, Double),
            (Int, Double),
            (UInt, Double),
            (Float, Double),
        ] {
            assert!(cast(Implicit, from, to), "{from:?} to {to:?}");
        }
        // They never narrow, change signedness the wrong way, or go inexact.
        for (from, to) in [
            (Int, Short),
            (Long, Int),
            (Int, UInt),
            (SByte, Byte),
            (Short, UShort),
            (Int, Float),
            (Long, Double),
            (Double, Float),
            (Double, Int),
            (Int, Bool),
            (Bool, Int),
            (Char, Int),
            (Int, Char),
            (ULong, Long),
            (NInt, Long),
            (NInt, Int),
            (Int, NInt),
            (Short, NInt),
            (Byte, NUInt),
            (NUInt, ULong),
        ] {
            assert!(!cast(Implicit, from, to), "{from:?} to {to:?}");
        }
        assert!(cast(Implicit, Byte, Int));
        // Numeric casts go between any two numeric types, and only those.
        assert!(cast(Numeric, Long, Short));
        assert!(cast(Numeric, Double, Int));
        assert!(cast(Numeric, Int, Float));
        assert!(cast(Numeric, Int, Int));
        assert!(!cast(Numeric, Int, Bool));
        assert!(!cast(Numeric, Bool, Int));
        assert!(!cast(Numeric, Char, Int));
        let to_string = with_stmt(assign(
            place(2),
            Rvalue::Cast(Numeric, int_const(1), Ty::String),
        ));
        assert!(validate(&fx.db, &to_string, true).is_err());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "invalid MIR")]
    fn debug_validate_panics_on_a_bad_body() {
        let fx = fx();
        debug_validate(&fx.db, &missing_target(), true);
    }

    // Storage markers.

    fn live(local: u32) -> Statement {
        stmt(StatementKind::StorageLive(LocalId(local)))
    }

    fn dead(local: u32) -> Statement {
        stmt(StatementKind::StorageDead(LocalId(local)))
    }

    /// `simple`'s locals with `blocks`.
    fn blocks(blocks: Vec<Block>) -> Mir {
        let mut mir = simple(vec![], TerminatorKind::Return);
        mir.bodies[0].blocks = blocks;
        mir
    }

    #[test]
    fn rejects_storage_dead_of_a_local_never_live() {
        let fx = fx();
        let mir = simple(vec![dead(2)], TerminatorKind::Return);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb0[0]: storage_dead of _2, which is not live"]
        );
    }

    #[test]
    fn rejects_storage_dead_twice() {
        let fx = fx();
        let mir = simple(vec![live(2), dead(2), dead(2)], TerminatorKind::Return);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb0[2]: storage_dead of _2, which is already dead"]
        );
    }

    #[test]
    fn rejects_storage_live_twice() {
        let fx = fx();
        let mir = simple(vec![live(2), live(2), dead(2)], TerminatorKind::Return);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb0[1]: storage_live of _2, which is already live"]
        );
    }

    #[test]
    fn rejects_paths_that_join_with_different_live_locals() {
        let fx = fx();
        let mir = blocks(vec![
            block(
                vec![],
                TerminatorKind::Switch {
                    discr: copy(3),
                    targets: vec![(Const::Bool(false), BlockId(2))],
                    otherwise: BlockId(1),
                },
            ),
            block(vec![live(2)], TerminatorKind::Goto(BlockId(3))),
            block(vec![], TerminatorKind::Goto(BlockId(3))),
            block(vec![], TerminatorKind::Return),
        ]);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb1[term]: bb3 is entered with _2 live here but not on another path"]
        );
    }

    #[test]
    fn rejects_a_local_live_at_return() {
        let fx = fx();
        let mir = simple(vec![live(2)], TerminatorKind::Return);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb0[term]: return while _2 is live"]
        );
    }

    #[test]
    fn rejects_storage_dead_of_a_local_ended_in_an_earlier_block() {
        let fx = fx();
        let mir = blocks(vec![
            block(vec![live(2), dead(2)], TerminatorKind::Goto(BlockId(1))),
            block(vec![], TerminatorKind::Goto(BlockId(2))),
            block(vec![dead(2), live(2), dead(2)], TerminatorKind::Return),
        ]);
        assert_eq!(
            errors(&fx, &mir, true),
            ["fn F bb2[0]: storage_dead of _2, which is already dead"]
        );
    }

    #[test]
    fn storage_problems_come_in_the_order_of_the_blocks() {
        // The search reaches bb2 before bb1; the report keeps bb1 first.
        let fx = fx();
        let mir = blocks(vec![
            block(
                vec![],
                TerminatorKind::Switch {
                    discr: copy(3),
                    targets: vec![(Const::Bool(false), BlockId(2))],
                    otherwise: BlockId(1),
                },
            ),
            block(vec![dead(2)], TerminatorKind::Return),
            block(vec![live(2)], TerminatorKind::Return),
        ]);
        assert_eq!(
            errors(&fx, &mir, true),
            [
                "fn F bb1[0]: storage_dead of _2, which is not live",
                "fn F bb2[term]: return while _2 is live",
            ]
        );
    }

    #[test]
    fn a_block_state_holds_only_the_live_locals() {
        // A chain of blocks, each with a temporary of its own that lives
        // inside it, under one local that lives throughout: every entry
        // state holds that one local, however many locals the body has.
        let fx = fx();
        let n = 2000u32;
        let mut mir = simple(vec![], TerminatorKind::Return);
        let body = &mut mir.bodies[0];
        body.locals
            .extend((0..n).map(|_| val(Ty::Primitive(Primitive::Int))));
        let mut chain = vec![block(vec![live(2)], TerminatorKind::Goto(BlockId(1)))];
        for k in 0..n {
            let temp = 7 + k;
            chain.push(block(
                vec![live(temp), dead(temp)],
                TerminatorKind::Goto(BlockId(k + 2)),
            ));
        }
        chain.push(block(vec![dead(2)], TerminatorKind::Return));
        body.blocks = chain;
        let checker = Checker {
            db: &fx.db,
            mir: &mir,
            body: &mir.bodies[0],
            std_loaded: true,
            errors: &mut Vec::new(),
        };
        let mut problems = Vec::new();
        let entries = checker.storage_entries(&mut problems);
        assert_eq!(problems, []);
        assert_eq!(entries.len(), n as usize + 2);
        assert_eq!(entries[0].as_ref().map(|path| path.live.len()), Some(0));
        for entry in &entries[1..] {
            assert_eq!(
                entry.as_ref().map(|path| path.live.as_slice()),
                Some(&[LocalId(2)][..])
            );
        }
    }

    #[test]
    fn a_marker_on_a_parameter_or_a_missing_local_is_reported_once() {
        let fx = fx();
        let mir = simple(vec![live(1), live(99)], TerminatorKind::Return);
        assert_eq!(
            errors(&fx, &mir, true),
            [
                "fn F bb0[0]: storage statement names _1, which is the return slot or a parameter",
                "fn F bb0[1]: no local _99",
            ]
        );
    }

    #[test]
    fn accepts_paired_markers_on_every_path() {
        let fx = fx();
        // A loop that makes `_2` live and ends it each time round, and a
        // join where both paths end it; a block nothing reaches is not
        // checked.
        let mir = blocks(vec![
            block(vec![], TerminatorKind::Goto(BlockId(1))),
            block(
                vec![live(2)],
                TerminatorKind::Switch {
                    discr: copy(3),
                    targets: vec![(Const::Bool(false), BlockId(3))],
                    otherwise: BlockId(2),
                },
            ),
            block(vec![dead(2)], TerminatorKind::Goto(BlockId(1))),
            block(vec![dead(2)], TerminatorKind::Return),
            block(vec![dead(2), dead(2)], TerminatorKind::Return),
        ]);
        assert_eq!(validate(&fx.db, &mir, true), Ok(()));
    }
}
