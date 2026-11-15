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
pub(crate) mod interp;
pub(crate) mod scan;

use mora_hir::{
    BodyOwner, BodyResolution, ExprId, ItemId, ItemKind, LocalId, Resolved, Target, ValueKind,
    body, body_owners, body_source_map, infer, instance_fields, item_tree, owner_name,
};
use mora_span::{Db, Project, SourceFile, TextRange, TextSize};

use self::graph::const_graph;
use self::interp::{Failure, Stop};
use self::scan::const_scan;
use crate::lower::consts::default_mir;
use crate::lower::expr::constant;
use crate::lower::later::{Later, evaluation_later, exprs_later};
use crate::{AggregateKind, Const, mir};

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
    const_target(db, &resolution.paths.get(&expr)?.target)
}

/// The constant that a path resolved to `target` reads, with its project,
/// as [`const_read`] gives it.
pub(crate) fn const_target(db: &dyn Db, target: &Target) -> Option<(Project, ConstKey)> {
    let (item, owner) = match target {
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

/// What evaluating a [`ConstKey`] gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Evaluated {
    /// Its value.
    Value(ConstValue),
    /// No value: the error, or `None` when a key it reads has no value,
    /// whose own evaluation reports why.
    Failed(Option<ConstError>),
    /// No value yet: its code, or the code of a key it reads, is lowered
    /// by a later milestone.
    Later(Later),
}

/// Why a key has no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstError {
    /// The key.
    pub(crate) key: ConstKey,
    /// Where in the file of the key.
    pub(crate) range: TextRange,
    /// What went wrong.
    pub(crate) kind: ErrorKind,
}

/// What went wrong in the evaluation of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    /// An operation that §14.6 never allows in a constant, and what it is.
    Unsupported(&'static str),
    /// An operation that failed.
    Failed(Failure),
    /// A cycle of reads, as [`graph::Cycle::members`] orders it; the key is
    /// its first member.
    Cycle(Vec<ConstKey>),
}

/// The value of `key`, a key of `project`, each step in order: a key in a
/// cycle of reads fails, with the error on the cycle's first member; then
/// one with an operation §14.6 never allows; a key whose code a later
/// milestone lowers, or that reads one that waits, waits for the latest of
/// those milestones; a key that reads one without a value fails without an
/// error of its own; and any other key runs its MIR in the interpreter.
///
/// The milestones are those of [`evaluation_later`], which leaves the reads
/// to this step, so a key never asks for its own value. No body that a
/// later milestone lowers is ever lowered or run. What runs
/// is one expression, without a block, a `match` or a lambda, so the
/// interpreter never sees a loop.
#[salsa::tracked(returns(ref))]
pub(crate) fn const_value(db: &dyn Db, project: Project, key: ConstKey) -> Evaluated {
    if let Some(cycle) = const_graph(db, project).cycle_of(key) {
        if cycle.members.first() != Some(&key) {
            return Evaluated::Failed(None);
        }
        let kind = ErrorKind::Cycle(cycle.members.clone());
        return failed(key, cycle.read, kind);
    }
    let scan = const_scan(db, project, key);
    if let Some((range, what)) = scan.unsupported {
        return failed(key, range, ErrorKind::Unsupported(what));
    }
    let file = key.file();
    let owner = key.owner(db);
    let later = match key {
        ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
            evaluation_later(db, project, file, owner)
        }
        ConstKey::Default { .. } => exprs_later(db, project, file, owner, &scan.exprs),
    };
    // The key waits for the latest of its own milestone and those of its
    // reads, its own first on a tie, then the reads in order; a constant it
    // reads waits as `a constant`, an initializer for what it waits for.
    let waits_itself = later.is_some();
    let mut waits = later;
    let mut read_failed = false;
    for &(read_project, read, _) in &scan.reads {
        match const_value(db, read_project, read) {
            Evaluated::Value(_) => {}
            Evaluated::Failed(_) => read_failed = true,
            Evaluated::Later(later) => {
                if waits
                    .as_ref()
                    .is_none_or(|latest| later.milestone > latest.milestone)
                {
                    waits = Some(match read {
                        ConstKey::Constant { .. } => Later {
                            what: "a constant".to_owned(),
                            milestone: later.milestone,
                        },
                        ConstKey::Initializer { .. } | ConstKey::Default { .. } => later.clone(),
                    });
                }
            }
        }
    }
    // A read without a value stops a key whose own code this milestone
    // lowers; a key whose own code waits waits, whatever its reads give.
    if read_failed && !waits_itself {
        return Evaluated::Failed(None);
    }
    if let Some(later) = waits {
        return Evaluated::Later(later);
    }
    let body = match key {
        ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
            &mir(db, project, file, owner).bodies[0]
        }
        ConstKey::Default { default, .. } => {
            &default_mir(db, project, file, owner, default).bodies[0]
        }
    };
    // Every initializer the body calls is a read of its scan, which has a
    // value by now.
    let without_value = || -> ! {
        let name = key.name(db, project);
        panic!("internal error evaluating {name}: a field initializer without a value");
    };
    let mut initializer = |item: ItemId, field| {
        let read = ConstKey::Initializer { item, field };
        match const_value(db, item.project, read) {
            Evaluated::Value(value) => Some(value.clone()),
            Evaluated::Failed(_) | Evaluated::Later(_) => without_value(),
        }
    };
    match interp::run(db, body, &mut initializer) {
        Ok(value) => Evaluated::Value(value),
        Err(Stop::Failed(failure)) => failed(key, failure.range, ErrorKind::Failed(failure)),
        Err(Stop::Dependency) => without_value(),
    }
}

/// The error `kind` of `key` at `range`.
fn failed(key: ConstKey, range: TextRange, kind: ErrorKind) -> Evaluated {
    Evaluated::Failed(Some(ConstError { key, range, kind }))
}

#[cfg(test)]
mod tests;
