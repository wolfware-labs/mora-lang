//! A static member and the initializer of an instance field have no `this`:
//! what they may not use (`MR0149`, `MR0152`).

use super::Check;
use crate::bodies::{BodyOwner, Expr, ExprId};
use crate::inference::{Callee, InstanceMember, InstanceUse, TypeError};
use crate::items::{ItemKind, ValueKind, item_tree};
use crate::resolve::item_data;
use crate::signature::signature;
use crate::ty::Signature;

impl Check<'_> {
    /// Reports each use of `this`, or of an instance member of the type by
    /// its bare name, in the body of a static member of that type
    /// (`MR0149`) or in the initializer of one of its instance fields
    /// (`MR0152`).
    pub(super) fn instance_uses(&mut self, owner: BodyOwner) {
        let field = self.initialized_field(owner);
        if field.is_none() && !self.is_static_member(owner) {
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
            let Some(used) = used else { continue };
            self.errors.push(match &field {
                Some(field) => TypeError::InitializerThis {
                    expr: at,
                    used,
                    field: field.clone(),
                },
                None => TypeError::StaticThis { expr: at, used },
            });
        }
    }

    /// Whether `owner` is a static member of a type: a static function, an
    /// operator, a static implementation of an interface member, or the
    /// initializer or getter of a static or constant value.
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
            BodyOwner::Member(..) => self.body.is_static,
            BodyOwner::TopLevel | BodyOwner::Extension(..) => false,
        }
    }

    /// The name of the instance field whose initializer `owner` is, if it
    /// is one.
    fn initialized_field(&self, owner: BodyOwner) -> Option<String> {
        let BodyOwner::Value(item, index) = owner else {
            return None;
        };
        let tree = item_tree(self.db, self.scope.file);
        let value = tree.item(item).values.get(index as usize)?;
        value.is_instance_field().then(|| value.name.clone())
    }

    /// The instance field or getter the path `path` names, if it does.
    fn instance_value(&self, path: ExprId) -> Option<InstanceUse> {
        let member = self.resolution.value_member(self.db, path)?;
        if member.is_static {
            return None;
        }
        let kind = match member.kind {
            ValueKind::Field => InstanceMember::Field,
            ValueKind::Getter => InstanceMember::Getter,
            // These belong to the type, and are `static` already.
            ValueKind::Const | ValueKind::EnumMember | ValueKind::UnionCase => return None,
        };
        Some(InstanceUse::Member {
            kind,
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
            kind: InstanceMember::Method,
            name: data.name.clone(),
        })
    }
}
