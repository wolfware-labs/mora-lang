//! What an expression may name and where it may stand: a type or a
//! namespace is no value (`MR0127`, §14.5), and what an assignment or a
//! step changes is a place (`MR0186`, §7.11).

use std::collections::HashSet;

use super::Check;
use crate::bodies::{Expr, ExprId, Pat, Target};
use crate::inference::{NotAPlace, TypeError};
use crate::items::ValueKind;
use crate::resolve::{Resolved, item_data, members_named};
use crate::signature::instance_methods;

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
        if !matches!(self.body.exprs[expr], Expr::Path(_) | Expr::Member { .. }) {
            return None;
        }
        // A path in an expression is one segment, the parser making later
        // dots member accesses, so a resolution names the whole of `expr`.
        if let Some(resolution) = self.resolution.paths.get(&expr) {
            return match &resolution.target {
                Target::Item(named @ Resolved::Namespace(_)) => Some(named.clone()),
                Target::Item(Resolved::Item(item)) if item_data(self.db, *item).kind.is_type() => {
                    Some(Resolved::Item(*item))
                }
                _ => None,
            };
        }
        let (ty, name) = self.struct_member(expr)?;
        members_named(self.db, ty, name)
            .find(|(_, data)| data.kind.is_type())
            .map(|(member, _)| Resolved::Item(member))
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
                Target::Value(value) => value_problem(value.member(self.db)?.kind),
                _ => None,
            };
        }
        let (ty, name) = self.struct_member(expr)?;
        if instance_methods(self.db, ty, name).next().is_some() {
            return Some(NotAPlace::Method);
        }
        item_data(self.db, ty)
            .values
            .iter()
            .find(|value| value.name == name && !value.is_static)
            .and_then(|value| match value.kind {
                ValueKind::Getter => Some(NotAPlace::Getter),
                _ => None,
            })
    }
}

/// What a value member of the kind `kind` is when it is not a place.
fn value_problem(kind: ValueKind) -> Option<NotAPlace> {
    match kind {
        ValueKind::Field => None,
        ValueKind::Getter => Some(NotAPlace::Getter),
        ValueKind::Const => Some(NotAPlace::Constant),
        ValueKind::EnumMember | ValueKind::UnionCase => Some(NotAPlace::Case),
    }
}
