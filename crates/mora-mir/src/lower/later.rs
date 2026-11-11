//! The bodies that a later milestone of the roadmap lowers. [`mir`] stops
//! on one with an internal error that names the milestone, and
//! [`mir_dump`] writes a line in its place.
//!
//! [`mir`]: super::mir
//! [`mir_dump`]: super::mir_dump

use mora_hir::{
    BodyKind, BodyOwner, FnSignature, ItemId, ItemKind, Signature, ValueKind, body, item_tree,
    signature, this_type,
};
use mora_span::{Db, Project, SourceFile};

/// A body that is not lowered yet: what it is and the milestone of the
/// roadmap that lowers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Later {
    /// What the body is, as `a getter`.
    pub(crate) what: &'static str,
    /// The milestone, as `M2.1`.
    pub(crate) milestone: &'static str,
}

/// What a body is that a later milestone lowers.
const fn later(what: &'static str, milestone: &'static str) -> Option<Later> {
    Some(Later { what, milestone })
}

/// The milestone that lowers the body of `owner`, `None` when this one
/// does.
pub(crate) fn lowered_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    let tree = item_tree(db, file);
    let id = |item| ItemId {
        project,
        file,
        item,
    };
    match owner {
        BodyOwner::TopLevel => None,
        BodyOwner::Item(item) => {
            let data = tree.item(item);
            match signature(db, id(item)) {
                Signature::Function(function) => match function_later(function) {
                    Some(later) => Some(later),
                    None if function.receiver.is_some() && this_type(db, id(item)).is_none() => {
                        method_later(data.parent.map(|parent| tree.item(parent).kind))
                    }
                    None => None,
                },
                _ if data.kind == ItemKind::Field && data.is_static => {
                    later("the initializer of a static", "M3.13")
                }
                _ if data.kind == ItemKind::Field => {
                    later("the initializer of a constant", "M1.11")
                }
                _ => None,
            }
        }
        BodyOwner::Value(ty, index) => {
            let data = tree.item(ty);
            let value = data.values.get(index as usize)?;
            match value.kind {
                ValueKind::Field if value.is_static => {
                    later("the initializer of a static field", "M3.13")
                }
                ValueKind::Field if data.kind == ItemKind::Class => {
                    later("the initializer of a field of a class", "M2.1")
                }
                ValueKind::Field => None,
                ValueKind::Const => later("the initializer of a constant", "M1.11"),
                ValueKind::Getter => later("a getter", "M2.1"),
                ValueKind::EnumMember => later("the value of an enum member", "M3.2"),
                ValueKind::UnionCase => later("a union case", "M3.1"),
            }
        }
        BodyOwner::Member(..) => match body(db, file, owner).kind {
            BodyKind::Constructor => later("a constructor", "M2.1"),
            BodyKind::Destructor => later("a destructor", "M2.6"),
            BodyKind::Operator => later("an operator", "M3.6"),
            BodyKind::Get | BodyKind::Set => later("an indexer", "M3.6"),
            BodyKind::Function | BodyKind::Getter => {
                later("an explicit interface implementation", "M3.10")
            }
            _ => None,
        },
        BodyOwner::Extension(_, 0) => later("an extension block", "M3.12"),
        BodyOwner::Extension(..) => later("an extension member", "M3.12"),
    }
}

/// The milestone that lowers a function with the signature `function`,
/// `None` when this one does: generic and async functions come later.
fn function_later(function: &FnSignature) -> Option<Later> {
    if function.is_generic {
        return later("a generic function", "M3.8");
    }
    if function.is_async {
        return later("an async function", "M6.2");
    }
    None
}

/// The milestone that lowers a method that takes `this` in a type of the
/// kind `parent` whose `this` has no type yet: a struct's is generic.
fn method_later(parent: Option<ItemKind>) -> Option<Later> {
    match parent? {
        ItemKind::Struct => later("a method of a generic struct", "M3.8"),
        ItemKind::Class => later("a method of a class", "M2.1"),
        ItemKind::Union => later("a method of a union", "M3.1"),
        ItemKind::Interface => later("a method of an interface", "M3.10"),
        ItemKind::Enum | ItemKind::Function | ItemKind::Macro | ItemKind::Field => None,
    }
}
