//! A static member has no `this`: what it may not use (`MR0149`).

use super::Check;
use crate::bodies::{BodyOwner, Expr, ExprId, Target};
use crate::inference::{Callee, InstanceUse, TypeError};
use crate::items::{ItemKind, item_tree};
use crate::resolve::item_data;
use crate::signature::signature;
use crate::ty::Signature;

impl Check<'_> {
    /// Reports each use of `this`, or of an instance member of the type by
    /// its bare name, in the body of a static member of that type.
    pub(super) fn static_member_uses(&mut self, owner: BodyOwner) {
        if !self.is_static_member(owner) {
            return;
        }
        let body = self.body;
        for (expr, node) in body.exprs.iter() {
            let (at, used) = match node {
                Expr::This => (expr, Some(InstanceUse::This)),
                Expr::Path(_) => (expr, self.instance_value(expr)),
                Expr::Call { callee, .. } => (*callee, self.instance_method(expr, *callee)),
                _ => continue,
            };
            if let Some(used) = used {
                self.errors.push(TypeError::StaticThis { expr: at, used });
            }
        }
    }

    /// Whether `owner` is a static member of a type: a static function, or
    /// the initializer or getter of a static or constant value.
    fn is_static_member(&self, owner: BodyOwner) -> bool {
        let tree = item_tree(self.db, self.scope.file);
        match owner {
            BodyOwner::Item(item) => {
                let data = tree.item(item);
                data.is_static && data.parent.is_some()
            }
            BodyOwner::Value(item, index) => tree
                .item(item)
                .values
                .get(index as usize)
                .is_some_and(|value| value.is_static),
            _ => false,
        }
    }

    /// The instance field or getter the path `path` names, if it does.
    fn instance_value(&self, path: ExprId) -> Option<InstanceUse> {
        let resolution = self.resolution.paths.get(&path)?;
        let Target::Value(value) = &resolution.target else {
            return None;
        };
        let member = value.member(self.db)?;
        (!member.is_static).then(|| InstanceUse::Member {
            kind: if member.is_instance_field() {
                "field"
            } else {
                "getter"
            },
            name: member.name.clone(),
        })
    }

    /// The instance method that the call `call` of a bare name calls, if it
    /// does.
    fn instance_method(&self, call: ExprId, callee: ExprId) -> Option<InstanceUse> {
        if !matches!(self.body.exprs[callee], Expr::Path(_)) {
            return None;
        }
        let Some(Callee::Item(item)) = self.calls.get(&call).copied() else {
            return None;
        };
        let Signature::Function(function) = signature(self.db, item) else {
            return None;
        };
        function.receiver?;
        let data = item_data(self.db, item);
        (data.kind == ItemKind::Function).then(|| InstanceUse::Member {
            kind: "method",
            name: data.name.clone(),
        })
    }
}
