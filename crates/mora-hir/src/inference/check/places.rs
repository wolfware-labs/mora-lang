//! What an expression may name and where it may stand: a type or a
//! namespace is no value (`MR0127`, §14.5), and what an assignment or a
//! step changes is a place (`MR0186`, §7.11).

use super::Check;
use crate::arena::arena_index;
use crate::bodies::{Body, Expr, ExprId, LocalKind, Pat, Target};
use crate::inference::{NotAPlace, TypeError};
use crate::items::ValueKind;
use crate::resolve::{Resolved, members_named};
use crate::signature::{instance_field_named, instance_methods};

impl Check<'_> {
    /// Reports each expression that names a type or a namespace where a
    /// value is expected (`MR0127`, D-154): anywhere but as the receiver of
    /// a member, as `P` in `P.Create()`, the operand of `nameof`, or a
    /// pattern. A member of a value is no way to write a type, so a nested
    /// type named through a value is reported wherever it stands, and so is
    /// a type keyword anywhere but as a receiver, as `int` in `int.Parse`.
    pub(super) fn types_as_values(&mut self) {
        for (expr, _) in self.body.exprs.iter() {
            if let Some(error) = self.named_as_value(expr) {
                self.errors.push(error);
            }
        }
    }

    /// The error of `expr` when it names a type, a type keyword or a
    /// namespace where a value is expected, which
    /// [`Check::types_as_values`] reports.
    pub(super) fn named_as_value(&self, expr: ExprId) -> Option<TypeError> {
        // Where a type may stand is looked up only once an expression names
        // a type or is a type keyword.
        let allowed = |expr: ExprId| {
            self.type_positions
                .get_or_init(|| type_positions(self.body))[arena_index(expr)]
        };
        match &self.body.exprs[expr] {
            Expr::Type(_) if !allowed(expr) => Some(TypeError::TypeKeywordAsValue { expr }),
            // Only a path or a member names a type or a namespace.
            Expr::Path(_) | Expr::Member { .. } => {
                let named = self.named_type(expr)?;
                if allowed(expr) && !self.member_of_value(expr) {
                    return None;
                }
                Some(TypeError::TypeAsValue { expr, named })
            }
            _ => None,
        }
    }

    /// Whether `expr` is a member of a value, which no path resolution
    /// names, rather than a member of a type or a namespace.
    fn member_of_value(&self, expr: ExprId) -> bool {
        matches!(self.body.exprs[expr], Expr::Member { .. })
            && !self.resolution.paths.contains_key(&expr)
    }

    /// The type or namespace that `expr`, a path or a member, names as a
    /// whole: through a path, or as a nested type of a struct value.
    fn named_type(&self, expr: ExprId) -> Option<Resolved> {
        // A path in an expression is one segment, the parser making later
        // dots member accesses, so a resolution names the whole of `expr`.
        if let Some(resolution) = self.resolution.paths.get(&expr) {
            return match &resolution.target {
                Target::Item(named @ Resolved::Namespace(_)) => Some(named.clone()),
                Target::Item(Resolved::Item(item)) if self.item_data(*item).kind.is_type() => {
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
    /// step, when it is not a place (`MR0186`, D-155). `discard` says that
    /// `_` may stand there, as on the left of a plain `=`. A tuple of places
    /// is a place for `=`, each element checked.
    pub(super) fn require_place(&mut self, place: ExprId, discard: bool) {
        if let Expr::Tuple(elems) = &self.body.exprs[place] {
            for &(_, elem) in elems {
                self.require_place(elem, discard);
            }
            return;
        }
        if let Some(problem) = self.place_problem(place, discard) {
            self.errors.push(TypeError::NotAPlace {
                expr: place,
                problem,
            });
        }
    }

    /// Why `expr` is not a place, `None` when it is one or is not known
    /// yet. A place is a local, a parameter, `this`, an element, a field of
    /// a place, a static field, or `_` where `discard` allows it. A field of
    /// a struct is part of the value that holds it, so it is a place only
    /// when that value is one; a member of a value whose type is not known
    /// yet, as a class's, and any member of a struct value that is not a
    /// declared instance field, as a static member named through a value
    /// (D-152), are not reported. A type, a type keyword or a namespace is
    /// `MR0127` already.
    fn place_problem(&self, expr: ExprId, discard: bool) -> Option<NotAPlace> {
        match &self.body.exprs[expr] {
            Expr::Local(local) => (self.body.locals[*local].kind == LocalKind::Function)
                .then_some(NotAPlace::Function),
            // A type keyword is `MR0127` already.
            Expr::This | Expr::Index { .. } | Expr::Missing | Expr::Type(_) => None,
            Expr::Discard => (!discard).then_some(NotAPlace::Discard),
            Expr::Path(_) => self.named_member(expr),
            Expr::Member { receiver, .. } => {
                if let Some(problem) = self.named_member(expr) {
                    return Some(problem);
                }
                // A member of a type, as a static field through it, has no
                // value around it.
                if !self.member_of_value(expr) {
                    return None;
                }
                // Only a declared instance field is part of the value; any
                // other name keeps the rule of its own milestone.
                let (ty, name) = self.struct_member(expr)?;
                let field = instance_field_named(self.db, ty, name).is_some();
                (field && self.place_problem(*receiver, false).is_some())
                    .then_some(NotAPlace::FieldOfValue)
            }
            Expr::Call { .. } => Some(NotAPlace::Call),
            _ => Some(NotAPlace::Value),
        }
    }

    /// What `expr`, a path or a member, names that is not a place, `None`
    /// for a place or for something not known yet.
    fn named_member(&self, expr: ExprId) -> Option<NotAPlace> {
        if let Some(resolution) = self.resolution.paths.get(&expr) {
            return match &resolution.target {
                Target::Item(Resolved::Functions(_)) => Some(NotAPlace::Function),
                // A constant at namespace level; a static there is a place.
                Target::Item(Resolved::Item(item)) => {
                    let data = self.item_data(*item);
                    data.is_constant().then_some(NotAPlace::Constant)
                }
                Target::Value(value) => value_problem(self.value_member(*value)?.kind),
                _ => None,
            };
        }
        let (ty, name) = self.struct_member(expr)?;
        if instance_methods(self.db, ty, name).next().is_some() {
            return Some(NotAPlace::Function);
        }
        self.item_data(ty)
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

/// Whether each expression of `body`, by its index, stands where a type
/// may ([`Check::types_as_values`]): as the receiver of a member, as `P`
/// in `P.Create()`, the operand of `nameof`, or a pattern, which tests for
/// a type by its name, as `p is P`.
fn type_positions(body: &Body) -> Vec<bool> {
    let mut allowed = vec![false; body.exprs.len()];
    for (_, node) in body.exprs.iter() {
        match node {
            Expr::Member { receiver, .. } => allowed[arena_index(*receiver)] = true,
            Expr::Nameof(inner) => allowed[arena_index(*inner)] = true,
            _ => {}
        }
    }
    for (_, pat) in body.pats.iter() {
        match pat {
            Pat::Constant(expr) | Pat::Case { path: expr, .. } => {
                allowed[arena_index(*expr)] = true;
            }
            _ => {}
        }
    }
    allowed
}
