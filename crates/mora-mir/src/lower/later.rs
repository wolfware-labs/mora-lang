//! The bodies that a later milestone of the roadmap lowers. [`mir`] stops
//! on one with an internal error that names the milestone, and
//! [`mir_dump`] writes a line in its place.
//!
//! A body comes later for what it is, as a getter; for a type in its
//! signature, as a class type; or for a local function in it that comes
//! later, since a local function is lowered with the body it is declared
//! in.
//!
//! [`mir`]: super::mir
//! [`mir_dump`]: super::mir_dump

use mora_hir::{
    BodyKind, BodyOwner, Construct, FnSignature, ItemId, ItemKind, Signature, Stmt, Ty, ValueKind,
    body, body_resolution, infer, is_generic_type, item_tree, local_functions, signature,
    this_type,
};
use mora_span::{Db, Project, SourceFile};

use super::structs::field_of_value;

/// The milestone that lowers generic code: monomorphization.
const GENERIC: &str = "M3.9";

/// The milestone that lowers async functions.
const ASYNC: &str = "M6.2";

/// A body that is not lowered yet: what it is and the milestone of the
/// roadmap that lowers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Later {
    /// What the body is, as `a getter`.
    pub(crate) what: String,
    /// The milestone, as `M2.1`.
    pub(crate) milestone: &'static str,
}

/// What a body is that a later milestone lowers.
fn later(what: impl Into<String>, milestone: &'static str) -> Option<Later> {
    Some(Later {
        what: what.into(),
        milestone,
    })
}

/// A kind of type, or of declaration a member is in, that a later
/// milestone lowers.
#[derive(Clone, Copy)]
struct Kind {
    /// How a message names it, as `a class type`.
    what: &'static str,
    /// The milestone.
    milestone: &'static str,
}

/// The milestone that lowers the body of `owner`, `None` when this one
/// does.
pub(crate) fn lowered_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    let later =
        owner_later(db, project, file, owner).or_else(|| local_later(db, project, file, owner))?;
    Some(in_generic_struct(db, project, file, owner, later))
}

/// `later` for a body of `owner`, or the milestone of generic code when
/// the owner is a member of a generic struct and `later` names an earlier
/// one: the latest of the milestones that apply lowers the body.
fn in_generic_struct(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
    later: Later,
) -> Later {
    let tree = item_tree(db, file);
    let ty = match owner {
        BodyOwner::Item(item) => tree.item(item).parent,
        BodyOwner::Value(ty, _) | BodyOwner::Member(ty, _) => Some(ty),
        BodyOwner::TopLevel | BodyOwner::Extension(..) => None,
    };
    let generic = ty.is_some_and(|ty| {
        tree.item(ty).kind == ItemKind::Struct
            && is_generic_type(
                db,
                ItemId {
                    project,
                    file,
                    item: ty,
                },
            )
    });
    if generic && milestone_order(later.milestone) < milestone_order(GENERIC) {
        return Later {
            what: format!("{} of a generic struct", later.what),
            milestone: GENERIC,
        };
    }
    later
}

/// The position of `milestone`, as `M3.9`, in the order of the roadmap.
fn milestone_order(milestone: &str) -> (u32, u32) {
    let number = |part: &str| part.parse().unwrap_or(u32::MAX);
    match milestone.trim_start_matches('M').split_once('.') {
        Some((major, minor)) => (number(major), number(minor)),
        None => (u32::MAX, u32::MAX),
    }
}

/// The milestone that lowers the body of `owner` for what it is or for a
/// type in its signature, `None` when this one does.
fn owner_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
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
                Signature::Function(function) => function_later(function)
                    .or_else(|| {
                        if function.receiver.is_some() && this_type(db, id(item)).is_none() {
                            method_later(data.parent.map(|parent| tree.item(parent).kind))
                        } else {
                            None
                        }
                    })
                    .or_else(|| signature_later(function)),
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
                ValueKind::Field => match parent_later(data.kind) {
                    Some(parent) => later(
                        format!("the initializer of a field of {}", parent.what),
                        parent.milestone,
                    ),
                    None => {
                        let kind = type_later(&field_of_value(db, id(ty), index)?.ty)?;
                        later(
                            format!("the initializer of a field with {}", kind.what),
                            kind.milestone,
                        )
                    }
                },
                ValueKind::Const => later("the initializer of a constant", "M1.11"),
                ValueKind::Getter => match parent_later(data.kind) {
                    Some(parent) if data.kind != ItemKind::Class => {
                        later(format!("a getter of {}", parent.what), parent.milestone)
                    }
                    _ => later("a getter", "M2.1"),
                },
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
            BodyKind::TopLevel | BodyKind::Initializer | BodyKind::Extension | BodyKind::Case => {
                None
            }
        },
        BodyOwner::Extension(_, 0) => later("an extension block", "M3.12"),
        BodyOwner::Extension(..) => later("an extension member", "M3.12"),
    }
}

/// The milestone that lowers a function with the signature `function`,
/// `None` when this one does: generic and async functions come later.
fn function_later(function: &FnSignature) -> Option<Later> {
    if function.is_generic {
        return later("a generic function", GENERIC);
    }
    if function.is_async {
        return later("an async function", ASYNC);
    }
    None
}

/// The milestone that lowers a method that takes `this` in a type of the
/// kind `parent` whose `this` has no type yet: a struct's is generic.
fn method_later(parent: Option<ItemKind>) -> Option<Later> {
    match parent? {
        ItemKind::Struct => later("a method of a generic struct", GENERIC),
        kind => {
            let parent = parent_later(kind)?;
            later(format!("a method of {}", parent.what), parent.milestone)
        }
    }
}

/// The milestone that lowers the members of a type of the kind `kind`,
/// `None` for a struct, whose members this one lowers, and for what is
/// not a type.
fn parent_later(kind: ItemKind) -> Option<Kind> {
    let (what, milestone) = match kind {
        ItemKind::Class => ("a class", "M2.1"),
        ItemKind::Union => ("a union", "M3.1"),
        ItemKind::Enum => ("an enum", "M3.2"),
        ItemKind::Interface => ("an interface", "M3.10"),
        ItemKind::Struct | ItemKind::Function | ItemKind::Macro | ItemKind::Field => return None,
    };
    Some(Kind { what, milestone })
}

/// The milestone that lowers a function with the signature `function` for
/// the types of its parameters and its return type, `None` when this one
/// does.
fn signature_later(function: &FnSignature) -> Option<Later> {
    let kind = function
        .params
        .iter()
        .map(|param| &param.ty)
        .chain([&function.ret])
        .find_map(type_later)?;
    later(format!("a signature with {}", kind.what), kind.milestone)
}

/// The milestone that lowers the first local function of the body of
/// `owner`, in the order of the text, that comes later: a generic or async
/// one, or one with a type in its signature that comes later. `None` when
/// this one lowers them all.
fn local_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let functions = local_functions(db, project, file, owner);
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    let resolution = body_resolution(db, project, file, owner);
    functions.iter().find_map(|function| {
        let Stmt::Function(def) = &hir.stmts[function.stmt] else {
            return None;
        };
        let name = &hir.locals[def.local].name;
        if def.is_generic {
            return later(format!("the generic local function `{name}`"), GENERIC);
        }
        if def.is_async {
            return later(format!("the async local function `{name}`"), ASYNC);
        }
        let ret = resolution.return_type(def.ret);
        let kind = def
            .params
            .iter()
            .filter_map(|&param| inference.local(param))
            .chain([&ret])
            .find_map(type_later)?;
        later(
            format!(
                "the local function `{name}` with {} in its signature",
                kind.what
            ),
            kind.milestone,
        )
    })
}

/// The milestone that lowers a body with the type `ty` in its signature,
/// `None` when this one does. The first element of a tuple that comes
/// later decides for the tuple, and the outer type decides for the types
/// inside it, as for `C[]`, an array type.
fn type_later(ty: &Ty) -> Option<Kind> {
    let construct = match ty {
        Ty::Primitive(_) | Ty::String | Ty::Struct(_) | Ty::Never | Ty::Error => return None,
        Ty::Tuple(elems) => return elems.iter().find_map(|elem| type_later(&elem.ty)),
        Ty::Unsupported(ty) => ty.construct,
    };
    let (what, milestone) = match construct {
        Construct::Array => ("an array type", "M1.11"),
        Construct::External => ("a type of the standard library", "M1.15"),
        Construct::Class => ("a class type", "M2.1"),
        Construct::Weak => ("a weak reference type", "M2.10"),
        Construct::Function => ("a function type", "M2.11"),
        Construct::Union => ("a union type", "M3.1"),
        Construct::Enum => ("an enum type", "M3.2"),
        Construct::Optional => ("an option type", "M3.5"),
        Construct::TypeParam => ("a type parameter", GENERIC),
        Construct::Generic => ("a generic type", GENERIC),
        Construct::Interface => ("an interface type", "M3.10"),
        Construct::Any => ("an interface value type", "M3.11"),
        Construct::Splice => ("a spliced type", "M4.4"),
    };
    Some(Kind { what, milestone })
}
