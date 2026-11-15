//! Constant evaluation (§14.6): a MIR interpreter that computes the values
//! of constants at compile time.
//!
//! What evaluation runs is named by a [`ConstKey`]: a constant, the
//! initializer of a field that a memberwise creation leaves out, or a
//! default value of a parameter. [`scan::const_scan`] reads the body of a
//! key before anything runs: the constants and initializers it reads, and
//! the first operation that §14.6 never allows in it.

mod arith;
pub(crate) mod graph;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the evaluation that runs it comes next")
)]
pub(crate) mod interp;
pub(crate) mod scan;

use mora_hir::{
    BodyOwner, BodyResolution, ExprId, ItemId, ItemKind, LocalId, Resolved, Target, ValueKind,
    body, body_owners, body_source_map, infer, instance_fields, item_tree, owner_name,
};
use mora_span::{Db, Project, SourceFile, TextSize};

use crate::lower::expr::constant;
use crate::{AggregateKind, Const};

/// A body that constant evaluation runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ConstKey {
    /// A constant: at namespace level (`BodyOwner::Item`) or a member of a
    /// type (`BodyOwner::Value`, kind `Const`).
    Constant { file: SourceFile, owner: BodyOwner },
    /// The initializer of the instance field at position `field` of the
    /// struct `item` (as `Callee::Initializer`).
    Initializer { item: ItemId, field: u32 },
    /// The default value `default` of a parameter of a function or local
    /// function of `owner`.
    Default {
        file: SourceFile,
        owner: BodyOwner,
        default: ExprId,
    },
}

/// A value that constant evaluation computes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstValue {
    /// A scalar.
    Scalar(Const),
    /// A struct or a tuple, with the value of each field or element in
    /// order.
    Aggregate(AggregateKind, Vec<ConstValue>),
}

impl ConstKey {
    /// The file whose body it runs: the struct's for an initializer.
    pub(crate) fn file(self) -> SourceFile {
        match self {
            ConstKey::Constant { file, .. } | ConstKey::Default { file, .. } => file,
            ConstKey::Initializer { item, .. } => item.file,
        }
    }

    /// The owner of the body it runs: for an initializer, the value
    /// member that declares the field.
    ///
    /// # Panics
    ///
    /// When the struct of an initializer has no instance field at its
    /// position, which only a compiler bug can cause.
    pub(crate) fn owner(self, db: &dyn Db) -> BodyOwner {
        match self {
            ConstKey::Constant { owner, .. } | ConstKey::Default { owner, .. } => owner,
            ConstKey::Initializer { item, field } => {
                let Some((index, _)) = instance_fields(db, item).nth(field as usize) else {
                    let name = &item_tree(db, item.file).item(item.item).name;
                    panic!("internal error evaluating {name}: no instance field {field}");
                };
                BodyOwner::Value(item.item, index)
            }
        }
    }

    /// The expression it evaluates: the root of the body, or the default
    /// value. `None` for a body without one.
    pub(crate) fn root(self, db: &dyn Db) -> Option<ExprId> {
        match self {
            ConstKey::Default { default, .. } => Some(default),
            ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
                body(db, self.file(), self.owner(db)).root
            }
        }
    }

    /// The name diagnostics give it: `A`, `P.K`, `P.X`, the name of the
    /// owner without the leading `<root namespace>.`; for a default value,
    /// `the default value of x in F`.
    ///
    /// # Panics
    ///
    /// When no parameter of a default's owner has it, which only a
    /// compiler bug can cause.
    pub(crate) fn name(self, db: &dyn Db, project: Project) -> String {
        let file = self.file();
        let owner = self.owner(db);
        let full = owner_name(db, project, file, owner);
        let prefix = format!("{}.", project.root_namespace(db));
        let short = full.strip_prefix(&prefix).unwrap_or(&full).to_owned();
        let ConstKey::Default { default, .. } = self else {
            return short;
        };
        let hir = body(db, file, owner);
        let Some(param) = default_param(hir, default) else {
            panic!("internal error evaluating a default value in {short}: no parameter has it");
        };
        format!("the default value of {} in {short}", hir.locals[param].name)
    }

    /// The order keys are reported in: constants first, then initializers,
    /// then defaults; then by the path of the file; then by where the
    /// expression it evaluates starts.
    pub(crate) fn sort_key(self, db: &dyn Db) -> (u8, String, TextSize) {
        let kind = match self {
            ConstKey::Constant { .. } => 0,
            ConstKey::Initializer { .. } => 1,
            ConstKey::Default { .. } => 2,
        };
        let file = self.file();
        let start = self.root(db).map_or(TextSize::new(0), |root| {
            body_source_map(db, file, self.owner(db)).expr(root).start()
        });
        (kind, file.path(db).clone(), start)
    }
}

/// The parameter whose default value is `default`, among those of the
/// body and of its local functions.
fn default_param(hir: &mora_hir::Body, default: ExprId) -> Option<LocalId> {
    all_defaults(hir)
        .find(|&(_, expr)| expr == default)
        .map(|(param, _)| param)
}

/// The default values of the parameters of the body and of its local
/// functions, each with its parameter; those of lambdas are not among them.
fn all_defaults(hir: &mora_hir::Body) -> impl Iterator<Item = (LocalId, ExprId)> + '_ {
    let functions = hir
        .stmts
        .iter()
        .filter_map(|(stmt, _)| hir.local_function(stmt))
        .map(|function| function.defaults.as_slice());
    std::iter::once(hir.defaults.as_slice())
        .chain(functions)
        .flatten()
        .copied()
}

/// The constant the path or member `expr` reads, with its project: a
/// namespace-level `const` item (not `static`), or a value member of kind
/// `Const` named by its bare name or through its type.
pub(crate) fn const_read(
    db: &dyn Db,
    resolution: &BodyResolution,
    expr: ExprId,
) -> Option<(Project, ConstKey)> {
    let (item, owner) = match &resolution.paths.get(&expr)?.target {
        Target::Item(Resolved::Item(item)) => {
            let data = item_tree(db, item.file).item(item.item);
            if data.kind != ItemKind::Field || data.is_static {
                return None;
            }
            (*item, BodyOwner::Item(item.item))
        }
        Target::Value(value) => {
            if value.member(db)?.kind != ValueKind::Const {
                return None;
            }
            let index = u32::try_from(value.index).ok()?;
            (value.ty, BodyOwner::Value(value.ty.item, index))
        }
        Target::Item(_) | Target::Error => return None,
    };
    let key = ConstKey::Constant {
        file: item.file,
        owner,
    };
    Some((item.project, key))
}

/// The constants of `file`, in the order of [`body_owners`].
pub(crate) fn constants(db: &dyn Db, file: SourceFile) -> Vec<ConstKey> {
    let tree = item_tree(db, file);
    body_owners(db, file)
        .into_iter()
        .filter(|&owner| match owner {
            BodyOwner::Item(item) => {
                let data = tree.item(item);
                data.kind == ItemKind::Field && !data.is_static
            }
            BodyOwner::Value(item, index) => tree
                .item(item)
                .values
                .get(index as usize)
                .is_some_and(|value| value.kind == ValueKind::Const),
            BodyOwner::TopLevel | BodyOwner::Member(..) | BodyOwner::Extension(..) => false,
        })
        .map(|owner| ConstKey::Constant { file, owner })
        .collect()
}

/// The default values of `file` that evaluation computes: those of
/// functions, methods and local functions for which [`constant`] gives no
/// value, in the order of the text. The defaults of lambdas are not among
/// them.
pub(crate) fn defaults(db: &dyn Db, project: Project, file: SourceFile) -> Vec<ConstKey> {
    let mut found = Vec::new();
    for owner in body_owners(db, file) {
        let hir = body(db, file, owner);
        let mut owned = all_defaults(hir).peekable();
        if owned.peek().is_none() {
            continue;
        }
        let inference = infer(db, project, file, owner);
        let map = body_source_map(db, file, owner);
        for (_, default) in owned {
            if constant(hir, inference, default).is_none() {
                let key = ConstKey::Default {
                    file,
                    owner,
                    default,
                };
                found.push((map.expr(default).start(), key));
            }
        }
    }
    found.sort_by_key(|&(start, _)| start);
    found.into_iter().map(|(_, key)| key).collect()
}

#[cfg(test)]
mod tests;
