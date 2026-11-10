//! Local functions (§7.10). A local function is lowered as a body of its
//! own, which takes what it uses of the bodies around it as extra
//! parameters, as [`mora_hir::local_functions`] gives them: `this` first,
//! as a borrow, then its own parameters, then a borrow of each local it
//! takes. A call passes the arguments and those borrows in that order.
//!
//! A local function's body is named after the body it is declared in, a
//! dot and its own name, as `App.F.G`. Local functions of one name in the
//! same body, in blocks side by side, are told apart by a suffix after the
//! first: `App.F.G#2` for the second in the order of the text.

use mora_hir::{Captured, LocalFunction, ParamMode, Stmt, Ty, local_functions};

use super::{BodyShape, Owner};
use crate::types::param_ty;

/// A local function of an owner.
pub(crate) struct LocalFn<'a> {
    /// The local its name declares.
    pub(super) local: mora_hir::LocalId,
    /// Its declaration.
    pub(super) def: &'a LocalFunction,
    /// The name of its body.
    name: String,
    /// What it takes from the bodies around it, `this` first, each with the
    /// mode of its borrow.
    pub(super) captures: &'a [(Captured, ParamMode)],
}

impl<'a> Owner<'a> {
    /// The local functions of the body, in the order of the text.
    pub(super) fn local_functions(&self, owner: mora_hir::BodyOwner) -> Vec<LocalFn<'a>> {
        let hir = self.hir;
        let found = local_functions(self.db, self.project, self.file, owner);
        let mut functions: Vec<LocalFn<'a>> = Vec::with_capacity(found.len());
        // The name each function would have without a suffix.
        let mut bases: Vec<String> = Vec::with_capacity(found.len());
        for function in found {
            let Stmt::Function(def) = &hir.stmts[function.stmt] else {
                panic!(
                    "internal error lowering {}: a local function that is not one",
                    self.name
                );
            };
            let prefix = function
                .outer
                .map_or(&self.name, |outer| &functions[outer].name);
            let base = format!("{prefix}.{}", hir.locals[def.local].name);
            let earlier = bases.iter().filter(|&earlier| *earlier == base).count();
            let name = match earlier {
                0 => base.clone(),
                _ => format!("{base}#{}", earlier + 1),
            };
            bases.push(base);
            functions.push(LocalFn {
                local: def.local,
                def,
                name,
                captures: &function.captures,
            });
        }
        functions
    }

    /// The shape of the body of the local function `function`: `this`, its
    /// own parameters, then the locals it takes.
    pub(super) fn function_shape(&self, function: &LocalFn<'_>) -> BodyShape {
        let def = function.def;
        let at = self.map.local(def.local);
        let Some(root) = def.body else {
            self.ice_at(&function.name, at, "a local function without a body");
        };
        if def.is_async {
            self.ice_at(&function.name, at, "an async local function is not lowered");
        }
        let mut this = None;
        let mut captures = Vec::new();
        for &(captured, mode) in function.captures {
            match captured {
                Captured::This => match &self.this_ty {
                    Some(ty) => this = Some(param_ty(mode, ty.clone())),
                    None => self.ice_at(&function.name, at, "`this` outside a method"),
                },
                Captured::Local(local) => captures.push((local, mode)),
            }
        }
        let params = def
            .params
            .iter()
            .copied()
            .zip(def.modes.iter().copied())
            .chain(captures)
            .map(|(local, mode)| (local, param_ty(mode, self.local_ty(&function.name, local))))
            .collect();
        BodyShape {
            name: function.name.clone(),
            ret: self.resolution.return_type(def.ret),
            this,
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
