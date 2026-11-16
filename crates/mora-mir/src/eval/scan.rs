//! The scan of the body of a [`ConstKey`] before it runs: the constants and
//! initializers it reads, and the first operation that §14.6 never allows
//! in a constant.

use std::ops::ControlFlow;

use mora_hir::{
    Arg, Body, BodyResolution, BodySourceMap, Creation, Expr, ExprId, Inference, ItemId, UnaryOp,
    ValueKind, body, body_resolution, body_source_map, creation, infer, instance_fields,
};
use mora_span::{Db, Project, TextRange};

use super::{ConstKey, const_read};
use crate::lower::later::ExprRules;

/// What the body of a [`ConstKey`] reads and the first operation in it
/// that constant evaluation never runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstScan {
    /// The first operation in pre-order that §14.6 never allows, and what
    /// it is.
    pub(crate) unsupported: Option<(TextRange, &'static str)>,
    /// The constants and initializers it reads, in pre-order, each with
    /// the range of the read.
    pub(crate) reads: Vec<(Project, ConstKey, TextRange)>,
    /// Every expression it visited.
    pub(crate) exprs: Vec<ExprId>,
}

/// Scans the expression of `key` in pre-order, up to its first operation
/// that §14.6 never allows. A read of a constant, or of the initializer of
/// a field that a memberwise creation leaves out, is recorded and not
/// descended into. Neither is a construct a later milestone lowers
/// ([`ExprRules::construct_later`]), as a constructor call, `with`, `is`,
/// a `match` or a call of a generic function, nor a block: the milestones
/// that lower them decide what they hold.
#[salsa::tracked(returns(ref))]
pub(crate) fn const_scan(db: &dyn Db, project: Project, key: ConstKey) -> ConstScan {
    let file = key.file();
    let owner = key.owner(db);
    let hir = body(db, file, owner);
    let mut scanner = Scanner {
        db,
        hir,
        map: body_source_map(db, file, owner),
        resolution: body_resolution(db, project, file, owner),
        inference: infer(db, project, file, owner),
        rules: ExprRules::new(db, project, file, owner),
        scan: ConstScan {
            unsupported: None,
            reads: Vec::new(),
            exprs: Vec::new(),
        },
    };
    if let Some(root) = key.root(db) {
        // A scan that stops has recorded why.
        let _ = scanner.visit(root);
    }
    scanner.scan
}

/// A scan in progress over one body.
struct Scanner<'a> {
    /// The database.
    db: &'a dyn Db,
    /// The HIR body the key's expression is in.
    hir: &'a Body,
    /// Where the parts of the body are.
    map: &'a BodySourceMap,
    /// What the paths of the body name.
    resolution: &'a BodyResolution,
    /// The types and records of the body.
    inference: &'a Inference,
    /// What decides whether a construct of the body waits.
    rules: ExprRules<'a>,
    /// What the scan has found so far.
    scan: ConstScan,
}

impl Scanner<'_> {
    /// Visits `expr` and what it holds, breaking at the first unsupported
    /// operation.
    fn visit(&mut self, expr: ExprId) -> ControlFlow<()> {
        self.scan.exprs.push(expr);
        if let Some(what) = self.unsupported(expr) {
            self.scan.unsupported = Some((self.map.expr(expr), what));
            return ControlFlow::Break(());
        }
        // A construct a later milestone lowers decides what it holds, so
        // what it holds is neither read nor checked (§14.6).
        if self.rules.construct_later(expr).is_some() {
            return ControlFlow::Continue(());
        }
        let hir = self.hir;
        let node = &hir.exprs[expr];
        if matches!(node, Expr::Path(_) | Expr::Member { .. })
            && let Some((project, key)) = const_read(self.db, self.resolution, expr)
        {
            self.scan.reads.push((project, key, self.map.expr(expr)));
            return ControlFlow::Continue(());
        }
        if let Expr::New { args, inits, .. } = node
            && let Some(item) = self.memberwise(expr, args.as_deref())
        {
            let inits = inits.as_deref().unwrap_or_default();
            for &(_, value) in inits {
                self.visit(value)?;
            }
            self.read_left_out(expr, item, inits);
            return ControlFlow::Continue(());
        }
        for child in children(node) {
            self.visit(child)?;
        }
        ControlFlow::Continue(())
    }

    /// What `expr` is when §14.6 never allows it in a constant.
    fn unsupported(&self, expr: ExprId) -> Option<&'static str> {
        Some(match &self.hir.exprs[expr] {
            // A call of the library has no callee until the library has
            // signatures, and waits for them.
            Expr::Call { .. } if self.inference.callee(expr).is_some() => "a call",
            Expr::Interpolated(_) => "an interpolated string",
            Expr::Lambda(_) => "a lambda",
            Expr::Assign { .. } => "an assignment",
            Expr::Postfix {
                increment: true, ..
            }
            | Expr::Unary {
                op: UnaryOp::PreIncrement,
                ..
            } => "`++`",
            Expr::Postfix {
                increment: false, ..
            }
            | Expr::Unary {
                op: UnaryOp::PreDecrement,
                ..
            } => "`--`",
            Expr::Local(_) => "a local variable",
            Expr::This => "`this`",
            // A field or getter of `this` named by its bare name.
            Expr::Path(_)
                if self
                    .resolution
                    .value_member(self.db, expr)
                    .is_some_and(|member| {
                        !member.is_static
                            && matches!(member.kind, ValueKind::Field | ValueKind::Getter)
                    }) =>
            {
                "`this`"
            }
            _ => return None,
        })
    }

    /// The struct that `new` at `expr` creates memberwise, with `args`
    /// when it has parentheses.
    fn memberwise(&self, expr: ExprId, args: Option<&[Arg]>) -> Option<ItemId> {
        let item = self.inference.expr(expr)?.as_struct()?;
        (creation(self.db, item, args) == Creation::Memberwise).then_some(item)
    }

    /// Records a read of the initializer of each instance field of `item`
    /// that `inits` leave out, in the order of the fields, at the `new`
    /// expression `expr`.
    fn read_left_out(&mut self, expr: ExprId, item: ItemId, inits: &[(String, ExprId)]) {
        let range = self.map.expr(expr);
        for (field, (_, data)) in (0u32..).zip(instance_fields(self.db, item)) {
            if !inits.iter().any(|(name, _)| *name == data.name) {
                let key = ConstKey::Initializer { item, field };
                self.scan.reads.push((item.project, key, range));
            }
        }
    }
}

/// The expressions `node` holds that a scan descends into, in the order of
/// the text: none for a `match` and a block, which are left to the
/// milestones that lower them, for the operand of `nameof`, which is never
/// evaluated, nor for what [`Scanner::unsupported`] stops at. A construct
/// that waits ([`ExprRules::construct_later`]) is stopped at before its
/// parts are asked for, so the parts listed here are those the scan reads
/// once its milestone lands.
pub(super) fn children(node: &Expr) -> Vec<ExprId> {
    match node {
        Expr::Missing
        | Expr::Literal(_)
        | Expr::Interpolated(_)
        | Expr::Local(_)
        | Expr::Path(_)
        | Expr::This
        | Expr::Type(_)
        | Expr::Discard
        | Expr::Postfix { .. }
        | Expr::Assign { .. }
        | Expr::Default(_)
        | Expr::Sizeof(_)
        | Expr::Nameof(_)
        | Expr::Macro
        | Expr::Match { .. }
        | Expr::Block(_)
        | Expr::Lambda(_) => Vec::new(),
        Expr::Tuple(elements) => elements.iter().map(|&(_, element)| element).collect(),
        Expr::Collection(elements) => elements.iter().map(|&(_, element)| element).collect(),
        Expr::Member { receiver, .. } => vec![*receiver],
        Expr::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().map(|arg| arg.expr))
            .collect(),
        Expr::Index { base, args, .. } => std::iter::once(*base)
            .chain(args.iter().map(|arg| arg.expr))
            .collect(),
        Expr::Unary { expr, .. }
        | Expr::Propagate(expr)
        | Expr::Cast { expr, .. }
        | Expr::Await(expr)
        | Expr::Is { expr, .. }
        | Expr::As { expr, .. } => vec![*expr],
        Expr::NewArray { size, .. } => vec![*size],
        Expr::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
        Expr::Range { start, end } => start.iter().chain(end).copied().collect(),
        Expr::With { expr, inits } => std::iter::once(*expr)
            .chain(inits.iter().map(|&(_, value)| value))
            .collect(),
        Expr::Conditional {
            condition,
            then,
            r#else,
        } => vec![*condition, *then, *r#else],
        Expr::New { args, inits, .. } => args
            .iter()
            .flatten()
            .map(|arg| arg.expr)
            .chain(inits.iter().flatten().map(|&(_, value)| value))
            .collect(),
    }
}
