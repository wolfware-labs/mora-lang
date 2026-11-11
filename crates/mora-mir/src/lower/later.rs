//! The bodies that a later milestone of the roadmap lowers. [`mir`] stops
//! on one with an internal error that names the milestone, and
//! [`mir_dump`] writes a line in its place.
//!
//! A body comes later for what it is, as a getter; for a type in its
//! signature, as a class type; for a `this` of a generic type, as in a
//! method of `struct B<T>`; or for a local function in it that comes
//! later, since a local function is lowered with the body it is declared
//! in. When several milestones apply, the latest of them lowers the body.
//!
//! [`mir`]: super::mir
//! [`mir_dump`]: super::mir_dump

use std::fmt;
use std::iter;

use mora_hir::{
    BodyKind, BodyOwner, Construct, FnSignature, ItemId, ItemKind, Signature, Stmt, Ty, ValueKind,
    body, body_resolution, infer, is_generic_type, item_tree, local_functions, signature,
};
use mora_span::{Db, Project, SourceFile};

use super::structs::field_of_value;

/// A milestone of the roadmap, as `M3.9`. A later milestone compares
/// greater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Milestone {
    /// The phase, as `3` in `M3.9`.
    phase: u8,
    /// The milestone within the phase, as `9` in `M3.9`.
    number: u8,
}

impl fmt::Display for Milestone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "M{}.{}", self.phase, self.number)
    }
}

/// The milestone of the roadmap numbered `M<phase>.<number>`.
const fn milestone(phase: u8, number: u8) -> Milestone {
    Milestone { phase, number }
}

/// M1.11, constant evaluation.
pub(super) const CONSTANT_EVALUATION: Milestone = milestone(1, 11);
/// M1.15, the standard library bootstrap.
const STD_BOOTSTRAP: Milestone = milestone(1, 15);
/// M2.1, classes, fields, getters and constructors.
const CLASSES: Milestone = milestone(2, 1);
/// M2.6, destructors and drop order.
const DESTRUCTORS: Milestone = milestone(2, 6);
/// M2.10, weak references.
const WEAK_REFERENCES: Milestone = milestone(2, 10);
/// M2.11, closures and function types.
const CLOSURES: Milestone = milestone(2, 11);
/// M3.1, unions.
const UNIONS: Milestone = milestone(3, 1);
/// M3.2, enums.
const ENUMS: Milestone = milestone(3, 2);
/// M3.5, options.
const OPTIONS: Milestone = milestone(3, 5);
/// M3.6, operators, conversions and indexers.
const OPERATORS: Milestone = milestone(3, 6);
/// M3.9, monomorphization, which lowers generic code.
const MONOMORPHIZATION: Milestone = milestone(3, 9);
/// M3.10, interfaces and static dispatch.
const INTERFACES: Milestone = milestone(3, 10);
/// M3.11, dynamic interface values.
const DYNAMIC_INTERFACES: Milestone = milestone(3, 11);
/// M3.12, tuples, collection expressions and smaller expressions.
const SMALLER_EXPRESSIONS: Milestone = milestone(3, 12);
/// M3.13, statics.
pub(super) const STATICS: Milestone = milestone(3, 13);
/// M4.4, macro expansion, ordering and hygiene.
const EXPANSION: Milestone = milestone(4, 4);
/// M6.2, async lowering.
const ASYNC: Milestone = milestone(6, 2);

/// A body that is not lowered yet: what it is and the milestone of the
/// roadmap that lowers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Later {
    /// What the body is, as `a getter`.
    pub(crate) what: String,
    /// The milestone, as `M2.1`.
    pub(crate) milestone: Milestone,
}

impl Later {
    /// A body that is `what` and that `milestone` lowers.
    fn new(what: impl Into<String>, milestone: Milestone) -> Self {
        Self {
            what: what.into(),
            milestone,
        }
    }
}

/// What a body is that a later milestone lowers.
fn later(what: impl Into<String>, milestone: Milestone) -> Option<Later> {
    Some(Later::new(what, milestone))
}

/// A kind of type, or of declaration a member is in, that a later
/// milestone lowers.
#[derive(Clone, Copy)]
struct Kind {
    /// How a message names it, as `a class type`.
    what: &'static str,
    /// The milestone.
    milestone: Milestone,
}

impl Kind {
    /// A body that comes later for this kind, which `phrase` names from
    /// the name of the kind.
    fn later(self, phrase: impl FnOnce(&str) -> String) -> Option<Later> {
        later(phrase(self.what), self.milestone)
    }
}

/// What a milestone of the roadmap lowers.
trait Due {
    /// The milestone that lowers it.
    fn milestone(&self) -> Milestone;
}

impl Due for Later {
    fn milestone(&self) -> Milestone {
        self.milestone
    }
}

impl Due for Kind {
    fn milestone(&self) -> Milestone {
        self.milestone
    }
}

/// The one of `candidates` that the latest milestone lowers, the first of
/// them on a tie: a body is lowered once every milestone that applies to
/// it is done.
fn latest<T: Due>(candidates: impl IntoIterator<Item = Option<T>>) -> Option<T> {
    candidates.into_iter().flatten().reduce(|latest, next| {
        if next.milestone() > latest.milestone() {
            next
        } else {
            latest
        }
    })
}

/// The milestone that lowers the body of `owner`, `None` when this one
/// does.
pub(crate) fn lowered_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    latest([
        owner_later(db, project, file, owner),
        local_later(db, project, file, owner),
        this_later(db, project, file, owner),
    ])
}

/// The milestone of generic code when the body of `owner` has a `this` of
/// a generic type: a method, a getter, a field initializer, a constructor,
/// a destructor, an indexer or an interface implementation that is not
/// `static`, in a type with type parameters. `None` for any other body: a
/// static member of a generic type is lowered for what it is, and a type
/// parameter in its signature comes later for that type.
fn this_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let tree = item_tree(db, file);
    let id = |item| ItemId {
        project,
        file,
        item,
    };
    let (ty, what) = match owner {
        BodyOwner::Item(item) => {
            let Signature::Function(function) = signature(db, id(item)) else {
                return None;
            };
            function.receiver?;
            (tree.item(item).parent?, "a method")
        }
        BodyOwner::Value(ty, index) => {
            let value = tree.item(ty).values.get(index as usize)?;
            if value.is_static {
                return None;
            }
            match value.kind {
                ValueKind::Field => (ty, "the initializer of a field"),
                ValueKind::Getter => (ty, "a getter"),
                ValueKind::Const | ValueKind::EnumMember | ValueKind::UnionCase => return None,
            }
        }
        BodyOwner::Member(ty, _) => {
            let hir = body(db, file, owner);
            if hir.is_static {
                return None;
            }
            let what = match hir.kind {
                BodyKind::Constructor => "a constructor",
                BodyKind::Destructor => "a destructor",
                BodyKind::Get | BodyKind::Set => "an indexer",
                BodyKind::Function | BodyKind::Getter => "an explicit interface implementation",
                BodyKind::Operator
                | BodyKind::TopLevel
                | BodyKind::Initializer
                | BodyKind::Extension
                | BodyKind::Case => return None,
            };
            (ty, what)
        }
        BodyOwner::TopLevel | BodyOwner::Extension(..) => return None,
    };
    is_generic_type(db, id(ty))
        .then(|| Later::new(format!("{what} of a generic type"), MONOMORPHIZATION))
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
                Signature::Function(function) => {
                    let method = function
                        .receiver
                        .and(data.parent)
                        .and_then(|parent| parent_later(tree.item(parent).kind))
                        .and_then(|kind| kind.later(|what| format!("a method of {what}")));
                    latest([function_later(function), method, signature_later(function)])
                }
                _ if data.kind == ItemKind::Field && data.is_static => {
                    later("the initializer of a static", STATICS)
                }
                _ if data.kind == ItemKind::Field => {
                    later("the initializer of a constant", CONSTANT_EVALUATION)
                }
                _ => None,
            }
        }
        BodyOwner::Value(ty, index) => {
            let data = tree.item(ty);
            let value = data.values.get(index as usize)?;
            match value.kind {
                ValueKind::Field if value.is_static => {
                    later("the initializer of a static field", STATICS)
                }
                ValueKind::Field => match parent_later(data.kind) {
                    Some(parent) => {
                        parent.later(|what| format!("the initializer of a field of {what}"))
                    }
                    None => type_later(&field_of_value(db, id(ty), index)?.ty)?
                        .later(|what| format!("the initializer of a field with {what}")),
                },
                ValueKind::Const => later("the initializer of a constant", CONSTANT_EVALUATION),
                ValueKind::Getter => match parent_later(data.kind) {
                    Some(parent) if data.kind != ItemKind::Class => {
                        parent.later(|what| format!("a getter of {what}"))
                    }
                    _ => later("a getter", CLASSES),
                },
                ValueKind::EnumMember => later("the value of an enum member", ENUMS),
                ValueKind::UnionCase => later("a union case", UNIONS),
            }
        }
        BodyOwner::Member(..) => match body(db, file, owner).kind {
            BodyKind::Constructor => later("a constructor", CLASSES),
            BodyKind::Destructor => later("a destructor", DESTRUCTORS),
            BodyKind::Operator => later("an operator", OPERATORS),
            BodyKind::Get | BodyKind::Set => later("an indexer", OPERATORS),
            BodyKind::Function | BodyKind::Getter => {
                later("an explicit interface implementation", INTERFACES)
            }
            BodyKind::TopLevel | BodyKind::Initializer | BodyKind::Extension | BodyKind::Case => {
                None
            }
        },
        BodyOwner::Extension(_, 0) => later("an extension block", SMALLER_EXPRESSIONS),
        BodyOwner::Extension(..) => later("an extension member", SMALLER_EXPRESSIONS),
    }
}

/// The milestone that lowers a function with the signature `function`,
/// `None` when this one does: generic and async functions come later.
fn function_later(function: &FnSignature) -> Option<Later> {
    latest([
        function
            .is_generic
            .then(|| Later::new("a generic function", MONOMORPHIZATION)),
        function
            .is_async
            .then(|| Later::new("an async function", ASYNC)),
    ])
}

/// The milestone that lowers the members of a type of the kind `kind`,
/// `None` for a struct, whose members this one lowers, and for what is
/// not a type.
fn parent_later(kind: ItemKind) -> Option<Kind> {
    let (what, milestone) = match kind {
        ItemKind::Class => ("a class", CLASSES),
        ItemKind::Union => ("a union", UNIONS),
        ItemKind::Enum => ("an enum", ENUMS),
        ItemKind::Interface => ("an interface", INTERFACES),
        ItemKind::Struct | ItemKind::Function | ItemKind::Macro | ItemKind::Field => return None,
    };
    Some(Kind { what, milestone })
}

/// The milestone that lowers a function with the signature `function` for
/// the types of its parameters and its return type, `None` when this one
/// does.
fn signature_later(function: &FnSignature) -> Option<Later> {
    let types = function.params.iter().map(|param| &param.ty);
    latest(types.chain([&function.ret]).map(type_later))?
        .later(|what| format!("a signature with {what}"))
}

/// The milestone that lowers the local functions of the body of `owner`,
/// generic and async ones and those with a type in their signature that
/// comes later; the first in the order of the text names it on a tie.
/// `None` when this one lowers them all.
fn local_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let functions = local_functions(db, project, file, owner);
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    let resolution = body_resolution(db, project, file, owner);
    latest(functions.iter().map(|function| {
        let Stmt::Function(def) = &hir.stmts[function.stmt] else {
            return None;
        };
        let name = &hir.locals[def.local].name;
        let ret = resolution.return_type(def.ret);
        let types = def
            .params
            .iter()
            .filter_map(|&param| inference.local(param));
        let signature = latest(types.chain([&ret]).map(type_later)).and_then(|kind| {
            kind.later(|what| format!("the local function `{name}` with {what} in its signature"))
        });
        latest([
            def.is_generic.then(|| {
                Later::new(
                    format!("the generic local function `{name}`"),
                    MONOMORPHIZATION,
                )
            }),
            def.is_async
                .then(|| Later::new(format!("the async local function `{name}`"), ASYNC)),
            signature,
        ])
    }))
}

/// The milestone that lowers a body with the type `ty` in its signature,
/// `None` when this one does. A type inside another one decides when its
/// milestone is later, as for `B<int>[]`, an array of a generic type.
fn type_later(ty: &Ty) -> Option<Kind> {
    let ty = match ty {
        Ty::Primitive(_) | Ty::String | Ty::Struct(_) | Ty::Never | Ty::Error => return None,
        Ty::Tuple(elems) => return latest(elems.iter().map(|elem| type_later(&elem.ty))),
        Ty::Unsupported(ty) => ty,
    };
    let (what, milestone) = match ty.construct {
        Construct::Array => ("an array type", CONSTANT_EVALUATION),
        Construct::External => ("a type of the standard library", STD_BOOTSTRAP),
        Construct::Class => ("a class type", CLASSES),
        Construct::Weak => ("a weak reference type", WEAK_REFERENCES),
        Construct::Function => ("a function type", CLOSURES),
        Construct::Union => ("a union type", UNIONS),
        Construct::Enum => ("an enum type", ENUMS),
        Construct::Optional => ("an option type", OPTIONS),
        Construct::TypeParam => ("a type parameter", MONOMORPHIZATION),
        Construct::Generic => ("a generic type", MONOMORPHIZATION),
        Construct::Interface => ("an interface type", INTERFACES),
        Construct::Any => ("an interface value type", DYNAMIC_INTERFACES),
        Construct::Splice => ("a spliced type", EXPANSION),
    };
    let own = Some(Kind { what, milestone });
    latest(iter::once(own).chain(ty.parts.iter().map(type_later)))
}
