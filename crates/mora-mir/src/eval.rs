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
use std::fmt::Display;

pub use self::report::diagnostics;

use mora_hir::{
    BodyOwner, BodyResolution, ExprId, ItemId, LocalId, Resolved, Target, ValueKind, body,
    body_owners, body_source_map, infer, instance_fields, item_tree, owner_name,
};
use mora_span::{Db, Project, SourceFile, TextRange, TextSize};

use self::graph::const_graph;
use self::interp::Failure;
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
                let declared = instance_fields(db, item).nth(field as usize);
                let name = || item_tree(db, item.file).item(item.item).name.clone();
                let what = format_args!("no instance field {field}");
                let (index, _) = expect_evaluating(declared, name, what);
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
        let name = || format!("a default value in {short}");
        let param = expect_evaluating(default_param(hir, default), name, "no parameter has it");
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
    /// The initializers of other projects that this pass evaluated itself,
    /// in the order it did, whose errors it reports.
    extras: Vec<ConstKey>,
}

impl ConstValues {
    /// What `key` gives, when the pass evaluated it or took it from a
    /// dependency.
    pub(crate) fn get(&self, key: ConstKey) -> Option<&Evaluated> {
        self.values.get(&key)
    }

    /// The initializers of other projects that the pass evaluated itself,
    /// in the order it did.
    pub(crate) fn extras(&self) -> &[ConstKey] {
        &self.extras
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
/// An initializer of another project that the read graph of that project
/// never reaches is evaluated by the pass of a project that reads it, and
/// that pass reports its error: the pass of the first project that
/// `project` depends on, directly or not, that has it, or else this one. So
/// a project that depends on another that runs it reports nothing more.
///
/// [`Operand::ConstRead`]: crate::Operand::ConstRead
#[salsa::tracked(returns(ref))]
pub(crate) fn const_values(db: &dyn Db, project: Project) -> ConstValues {
    let mut pass = Pass {
        db,
        project,
        values: HashMap::new(),
        extras: Vec::new(),
    };
    for &key in const_graph(db, project).order() {
        pass.ensure(project, key);
    }
    ConstValues {
        values: pass.values,
        extras: pass.extras,
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
    let name = || key.name(db, project);
    expect_evaluating(
        const_values(db, project).get(key),
        name,
        "a key outside the read graph",
    )
}

/// The projects that `project` depends on, directly or through others,
/// each once: depth first, each project's dependencies in their order
/// after it.
fn transitive_dependencies(db: &dyn Db, project: Project) -> Vec<Project> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut walk: Vec<Project> = project
        .dependencies(db)
        .iter()
        .rev()
        .map(|dependency| dependency.project)
        .collect();
    while let Some(next) = walk.pop() {
        if !seen.insert(next) {
            continue;
        }
        found.push(next);
        walk.extend(
            next.dependencies(db)
                .iter()
                .rev()
                .map(|dependency| dependency.project),
        );
    }
    found
}

/// A pass of [`const_values`] in progress.
struct Pass<'a> {
    /// The database.
    db: &'a dyn Db,
    /// The project whose keys it evaluates.
    project: Project,
    /// What each key evaluated so far gives, and what the keys of other
    /// projects it took from a dependency give.
    values: HashMap<ConstKey, Evaluated>,
    /// The keys of other projects it evaluated itself, in order.
    extras: Vec<ConstKey>,
}

/// A key that the evaluation of another needs.
#[derive(Clone, Copy)]
struct Need {
    /// The project of the key.
    project: Project,
    /// The key.
    key: ConstKey,
    /// The range of the read that needs it; `None` for a default value
    /// that a call leaves out, which decides only which milestone the
    /// key that calls waits for, since such a call is always inside code
    /// that waits.
    read: Option<TextRange>,
}

/// A frame of the walk of [`Pass::ensure`]: a key, the keys it needs and
/// the position of the next one.
struct Frame {
    /// The project of the key.
    project: Project,
    /// The key.
    key: ConstKey,
    /// What it needs.
    needs: Vec<Need>,
    /// The position in `needs` of the next one to look at.
    next: usize,
}

impl Pass<'_> {
    /// What `key`, a key of `project`, gives, when it is known: evaluated
    /// or taken by this pass, or evaluated by the pass of its own project.
    fn known(&self, project: Project, key: ConstKey) -> Option<&Evaluated> {
        if let Some(evaluated) = self.values.get(&key) {
            return Some(evaluated);
        }
        if project == self.project {
            return None;
        }
        const_values(self.db, project).get(key)
    }

    /// Whether `key`, a key of `project`, is known, after taking what it
    /// gives from the pass of the first project this one depends on,
    /// directly or not, that has it ([`transitive_dependencies`]), when
    /// neither this pass nor that of its own project does.
    fn resolve(&mut self, project: Project, key: ConstKey) -> bool {
        if self.known(project, key).is_some() {
            return true;
        }
        if project == self.project {
            return false;
        }
        let db = self.db;
        let taken = transitive_dependencies(db, self.project)
            .into_iter()
            .find_map(|dependency| const_values(db, dependency).get(key));
        match taken {
            Some(evaluated) => {
                self.values.insert(key, evaluated.clone());
                true
            }
            None => false,
        }
    }

    /// What `key`, a key of `project` that [`Pass::ensure`] has evaluated
    /// or found, gives.
    fn value(&self, project: Project, key: ConstKey) -> &Evaluated {
        let name = || key.name(self.db, project);
        expect_evaluating(self.known(project, key), name, "a key needed before it is")
    }

    /// Evaluates `key`, a key of `project`, after each key it needs
    /// ([`Pass::needs`]) that is not known, each of those after the keys it
    /// needs in turn. The walk keeps its own stack instead of recursing.
    /// The read graph lists each key after every key it reads, but not
    /// after the default values its calls leave out: a walk may evaluate
    /// such a default, and the keys it reads, before their turn comes, and
    /// each turn evaluates its key again, to the same result.
    ///
    /// A key the walk reaches again while it is still open closes a cycle.
    /// A cycle that goes through a default value a call leaves out is cut
    /// there: the key that calls is evaluated without it, and waits for
    /// the milestone of its own code. Any other cycle is one of
    /// initializers of another project that the read graphs never see, and
    /// fails as [`Pass::close_cycle`] says.
    fn ensure(&mut self, project: Project, key: ConstKey) {
        let needs = self.needs(project, key);
        let mut walk = vec![Frame {
            project,
            key,
            needs,
            next: 0,
        }];
        while let Some(mut frame) = walk.pop() {
            let Some(&need) = frame.needs.get(frame.next) else {
                let evaluated = self.evaluate(frame.project, frame.key, &frame.needs);
                if frame.project != self.project {
                    self.extras.push(frame.key);
                }
                self.values.insert(frame.key, evaluated);
                continue;
            };
            frame.next += 1;
            walk.push(frame);
            if self.resolve(need.project, need.key) {
                continue;
            }
            match walk.iter().position(|frame| frame.key == need.key) {
                None => {
                    let needs = self.needs(need.project, need.key);
                    walk.push(Frame {
                        project: need.project,
                        key: need.key,
                        needs,
                        next: 0,
                    });
                }
                Some(start) => self.close_cycle(&mut walk, start),
            }
        }
    }

    /// Closes the cycle from the frame `start` of `walk` to its last
    /// frame, whose last need leads back to `start`. When a default value
    /// is on it, the walk returns to the last frame that needs one, which
    /// goes on without it. Otherwise its members fail: the first of them by
    /// `sort_key`, with the error of the cycle in the order of its reads
    /// from there; the others without one.
    fn close_cycle(&mut self, walk: &mut Vec<Frame>, start: usize) {
        // The edge out of each frame of the cycle, in order: the need it
        // looked at last, which leads to the next frame, and from the last
        // frame back to `start`.
        let edges: Vec<Need> = walk[start..]
            .iter()
            .map(|frame| frame.needs[frame.next - 1])
            .collect();
        if let Some(cut) = edges.iter().rposition(|edge| edge.read.is_none()) {
            walk.truncate(start + cut + 1);
            return;
        }
        let db = self.db;
        let members: Vec<(Project, ConstKey)> = walk[start..]
            .iter()
            .map(|frame| (frame.project, frame.key))
            .collect();
        let first = (0..members.len())
            .min_by_key(|&at| members[at].1.sort_key(db))
            .unwrap_or_default();
        let order: Vec<ConstKey> = (0..members.len())
            .map(|at| members[(first + at) % members.len()].1)
            .collect();
        let read = edges[first].read.unwrap_or_default();
        for (at, &(project, key)) in members.iter().enumerate() {
            let evaluated = if at == first {
                failed(key, read, ErrorKind::Cycle(order.clone()))
            } else {
                Evaluated::Failed(None)
            };
            if project != self.project {
                self.extras.push(key);
            }
            self.values.insert(key, evaluated);
        }
        walk.truncate(start);
    }

    /// The keys whose results [`Pass::evaluate`] reads for `key`, a key of
    /// `project`: those its scan reads, then, for a constant or an
    /// initializer, the default values that its calls leave out. None for a
    /// key that fails before its reads: one in a cycle, or with an
    /// operation §14.6 never allows.
    fn needs(&self, project: Project, key: ConstKey) -> Vec<Need> {
        let db = self.db;
        let scan = const_scan(db, project, key);
        if const_graph(db, project).cycle_of(key).is_some() || scan.unsupported.is_some() {
            return Vec::new();
        }
        let reads = scan.reads.iter().map(|&(project, key, range)| Need {
            project,
            key,
            read: Some(range),
        });
        match key {
            ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
                let defaults = omitted_defaults(db, project, key.file(), key.owner(db));
                let defaults = defaults.into_iter().map(|(project, key)| Need {
                    project,
                    key,
                    read: None,
                });
                reads.chain(defaults).collect()
            }
            ConstKey::Default { .. } => reads.collect(),
        }
    }

    /// What `key`, a key of `project` whose needs `needs` ([`Pass::needs`])
    /// are known, gives, each step in order: a key in a cycle of reads
    /// fails, with the error on the cycle's first member; then one with an
    /// operation §14.6 never allows; a key whose code a later milestone
    /// lowers, or that reads one that waits, waits for the latest of those
    /// milestones; a key that reads one without a value fails without an
    /// error of its own; and any other key runs its MIR ([`eval_mir`]) in
    /// the interpreter.
    ///
    /// The milestones are those of [`evaluation_later`], which leaves the
    /// reads to this step. No body that a later milestone lowers is ever
    /// lowered or run. What runs is one expression, without a block, a
    /// `match` or a lambda, so the interpreter never sees a loop.
    fn evaluate(&self, project: Project, key: ConstKey, needs: &[Need]) -> Evaluated {
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
                // The defaults are the needs without a read. One that a
                // cycle cut is not known, and decides nothing.
                let defaults = needs
                    .iter()
                    .filter(|need| need.read.is_none())
                    .map(|need| (need.project, need.key));
                let defaults = defaults_later(defaults, |project, key| self.known(project, key));
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
        // The values of the reads, which the run takes. Only looked up.
        let mut read_values = HashMap::new();
        for &(read_project, read, _) in &scan.reads {
            match self.value(read_project, read) {
                Evaluated::Value(value) => {
                    read_values.insert((read_project, read), value);
                }
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
        let values = |project, key| {
            read_values
                .get(&(project, key))
                .map(|&value| ConstValue::clone(value))
        };
        match interp::run(db, &eval_mir(db, project, key).bodies[0], &values) {
            Ok(value) => Evaluated::Value(value),
            Err(failure) => failed(key, failure.range, ErrorKind::Failed(failure)),
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

/// `value`, or the internal error of [`fail_evaluating`] for the body that
/// `name` names, saying `what`, when it is `None`: a state only a compiler
/// bug can cause. Both are built only then.
pub(crate) fn expect_evaluating<T>(
    value: Option<T>,
    name: impl FnOnce() -> String,
    what: impl Display,
) -> T {
    value.unwrap_or_else(|| fail_evaluating(&name(), &what.to_string()))
}

#[cfg(test)]
mod tests;
