//! The bodies that a later milestone of the roadmap lowers. [`mir`] stops
//! on one with an internal error that names the milestone, and
//! [`mir_dump`] writes a line in its place.
//!
//! A body comes later for what it is, as a getter; for a type it
//! declares, as a class type among its parameters or the type of a field;
//! for the type of one of its locals or expressions; for its `this`, of a
//! generic type, as in a method of `struct B<T>` or of a struct nested in
//! it, or of a struct whose fields come later; or for a local function in
//! it that comes later, since a local function is lowered with the body it
//! is declared in; or for a struct it creates with a constructor, declared
//! or the one of a positional struct; or for an operator it applies to a
//! struct, which may declare it; or for a lambda in it, or a call that
//! inference leaves unresolved because a generic, `async` or `params`
//! function could take it. A struct type comes as late as the
//! types of its instance fields, which its layout holds, and a struct
//! nested in a generic type comes with generic code. When several
//! milestones apply, the latest of them lowers the body.
//!
//! [`mir`]: super::mir
//! [`mir_dump`]: super::mir_dump

use std::collections::HashSet;
use std::fmt;

use mora_hir::{
    BinaryOp, Body, BodyKind, BodyOwner, Construct, Creation, Expr, ExprId, FnSignature, Inference,
    ItemId, ItemKind, Resolved, Signature, Stmt, Target, Ty, UnaryOp, ValueKind, body,
    body_resolution, creation, enclosing_types, infer, instance_fields, is_generic_type, item_tree,
    local_functions, signature, struct_field,
};
use mora_span::{Db, Project, SourceFile};

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
/// M2.2, positional structs.
const POSITIONAL_STRUCTS: Milestone = milestone(2, 2);
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
/// The milestone that expands `params` arguments. The roadmap has none
/// yet, so a call that only a `params` overload leaves unresolved stays an
/// internal error of lowering.
const PARAMS: Option<Milestone> = None;

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
        local_functions_later(db, project, file, owner),
        this_later(db, project, file, owner),
        body_types_later(db, project, file, owner),
        creations_later(db, project, file, owner),
        operators_later(db, project, file, owner),
        lambdas_later(db, file, owner),
        calls_later(db, project, file, owner),
    ])
}

/// The milestone that lowers the body of `owner` for a lambda in it:
/// closures and function types. `None` when it has none.
fn lambdas_later(db: &dyn Db, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let hir = body(db, file, owner);
    hir.exprs
        .iter()
        .any(|(_, expr)| matches!(expr, Expr::Lambda(_)))
        .then(|| Later::new("a lambda", CLOSURES))
}

/// The milestone that lowers the body of `owner` for the calls inference
/// leaves without a callee because an overload of the called name is
/// generic, `async` or takes `params` (D-137): the latest of those that
/// apply to each call, the first call on a tie. A call of a local function
/// comes as late as the local function, which [`local_functions_later`]
/// counts, and a call with none of these overloads is an internal error
/// of lowering. `None` when every call has its callee.
fn calls_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    let resolution = body_resolution(db, project, file, owner);
    latest(hir.exprs.iter().map(|(id, expr)| {
        let Expr::Call { callee, .. } = expr else {
            return None;
        };
        if inference.callee(id).is_some() {
            return None;
        }
        let functions = match resolution.paths.get(callee).map(|path| &path.target) {
            Some(Target::Item(Resolved::Functions(items))) => items.clone(),
            _ => methods(db, hir, inference, *callee),
        };
        latest(functions.iter().map(|&item| {
            let Signature::Function(function) = signature(db, item) else {
                return None;
            };
            unchecked_call(function)
        }))
    }))
}

/// The non-static methods that a call through a value of a struct type
/// calls by the name of `callee`, a member; empty for any other callee.
fn methods(db: &dyn Db, hir: &Body, inference: &Inference, callee: ExprId) -> Vec<ItemId> {
    let Expr::Member { receiver, name, .. } = &hir.exprs[callee] else {
        return Vec::new();
    };
    let Some(Ty::Struct(ty)) = inference.expr(*receiver) else {
        return Vec::new();
    };
    let tree = item_tree(db, ty.file);
    tree.item(ty.item)
        .members
        .iter()
        .filter(|&&member| {
            let data = tree.item(member);
            data.kind == ItemKind::Function && data.name == *name && !data.is_static
        })
        .map(|&member| ItemId {
            item: member,
            ..*ty
        })
        .collect()
}

/// The milestone that checks and lowers a call that the function with the
/// signature `function` could take, `None` when that function leaves a
/// call unresolved for none of these reasons: a generic one comes with
/// generic code, an `async` one with async lowering, and one with a
/// `params` parameter with [`PARAMS`].
fn unchecked_call(function: &FnSignature) -> Option<Later> {
    let params = function
        .params
        .iter()
        .any(|param| param.is_params)
        .then_some(PARAMS)
        .flatten()
        .map(|milestone| Later::new("a call of a function with `params`", milestone));
    latest([
        function
            .is_generic
            .then(|| Later::new("a call of a generic function", MONOMORPHIZATION)),
        function
            .is_async
            .then(|| Later::new("a call of an async function", ASYNC)),
        params,
    ])
}

/// The milestone that lowers the body of `owner` for an operator it
/// applies to a struct, which a struct may declare (§6.9) and which
/// operators and conversions look up: arithmetic, bitwise and shift
/// operators, compound assignments, `!`, unary `+`, `-` and `~`, `++`
/// and `--`, and an indexer. Comparisons, `&&`, `||` and `??` are not counted. `None` when
/// it applies none.
fn operators_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    let on_struct = |expr| matches!(inference.expr(expr), Some(Ty::Struct(_)));
    hir.exprs
        .iter()
        .any(|(_, expr)| match expr {
            Expr::Binary { op, lhs, rhs }
            | Expr::Assign {
                op: Some(op),
                lhs,
                rhs,
            } => declarable(*op) && (on_struct(*lhs) || on_struct(*rhs)),
            Expr::Unary { op, expr } => *op != UnaryOp::FromEnd && on_struct(*expr),
            Expr::Postfix { expr, .. } => on_struct(*expr),
            Expr::Index { base, .. } => on_struct(*base),
            _ => false,
        })
        .then(|| Later::new("an operator on a struct", OPERATORS))
}

/// Whether a struct may declare the binary operator `op` (§6.9).
fn declarable(op: BinaryOp) -> bool {
    !matches!(
        op,
        BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Le
            | BinaryOp::Ge
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Coalesce
    )
}

/// The milestone that lowers the body of `owner` for the structs it
/// creates with a constructor: a call of a declared constructor comes with
/// constructors, and the creation of a positional struct with positional
/// structs. `None` when it creates every struct memberwise. The first in
/// the order of the body names it on a tie.
fn creations_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    latest(hir.exprs.iter().map(|(id, expr)| {
        let Expr::New { args, .. } = expr else {
            return None;
        };
        let Some(Ty::Struct(item)) = inference.expr(id) else {
            return None;
        };
        match creation(db, *item, args.as_deref()) {
            Creation::Constructor => later("a constructor call", CLASSES),
            Creation::Positional => {
                later("the creation of a positional struct", POSITIONAL_STRUCTS)
            }
            Creation::Memberwise | Creation::MemberwiseOff | Creation::NoConstructor => None,
        }
    }))
}

/// The milestone that lowers the body of `owner` for its `this`: a method,
/// a getter or a field initializer that is not `static`, a constructor, a
/// destructor, an indexer or an interface implementation. Generic code
/// when it is in a type with type parameters or nested in one; in a struct,
/// as late as the struct's instance fields, which `this` holds. `None` for
/// any other body: a static member of a generic type is lowered for what it
/// is, and a type parameter in its signature comes later for that type. A
/// `static` interface implementation is counted too, as it comes later
/// still for what it is.
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
            let kind = body(db, file, owner).kind;
            // An operator is static: it takes no `this`.
            if kind == BodyKind::Operator {
                return None;
            }
            (ty, member_later(kind)?.0)
        }
        BodyOwner::TopLevel | BodyOwner::Extension(..) => return None,
    };
    let ty = id(ty);
    let generic = in_generic_type(db, ty)
        .then(|| later(format!("{what} of a generic type"), MONOMORPHIZATION))
        .flatten();
    // The layout of a struct's `this` can hold a type that comes even
    // later than monomorphization, so both count.
    let layout = (tree.item(ty.item).kind == ItemKind::Struct)
        .then(|| {
            TypeWalk::new(db)
                .later(&Ty::Struct(ty))?
                .later(|kind| format!("{what} whose `this` holds {kind}"))
        })
        .flatten();
    latest([generic, layout])
}

/// Whether the type `item` declares type parameters or is nested in a type
/// that does, whose type parameters it can use: its layout and its members
/// are generic code either way.
fn in_generic_type(db: &dyn Db, item: ItemId) -> bool {
    enclosing_types(item_tree(db, item.file), Some(item.item))
        .any(|idx| is_generic_type(db, ItemId { item: idx, ..item }))
}

/// The milestone that lowers the body of `owner` for what it is or for a
/// type it declares, `None` when this one does.
fn owner_later(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Option<Later> {
    let tree = item_tree(db, file);
    let id = |item| ItemId {
        project,
        file,
        item,
    };
    let declared = || declared_types(db, project, file, owner);
    let (what, own, types) = match owner {
        BodyOwner::TopLevel => return None,
        BodyOwner::Item(item) => {
            let data = tree.item(item);
            let sig = signature(db, id(item));
            let (what, own) = match sig {
                Signature::Function(function) => {
                    let method = function
                        .receiver
                        .and(data.parent)
                        .and_then(|parent| parent_later(tree.item(parent).kind))
                        .and_then(|kind| kind.later(|what| format!("a method of {what}")));
                    return latest([
                        function_later(function),
                        method,
                        signature_later(db, function),
                    ]);
                }
                // Besides functions, only a constant or a static at
                // namespace level owns an item body.
                _ if data.is_static => ("the initializer of a static".to_owned(), STATICS),
                _ => (
                    "the initializer of a constant".to_owned(),
                    CONSTANT_EVALUATION,
                ),
            };
            let types = match sig {
                Signature::Value(ty) => vec![ty.clone()],
                _ => Vec::new(),
            };
            (what, Some(own), types)
        }
        BodyOwner::Value(ty, index) => {
            let data = tree.item(ty);
            let value = data.values.get(index as usize)?;
            let (what, own) = match value.kind {
                ValueKind::Field if value.is_static => (
                    "the initializer of a static field".to_owned(),
                    Some(STATICS),
                ),
                ValueKind::Field => match parent_later(data.kind) {
                    Some(parent) => (
                        format!("the initializer of a field of {}", parent.what),
                        Some(parent.milestone),
                    ),
                    None => ("the initializer of a field".to_owned(), None),
                },
                ValueKind::Const => (
                    "the initializer of a constant".to_owned(),
                    Some(CONSTANT_EVALUATION),
                ),
                ValueKind::Getter => match parent_later(data.kind) {
                    Some(parent) if data.kind != ItemKind::Class => (
                        format!("a getter of {}", parent.what),
                        Some(parent.milestone),
                    ),
                    _ => ("a getter".to_owned(), Some(CLASSES)),
                },
                ValueKind::EnumMember => ("the value of an enum member".to_owned(), Some(ENUMS)),
                ValueKind::UnionCase => ("a union case".to_owned(), Some(UNIONS)),
            };
            // A struct's fields are in its signature, a getter's type and
            // any other type's fields with the body.
            let field = struct_field(db, id(ty), index).map(|(_, field)| field.ty.clone());
            (what, own, field.into_iter().chain(declared()).collect())
        }
        BodyOwner::Member(..) => {
            let (what, own) = member_later(body(db, file, owner).kind)?;
            (what.to_owned(), Some(own), declared())
        }
        BodyOwner::Extension(_, 0) => (
            "an extension block".to_owned(),
            Some(SMALLER_EXPRESSIONS),
            declared(),
        ),
        BodyOwner::Extension(..) => (
            "an extension member".to_owned(),
            Some(SMALLER_EXPRESSIONS),
            declared(),
        ),
    };
    let typed =
        types_later(db, &types).and_then(|kind| kind.later(|ty| format!("{what} with {ty}")));
    latest([own.map(|own| Later::new(what, own)), typed])
}

/// What a member body of the kind `kind` is, as `a constructor`, and the
/// milestone that lowers it; `None` for a kind that is not a member's.
fn member_later(kind: BodyKind) -> Option<(&'static str, Milestone)> {
    match kind {
        BodyKind::Constructor => Some(("a constructor", CLASSES)),
        BodyKind::Destructor => Some(("a destructor", DESTRUCTORS)),
        BodyKind::Operator => Some(("an operator", OPERATORS)),
        BodyKind::Get | BodyKind::Set => Some(("an indexer", OPERATORS)),
        BodyKind::Function | BodyKind::Getter => {
            Some(("an explicit interface implementation", INTERFACES))
        }
        BodyKind::TopLevel | BodyKind::Initializer | BodyKind::Extension | BodyKind::Case => None,
    }
}

/// The parameter, return, getter and field types that the body of `owner`
/// declares and checks itself, because the item tree does not hold them.
/// An accessor of an indexer after the first declares none: the first
/// one checks the indexer's types, so they are taken from it.
fn declared_types(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Vec<Ty> {
    let mut owner = owner;
    let mut hir = body(db, file, owner);
    while let BodyOwner::Member(ty, member @ 1..) = owner
        && matches!(hir.kind, BodyKind::Get | BodyKind::Set)
        && hir.signature_types.is_empty()
    {
        owner = BodyOwner::Member(ty, member - 1);
        hir = body(db, file, owner);
    }
    let resolution = body_resolution(db, project, file, owner);
    hir.signature_types
        .iter()
        .map(|&ty| resolution.written(ty))
        .collect()
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
fn signature_later(db: &dyn Db, function: &FnSignature) -> Option<Later> {
    let types = function.params.iter().map(|param| &param.ty);
    types_later(db, types.chain([&function.ret]))?.later(|what| format!("a signature with {what}"))
}

/// The milestone that lowers the local functions of the body of `owner`,
/// generic and async ones and those with a type in their signature that
/// comes later; the first in the order of the text names it on a tie.
/// `None` when this one lowers them all.
fn local_functions_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
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
        let signature = types_later(db, types.chain([&ret])).and_then(|kind| {
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

/// The milestone that lowers the body of `owner` for the types of its
/// locals and of its expressions, temporaries included; `None` when this
/// one does. On a tie the locals name it before the expressions, each in
/// the order of the body. The name of a local function has no type:
/// [`local_functions_later`] looks into its signature.
fn body_types_later(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Later> {
    let hir = body(db, file, owner);
    let inference = infer(db, project, file, owner);
    let mut walk = TypeWalk::new(db);
    let locals = latest(hir.locals.iter().map(|(id, local)| {
        walk.later(inference.local(id)?)?
            .later(|what| format!("the local `{}` with {what}", local.name))
    }));
    let exprs = latest(hir.exprs.iter().map(|(id, _)| {
        walk.later(inference.expr(id)?)?
            .later(|what| format!("an expression with {what}"))
    }));
    latest([locals, exprs])
}

/// The milestone that lowers a body with `types` in it, `None` when this
/// one does; the first of them names it on a tie.
fn types_later<'a>(db: &'a dyn Db, types: impl IntoIterator<Item = &'a Ty>) -> Option<Kind> {
    let mut walk = TypeWalk::new(db);
    latest(types.into_iter().map(|ty| walk.later(ty)))
}

/// A walk over types for the milestone that lowers them. A type inside
/// another one decides when its milestone is later, as for `B<int>[]`, an
/// array of a generic type, and so do the types of a struct's instance
/// fields, which its layout holds. A struct nested in a generic type is
/// generic code. It keeps a stack rather than recursing, since a chain of
/// structs that each hold the next is as long as the program makes it.
struct TypeWalk<'a> {
    /// The database the fields of structs come from.
    db: &'a dyn Db,
    /// The structs already looked into, so that each is looked into once,
    /// one whose fields lead back to it included.
    seen: HashSet<ItemId>,
    /// The types still to look into, the next one last.
    stack: Vec<&'a Ty>,
}

impl<'a> TypeWalk<'a> {
    /// A walk that has looked into no struct yet.
    fn new(db: &'a dyn Db) -> Self {
        Self {
            db,
            seen: HashSet::new(),
            stack: Vec::new(),
        }
    }

    /// The milestone that lowers a body with the type `ty` in it, `None`
    /// when this one does or when every struct in it was looked into
    /// before. On a tie the outer type names it, then the first part.
    fn later(&mut self, ty: &'a Ty) -> Option<Kind> {
        let mut found = None;
        self.stack.push(ty);
        while let Some(ty) = self.stack.pop() {
            let start = self.stack.len();
            match ty {
                Ty::Primitive(_) | Ty::String | Ty::Never | Ty::Error => {}
                Ty::Struct(item) => {
                    if self.seen.insert(*item) {
                        if in_generic_type(self.db, *item) {
                            let generic = construct_later(Construct::Generic);
                            found = latest([found, Some(generic)]);
                        }
                        let fields = instance_fields(self.db, *item);
                        self.stack.extend(fields.map(|(_, field)| &field.ty));
                    }
                }
                Ty::Tuple(elems) => self.stack.extend(elems.iter().map(|elem| &elem.ty)),
                Ty::Unsupported(ty) => {
                    found = latest([found, Some(construct_later(ty.construct))]);
                    self.stack.extend(&ty.parts);
                }
            }
            // Parts are pushed in order and taken from the end: reversed,
            // the first of them is looked into first.
            self.stack[start..].reverse();
        }
        found
    }
}

/// The milestone that lowers a type made with `construct`.
fn construct_later(construct: Construct) -> Kind {
    let milestone = match construct {
        Construct::Array => CONSTANT_EVALUATION,
        Construct::External => STD_BOOTSTRAP,
        Construct::Class => CLASSES,
        Construct::Weak => WEAK_REFERENCES,
        Construct::Function => CLOSURES,
        Construct::Union => UNIONS,
        Construct::Enum => ENUMS,
        Construct::Optional => OPTIONS,
        Construct::TypeParam | Construct::Generic => MONOMORPHIZATION,
        Construct::Interface => INTERFACES,
        Construct::Any => DYNAMIC_INTERFACES,
        Construct::Splice => EXPANSION,
    };
    Kind {
        what: construct.description(),
        milestone,
    }
}
