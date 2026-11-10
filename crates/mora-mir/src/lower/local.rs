//! Local functions (§7.10). A local function is not a value and cannot
//! outlive the body it is declared in, so it is lowered as a body of its
//! own that takes what it uses of the bodies around it as extra
//! parameters: an `in` borrow of each local it reads, a `ref` borrow of
//! each it assigns. A call passes those borrows after the arguments.
//!
//! An expression, a local or a local function is inside a local function
//! when its text is.

use std::collections::{BTreeMap, HashSet};

use mora_hir::{
    ArgMode, Expr, ExprId, LocalFunction, LocalKind, ParamMode, Signature, Stmt, Ty, UnaryOp,
    arena_index, signature,
};
use mora_span::TextRange;

use super::{BodyShape, Owner};
use crate::{BorrowMode, LocalTy};

/// A local function of an owner.
pub(crate) struct LocalFn<'a> {
    /// The local its name declares.
    pub(super) local: mora_hir::LocalId,
    /// Its declaration.
    pub(super) def: &'a LocalFunction,
    /// Its name: the name of the body it is declared in, a dot, and its own.
    name: String,
    /// The locals of the bodies around it that it uses, in the order of
    /// their declarations, each with the borrow it takes of it.
    pub(super) captures: Vec<(mora_hir::LocalId, BorrowMode)>,
}

/// What a local function uses of the bodies around it.
struct Uses {
    /// Where its text is.
    range: TextRange,
    /// The locals it uses, by their position, each with the borrow it
    /// takes of it.
    captures: BTreeMap<usize, (mora_hir::LocalId, BorrowMode)>,
    /// The local functions it uses that are not inside it.
    functions: Vec<mora_hir::LocalId>,
}

impl<'a> Owner<'a> {
    /// The local functions of the body, in the order of the text, with
    /// what each uses of the bodies around it.
    pub(super) fn local_functions(&self) -> Vec<LocalFn<'a>> {
        let hir = self.hir;
        let mut found: Vec<(TextRange, &'a LocalFunction)> = hir
            .stmts
            .iter()
            .filter_map(|(id, stmt)| match stmt {
                Stmt::Function(function) => Some((self.stmt_range(id), function)),
                _ => None,
            })
            .collect();
        found.sort_by_key(|(range, _)| range.start());
        let mut uses: Vec<Uses> = found.iter().map(|&(range, _)| self.uses(range)).collect();
        // What a local function calls takes what it uses too, unless it is
        // declared in the caller. A local function is declared before the
        // code that calls it, so it comes first in the text, and one pass
        // in that order is enough.
        let position = |local| found.iter().position(|(_, def)| def.local == local);
        for caller in 0..uses.len() {
            for callee in uses[caller].functions.clone() {
                let Some(callee) = position(callee) else {
                    continue;
                };
                let inherited: Vec<_> = uses[callee].captures.values().copied().collect();
                for (local, mode) in inherited {
                    if !self.declared_in(uses[caller].range, local) {
                        capture(&mut uses[caller].captures, local, mode);
                    }
                }
            }
        }
        let mut functions: Vec<LocalFn<'a>> = Vec::with_capacity(found.len());
        for (index, ((range, def), uses)) in found.iter().zip(uses).enumerate() {
            let outer = found[..index]
                .iter()
                .rposition(|(outer, _)| outer.contains_range(*range));
            let prefix = outer.map_or(&self.name, |outer| &functions[outer].name);
            let name = format!("{prefix}.{}", hir.locals[def.local].name);
            functions.push(LocalFn {
                local: def.local,
                def,
                name,
                captures: uses.captures.into_values().collect(),
            });
        }
        functions
    }

    /// What the local function whose text is at `range` uses itself of the
    /// bodies around it.
    fn uses(&self, range: TextRange) -> Uses {
        let hir = self.hir;
        let inside = |expr: ExprId| range.contains_range(self.map.expr(expr));
        let mut read = Vec::new();
        let mut assigned = HashSet::new();
        let mut functions = Vec::new();
        for (id, expr) in hir.exprs.iter().filter(|&(id, _)| inside(id)) {
            match expr {
                Expr::Local(local) if !self.declared_in(range, *local) => {
                    if hir.locals[*local].kind == LocalKind::Function {
                        functions.push(*local);
                    } else {
                        read.push(*local);
                    }
                }
                Expr::Assign { lhs, .. } => self.roots(*lhs, &mut assigned),
                Expr::Unary {
                    op: UnaryOp::PreIncrement | UnaryOp::PreDecrement,
                    expr: target,
                }
                | Expr::Postfix { expr: target, .. } => self.roots(*target, &mut assigned),
                Expr::Call { callee, args } => {
                    for arg in args.iter().filter(|arg| arg.mode == Some(ArgMode::Ref)) {
                        self.roots(arg.expr, &mut assigned);
                    }
                    if let Expr::Member { receiver, .. } = &hir.exprs[*callee]
                        && self.takes_ref_receiver(id)
                    {
                        self.roots(*receiver, &mut assigned);
                    }
                }
                _ => {}
            }
        }
        let mut captures = BTreeMap::new();
        for local in read {
            let mode = if assigned.contains(&local) {
                BorrowMode::Ref
            } else {
                BorrowMode::In
            };
            capture(&mut captures, local, mode);
        }
        Uses {
            range,
            captures,
            functions,
        }
    }

    /// Whether the call `call` calls a `mut` method.
    fn takes_ref_receiver(&self, call: ExprId) -> bool {
        let Some(item) = self.inference.callee(call).and_then(mora_hir::Callee::item) else {
            return false;
        };
        matches!(
            signature(self.db, item),
            Signature::Function(function) if function.receiver == Some(ParamMode::Ref)
        )
    }

    /// Adds to `out` the local the place `place` is in: a local, or a
    /// field of one.
    fn roots(&self, place: ExprId, out: &mut HashSet<mora_hir::LocalId>) {
        match &self.hir.exprs[place] {
            Expr::Local(local) => {
                out.insert(*local);
            }
            Expr::Member { receiver, .. } => self.roots(*receiver, out),
            _ => {}
        }
    }

    /// Whether `local` is declared inside the text at `range`.
    fn declared_in(&self, range: TextRange, local: mora_hir::LocalId) -> bool {
        range.contains_range(self.map.local(local))
    }

    /// Where the text of the statement `stmt` is.
    fn stmt_range(&self, stmt: mora_hir::StmtId) -> TextRange {
        match self.map.stmts.get(&stmt) {
            Some(range) => *range,
            None => panic!(
                "internal error lowering {}: a statement without its range",
                self.name
            ),
        }
    }

    /// The shape of the body of the local function `function`: its own
    /// parameters, `in` (§10.3), then what it uses of the bodies around it.
    pub(super) fn function_shape(&self, function: &LocalFn<'_>) -> BodyShape {
        let def = function.def;
        let at = self.map.local(def.local);
        let Some(root) = def.body else {
            self.ice_at(&function.name, at, "a local function without a body");
        };
        if def.is_async {
            self.ice_at(&function.name, at, "an async local function is not lowered");
        }
        let params = def
            .params
            .iter()
            .map(|&param| (param, BorrowMode::In))
            .chain(function.captures.iter().copied())
            .map(|(local, mode)| {
                (
                    local,
                    LocalTy::Borrow(mode, self.local_ty(&function.name, local)),
                )
            })
            .collect();
        BodyShape {
            name: function.name.clone(),
            ret: self.resolution.return_type(def.ret),
            this: None,
            params,
            root,
        }
    }

    /// The type of `local`, in the body `body`.
    fn local_ty(&self, body: &str, local: mora_hir::LocalId) -> Ty {
        match self.inference.local(local) {
            Some(ty) => ty.clone(),
            None => self.ice_at(
                body,
                self.map.local(local),
                &format!("the local `{}` has no type", self.hir.locals[local].name),
            ),
        }
    }

    /// The local function `local`, with the position of its body in the
    /// owner's MIR, which [`crate::Callee::Local`] holds.
    pub(super) fn function(&self, local: mora_hir::LocalId) -> Option<(usize, &LocalFn<'a>)> {
        let index = self
            .functions
            .iter()
            .position(|function| function.local == local)?;
        Some((index + 1, &self.functions[index]))
    }
}

/// Records that a local function uses `local` with a borrow of `mode`: a
/// `ref` borrow when any use needs one.
fn capture(
    captures: &mut BTreeMap<usize, (mora_hir::LocalId, BorrowMode)>,
    local: mora_hir::LocalId,
    mode: BorrowMode,
) {
    let (_, recorded) = captures.entry(arena_index(local)).or_insert((local, mode));
    if mode == BorrowMode::Ref {
        *recorded = BorrowMode::Ref;
    }
}
