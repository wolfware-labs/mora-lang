//! What a member of a type may use of a value of the type: a static member
//! and the initializer of an instance field have no `this`, and a member of
//! a nested type has no value of the types around it (`MR0149`, `MR0152`).

use std::collections::HashSet;

use mora_span::Db;

use super::Check;
use crate::bodies::{BodyOwner, Expr, ExprId, Target};
use crate::defs::ItemId;
use crate::inference::{Callee, InstanceMember, InstanceUse, TypeError};
use crate::items::{ItemKind, ValueKind, item_tree};
use crate::resolve::{Resolved, item_data};
use crate::signature::signature;
use crate::ty::Signature;

/// The type a body is a member of, and what of a value of it the body has.
struct Member {
    /// The type the body is a member of.
    ty: ItemId,
    /// Whether the body is static, so it has no `this`.
    is_static: bool,
    /// The name of the instance field whose initializer the body is, which
    /// runs before the value exists.
    field: Option<String>,
}

impl Check<'_> {
    /// Reports each use of `this`, or of an instance member by its bare
    /// name, in a body that has no value to use it on: a static member
    /// (`MR0149`), the initializer of an instance field (`MR0152`), or a
    /// member of a nested type naming an instance member of a type around
    /// it (`MR0149`).
    pub(super) fn instance_uses(&mut self, owner: BodyOwner) {
        let Some(member) = self.member(owner) else {
            return;
        };
        let body = self.body;
        // The call reports a callee that names an instance method; any other
        // path that names one is a method group.
        let callees: HashSet<ExprId> = body
            .exprs
            .iter()
            .filter_map(|(_, node)| match node {
                Expr::Call { callee, .. } => Some(*callee),
                _ => None,
            })
            .collect();
        for (expr, node) in body.exprs.iter() {
            let (at, used) = match node {
                Expr::This => (expr, Some((member.ty, InstanceUse::This))),
                Expr::Path(_) => (
                    expr,
                    self.instance_value(expr).or_else(|| {
                        (!callees.contains(&expr))
                            .then(|| self.instance_method_group(expr))
                            .flatten()
                    }),
                ),
                Expr::Call { callee, .. } => (*callee, self.instance_method(expr, *callee)),
                _ => continue,
            };
            let Some((ty, used)) = used else { continue };
            let error = if let Some(field) = &member.field {
                TypeError::InitializerThis {
                    expr: at,
                    used,
                    field: field.clone(),
                }
            } else if ty != member.ty {
                TypeError::NestedThis {
                    expr: at,
                    used,
                    outer: item_data(self.db, ty).name.clone(),
                    nested: item_data(self.db, member.ty).name.clone(),
                }
            } else if member.is_static {
                TypeError::StaticThis { expr: at, used }
            } else {
                continue;
            };
            self.errors.push(error);
        }
    }

    /// The type `owner` is a member of, if it is one. A static member is a
    /// static function, an operator, a static implementation of an
    /// interface member, or the initializer or getter of a static or
    /// constant value.
    fn member(&self, owner: BodyOwner) -> Option<Member> {
        let tree = item_tree(self.db, self.scope.file);
        let (ty, is_static, field) = match owner {
            BodyOwner::Item(item) => {
                let data = tree.item(item);
                (data.parent?, data.is_static, None)
            }
            BodyOwner::Value(ty, index) => {
                let value = tree.item(ty).values.get(index as usize)?;
                let field = value.is_instance_field().then(|| value.name.clone());
                (ty, value.is_static, field)
            }
            BodyOwner::Member(ty, _) => (ty, self.body.is_static, None),
            // An extension member reaches its receiver as a parameter.
            BodyOwner::TopLevel | BodyOwner::Extension(..) => return None,
        };
        Some(Member {
            ty: self.scope.item(ty),
            is_static,
            field,
        })
    }

    /// The instance field or getter the path `path` names, if it does, with
    /// the type that declares it.
    fn instance_value(&self, path: ExprId) -> Option<(ItemId, InstanceUse)> {
        let Target::Value(value) = self.resolution.paths.get(&path)?.target else {
            return None;
        };
        let member = value.member(self.db)?;
        if member.is_static {
            return None;
        }
        let kind = match member.kind {
            ValueKind::Field => InstanceMember::Field,
            ValueKind::Getter => InstanceMember::Getter,
            // These belong to the type, and are `static` already.
            ValueKind::Const | ValueKind::EnumMember | ValueKind::UnionCase => return None,
        };
        let used = InstanceUse::Member {
            kind,
            name: member.name.clone(),
        };
        Some((value.ty, used))
    }

    /// The instance method that the path `path` names as a method group,
    /// outside a call, with the type that declares it. Inference picks no
    /// overload for a method group, so a set of overloads names one only
    /// when each of them takes a receiver.
    fn instance_method_group(&self, path: ExprId) -> Option<(ItemId, InstanceUse)> {
        let Target::Item(Resolved::Functions(items)) = &self.resolution.paths.get(&path)?.target
        else {
            return None;
        };
        let first = *items.first()?;
        if !items.iter().all(|&item| has_receiver(self.db, item)) {
            return None;
        }
        method(self.db, first)
    }

    /// The instance method that the call `call` of a bare name calls, if it
    /// does, with the type that declares it.
    fn instance_method(&self, call: ExprId, callee: ExprId) -> Option<(ItemId, InstanceUse)> {
        if !matches!(self.body.exprs[callee], Expr::Path(_)) {
            return None;
        }
        let Some(Callee::Item(item)) = self.calls.get(&call).copied() else {
            return None;
        };
        if !has_receiver(self.db, item) {
            return None;
        }
        method(self.db, item)
    }
}

/// Whether `item` is a method that takes a receiver.
fn has_receiver(db: &dyn Db, item: ItemId) -> bool {
    item_data(db, item).kind == ItemKind::Function
        && matches!(signature(db, item), Signature::Function(function) if function.receiver.is_some())
}

/// The use of the instance method `item`, with the type that declares it.
fn method(db: &dyn Db, item: ItemId) -> Option<(ItemId, InstanceUse)> {
    let parent = item_tree(db, item.file).item(item.item).parent?;
    let used = InstanceUse::Member {
        kind: InstanceMember::Method,
        name: item_data(db, item).name.clone(),
    };
    Some((
        ItemId {
            item: parent,
            ..item
        },
        used,
    ))
}
