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
mod report;
pub(crate) mod scan;

use std::collections::{HashMap, HashSet};

pub use self::report::diagnostics;

use mora_hir::{
    BodyOwner, BodyResolution, ExprId, ItemId, LocalId, Resolved, Target, ValueKind, body,
    body_owners, body_source_map, infer, instance_fields, item_tree, owner_name,
};
use mora_span::{Db, Project, SourceFile, TextRange, TextSize};

use self::graph::const_graph;
use self::interp::{Failure, Stop};
use self::scan::const_scan;
use crate::lower::consts::eval_mir;
use crate::lower::expr::constant;
use crate::lower::later::{Later, defaults_later, evaluation_later, exprs_later, omitted_defaults};
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
                    fail_evaluating(name, &format!("no instance field {field}"));
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
            fail_evaluating(
                &format!("a default value in {short}"),
                "no parameter has it",
            );
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
            if !item_tree(db, item.file).item(item.item).is_constant() {
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
    body_owners(db, file)
        .into_iter()
        .filter(|&owner| is_constant(db, file, owner))
        .map(|owner| ConstKey::Constant { file, owner })
        .collect()
}

/// Whether the body of `owner` of `file` is the initializer of a
/// constant, declared in a namespace or in a type.
pub(crate) fn is_constant(db: &dyn Db, file: SourceFile, owner: BodyOwner) -> bool {
    let tree = item_tree(db, file);
    match owner {
        BodyOwner::Item(item) => tree.item(item).is_constant(),
        BodyOwner::Value(item, index) => tree
            .item(item)
            .values
            .get(index as usize)
            .is_some_and(|value| value.kind == ValueKind::Const),
        BodyOwner::TopLevel | BodyOwner::Member(..) | BodyOwner::Extension(..) => false,
    }
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

/// What evaluating every key of `project` gives: the keys of its read
/// graph, and the initializers of other projects that their own read
/// graphs never reach, which a key of `project` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstValues {
    /// What each key gives. Only looked up, never iterated.
    values: HashMap<ConstKey, Evaluated>,
}

impl ConstValues {
    /// What `key` gives, when the pass evaluated it.
    pub(crate) fn get(&self, key: ConstKey) -> Option<&Evaluated> {
        self.values.get(&key)
    }
}

/// Evaluates every key of `project` in one pass, in the order of its read
/// graph, each after every key it needs, so each finds what those give
/// already computed and none is evaluated inside another.
///
/// Its dependencies do not grow with a chain of constants: the read graph,
/// and for each key its scan, what decides whether it waits, and its MIR,
/// in which a read of a constant is an [`Operand::ConstRead`] and not the
/// value; and for each other project it reads, that project's pass. So
/// checking it again after an edit goes one level deep per project, never
/// one per constant.
///
/// [`Operand::ConstRead`]: crate::Operand::ConstRead
#[salsa::tracked(returns(ref))]
pub(crate) fn const_values(db: &dyn Db, project: Project) -> ConstValues {
    let mut pass = Pass {
        db,
        project,
        values: HashMap::new(),
    };
    for &key in const_graph(db, project).order() {
        pass.ensure(project, key);
    }
    ConstValues {
        values: pass.values,
    }
}

/// What evaluating `key`, a key of the read graph of `project`, gives
/// ([`const_values`]).
///
/// # Panics
///
/// When the read graph of `project` does not hold `key`, which only a
/// compiler bug can cause.
pub(crate) fn const_value(db: &dyn Db, project: Project, key: ConstKey) -> &Evaluated {
    match const_values(db, project).get(key) {
        Some(evaluated) => evaluated,
        None => fail_evaluating(&key.name(db, project), "a key outside the read graph"),
    }
}

/// A pass of [`const_values`] in progress.
struct Pass<'a> {
    /// The database.
    db: &'a dyn Db,
    /// The project whose keys it evaluates.
    project: Project,
    /// What each key evaluated so far gives.
    values: HashMap<ConstKey, Evaluated>,
}

impl Pass<'_> {
    /// What `key`, a key of `project`, gives, when it is known: evaluated
    /// by this pass, or by the pass of its own project.
    fn known(&self, project: Project, key: ConstKey) -> Option<&Evaluated> {
        if let Some(evaluated) = self.values.get(&key) {
            return Some(evaluated);
        }
        if project == self.project {
            return None;
        }
        const_values(self.db, project).get(key)
    }

    /// What `key`, a key of `project` that [`Pass::ensure`] has evaluated
    /// or found, gives.
    fn value(&self, project: Project, key: ConstKey) -> &Evaluated {
        match self.known(project, key) {
            Some(evaluated) => evaluated,
            None => fail_evaluating(&key.name(self.db, project), "a key needed before it is"),
        }
    }

    /// Evaluates `key`, a key of `project`, unless it is known, after each
    /// key it needs ([`Pass::needs`]) that is not, each of those after the
    /// keys it needs in turn. The walk keeps its own stack instead of
    /// recursing.
    ///
    /// # Panics
    ///
    /// When a key needs itself without a cycle of the read graph: a call
    /// that leaves out a default value reading the constant that calls,
    /// or initializers of another project that read each other. Only an
    /// input the read graph never sees leads there.
    fn ensure(&mut self, project: Project, key: ConstKey) {
        if self.known(project, key).is_some() {
            return;
        }
        let mut open = HashSet::from([key]);
        // Each frame is a key, the keys it needs and the position of the
        // next one.
        let mut walk = vec![(project, key, self.needs(project, key), 0)];
        while let Some(frame) = walk.last_mut() {
            if let Some(&(project, key)) = frame.2.get(frame.3) {
                frame.3 += 1;
                if self.known(project, key).is_none() {
                    if !open.insert(key) {
                        let name = key.name(self.db, project);
                        fail_evaluating(&name, "it needs its own value outside a cycle of reads");
                    }
                    walk.push((project, key, self.needs(project, key), 0));
                }
                continue;
            }
            let Some((project, key, _, _)) = walk.pop() else {
                break;
            };
            open.remove(&key);
            let evaluated = self.evaluate(project, key);
            self.values.insert(key, evaluated);
        }
    }

    /// The keys whose results [`Pass::evaluate`] reads for `key`, a key of
    /// `project`: those its scan reads, then, for a constant or an
    /// initializer, the default values that its calls leave out. None for a
    /// key that fails before its reads: one in a cycle, or with an
    /// operation §14.6 never allows.
    fn needs(&self, project: Project, key: ConstKey) -> Vec<(Project, ConstKey)> {
        let db = self.db;
        let scan = const_scan(db, project, key);
        if const_graph(db, project).cycle_of(key).is_some() || scan.unsupported.is_some() {
            return Vec::new();
        }
        let reads = scan.reads.iter().map(|&(project, read, _)| (project, read));
        match key {
            ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
                let defaults = omitted_defaults(db, project, key.file(), key.owner(db));
                reads.chain(defaults).collect()
            }
            ConstKey::Default { .. } => reads.collect(),
        }
    }

    /// What `key`, a key of `project` whose needs are known, gives, each
    /// step in order: a key in a cycle of reads fails, with the error on
    /// the cycle's first member; then one with an operation §14.6 never
    /// allows; a key whose code a later milestone lowers, or that reads
    /// one that waits, waits for the latest of those milestones; a key
    /// that reads one without a value fails without an error of its own;
    /// and any other key runs its MIR ([`eval_mir`]) in the interpreter.
    ///
    /// The milestones are those of [`evaluation_later`], which leaves the
    /// reads to this step. No body that a later milestone lowers is ever
    /// lowered or run. What runs is one expression, without a block, a
    /// `match` or a lambda, so the interpreter never sees a loop.
    fn evaluate(&self, project: Project, key: ConstKey) -> Evaluated {
        let db = self.db;
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
                let defaults = omitted_defaults(db, project, file, owner);
                let defaults = defaults_later(&defaults, |project, key| self.value(project, key));
                evaluation_later(db, project, file, owner, defaults)
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
            match self.value(read_project, read) {
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
                            ConstKey::Initializer { .. } | ConstKey::Default { .. } => {
                                later.clone()
                            }
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
        // Every key the body reads, and every initializer it calls, is a
        // read of its scan, which has a value by now.
        let values = |project, key| match self.value(project, key) {
            Evaluated::Value(value) => Some(value.clone()),
            Evaluated::Failed(_) | Evaluated::Later(_) => None,
        };
        match interp::run(db, &eval_mir(db, project, key).bodies[0], &values) {
            Ok(value) => Evaluated::Value(value),
            Err(Stop::Failed(failure)) => failed(key, failure.range, ErrorKind::Failed(failure)),
            Err(Stop::Dependency) => {
                let name = key.name(db, project);
                fail_evaluating(&name, "a field initializer without a value");
            }
        }
    }
}

/// The error `kind` of `key` at `range`.
fn failed(key: ConstKey, range: TextRange, kind: ErrorKind) -> Evaluated {
    Evaluated::Failed(Some(ConstError { key, range, kind }))
}

/// Panics with an internal error of the evaluation of `name`: what only a
/// compiler bug can cause.
pub(crate) fn fail_evaluating(name: &str, what: &str) -> ! {
    panic!("internal error evaluating {name}: {what}")
}

#[cfg(test)]
mod tests;
