//! What an expression may name and where it may stand: a type or a
//! namespace is no value (`MR0127`, §14.5), and what an assignment or a
//! step changes is a place (`MR0186`, §7.11).

use std::collections::HashSet;

use super::Check;
use crate::bodies::{Expr, ExprId, Pat, Target};
use crate::defs::ItemId;
use crate::inference::{NotAPlace, TypeError};
use crate::items::{ItemKind, ValueKind};
use crate::resolve::{Resolved, item_data};
use crate::ty::Ty;

impl Check<'_> {
    /// Reports each expression that names a type or a namespace where a
    /// value is expected (`MR0127`, D-154): anywhere but as the receiver of
    /// a member, as `P` in `P.Create()`, the operand of `nameof`, or a
    /// pattern.
    pub(super) fn types_as_values(&mut self) {
        let body = self.body;
        let mut allowed = HashSet::new();
        for (_, node) in body.exprs.iter() {
            match node {
                Expr::Member { receiver, .. } => {
                    allowed.insert(*receiver);
                }
                Expr::Nameof(inner) => {
                    allowed.insert(*inner);
                }
                _ => {}
            }
        }
        // A pattern tests for a type by its name, as `p is P`.
        for (_, pat) in body.pats.iter() {
            match pat {
                Pat::Constant(expr) | Pat::Case { path: expr, .. } => {
                    allowed.insert(*expr);
                }
                _ => {}
            }
        }
        for (expr, _) in body.exprs.iter() {
            if allowed.contains(&expr) {
                continue;
            }
            if let Some(named) = self.named_type(expr) {
                self.errors.push(TypeError::TypeAsValue { expr, named });
            }
        }
    }

    /// The type or namespace that `expr`, a path or a member, names as a
    /// whole: through a path, or as a nested type of a struct value.
    fn named_type(&self, expr: ExprId) -> Option<Resolved> {
        let body = self.body;
        let path_len = match &body.exprs[expr] {
            Expr::Path(path) => path.segments.len(),
            Expr::Member { .. } => 1,
            _ => return None,
        };
        if let Some(resolution) = self.resolution.paths.get(&expr) {
            // Later segments are members of what the first ones name.
            if resolution.segments.max(1) < path_len {
                return None;
            }
            return match &resolution.target {
                Target::Item(named @ Resolved::Namespace(_)) => Some(named.clone()),
                Target::Item(Resolved::Item(item)) if is_type(self.db, *item) => {
                    Some(Resolved::Item(*item))
                }
                _ => None,
            };
        }
        let Expr::Member { receiver, name, .. } = &body.exprs[expr] else {
            return None;
        };
        let Some(Ty::Struct(ty)) = &self.exprs[crate::arena::arena_index(*receiver)] else {
            return None;
        };
        item_data(self.db, *ty)
            .members
            .iter()
            .map(|&member| ItemId {
                item: member,
                ..*ty
            })
            .find(|&member| item_data(self.db, member).name == *name && is_type(self.db, member))
            .map(Resolved::Item)
    }

    /// Reports `place`, the left side of an assignment or the operand of a
    /// step, when it is not a place (`MR0186`, D-155): a place is a local, a
    /// parameter, `this`, a field, an element, a tuple of places, or `_`.
    /// A member whose kind is not known yet, and a static member named
    /// through a value (D-152), is not reported; a type or a namespace is
    /// `MR0127` already.
    pub(super) fn require_place(&mut self, place: ExprId) {
        let body = self.body;
        let problem = match &body.exprs[place] {
            Expr::Tuple(elems) => {
                for &(_, elem) in elems {
                    self.require_place(elem);
                }
                return;
            }
            Expr::Local(_) | Expr::This | Expr::Index { .. } | Expr::Discard | Expr::Missing => {
                return;
            }
            Expr::Path(_) | Expr::Member { .. } => match self.named_member(place) {
                Some(problem) => problem,
                None => return,
            },
            Expr::Call { .. } => NotAPlace::Call,
            _ => NotAPlace::Value,
        };
        self.errors.push(TypeError::NotAPlace {
            expr: place,
            problem,
        });
    }

    /// What `expr`, a path or a member, names that is not a place, `None`
    /// for a place or for something not known yet.
    fn named_member(&self, expr: ExprId) -> Option<NotAPlace> {
        if let Some(resolution) = self.resolution.paths.get(&expr) {
            return match &resolution.target {
                Target::Item(Resolved::Functions(_)) => Some(NotAPlace::Method),
                Target::Value(value) => {
                    value_problem(value.member(self.db)?.kind, resolution.segments, expr, self)
                }
                _ => None,
            };
        }
        let Expr::Member { receiver, name, .. } = &self.body.exprs[expr] else {
            return None;
        };
        let Some(Ty::Struct(ty)) = &self.exprs[crate::arena::arena_index(*receiver)] else {
            return None;
        };
        let data = item_data(self.db, *ty);
        let method = data.members.iter().any(|&member| {
            let member = item_data(
                self.db,
                ItemId {
                    item: member,
                    ..*ty
                },
            );
            member.kind == ItemKind::Function && member.name == *name && !member.is_static
        });
        if method {
            return Some(NotAPlace::Method);
        }
        data.values
            .iter()
            .find(|value| value.name == *name && !value.is_static)
            .and_then(|value| match value.kind {
                ValueKind::Getter => Some(NotAPlace::Getter),
                _ => None,
            })
    }
}

/// What a value member of the kind `kind` that a path names is, when it is
/// not a place: only the whole path names the member, so a field of it is
/// a place.
fn value_problem(
    kind: ValueKind,
    segments: usize,
    expr: ExprId,
    check: &Check<'_>,
) -> Option<NotAPlace> {
    if let Expr::Path(path) = &check.body.exprs[expr]
        && segments.max(1) < path.segments.len()
    {
        return None;
    }
    match kind {
        ValueKind::Field => None,
        ValueKind::Getter => Some(NotAPlace::Getter),
        ValueKind::Const => Some(NotAPlace::Constant),
        ValueKind::EnumMember | ValueKind::UnionCase => Some(NotAPlace::Case),
    }
}

/// Whether `item` is a type: anything but a function, a macro or a value.
fn is_type(db: &dyn mora_span::Db, item: ItemId) -> bool {
    !matches!(
        item_data(db, item).kind,
        ItemKind::Function | ItemKind::Macro | ItemKind::Field
    )
}
