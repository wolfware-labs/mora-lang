//! The names of a body that are not locals (§14.5).

use std::collections::HashMap;

use mora_span::{Db, Project, SourceFile};

use super::{BodyOwner, CaptureTarget, Expr, ExprId, ScopeId, TypeId, body};
use crate::arena::arena_index;
use crate::defs::ItemId;
use crate::items::{ItemKind, ValueMember, Visibility, item_tree, signature_refs};
use crate::resolve::{
    ExprTarget, ResolveErrorKind, Resolved, Scope, effective_visibility, item_data,
    resolve_expr_path, resolve_static_member,
};
use crate::signature::{ResolveTypes, SignatureError, type_params};
use crate::ty::Ty;

/// What the paths and types of a body name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyResolution {
    /// What each [`Expr::Path`] names, and each [`Expr::Member`] whose
    /// receiver names a namespace or a type.
    pub paths: HashMap<ExprId, PathResolution>,
    /// Each of [`super::Body::types`], with its names looked up.
    pub types: Vec<Ty>,
    /// What does not resolve.
    pub(crate) errors: Vec<BodyError>,
}

/// What a path, or a member of a namespace or a type, names. For a path,
/// the segments after the first `segments` are member accesses on the
/// value, which type checking looks up; a member counts as one segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathResolution {
    /// What the first `segments` segments name.
    pub target: Target,
    /// How many segments name the target.
    pub segments: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A namespace, a type, a function or overloads, or a constant or
    /// static at namespace level.
    Item(Resolved),
    /// A value member of a type.
    Value(ValueRef),
    /// The path does not resolve; a diagnostic says why.
    Error,
}

/// A value member: its type and its position in [`crate::Item::values`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValueRef {
    /// The type that declares it.
    pub ty: ItemId,
    /// Its position among the type's value members.
    pub index: usize,
}

impl BodyResolution {
    /// The type written at `ty`, with its names looked up.
    pub fn written(&self, ty: TypeId) -> Ty {
        self.types
            .get(arena_index(ty))
            .cloned()
            .unwrap_or(Ty::Error)
    }

    /// The return type written at `ret`, of a local function or of a body
    /// that holds its own ([`crate::Body::ret`]): `()` when none is written.
    pub fn return_type(&self, ret: Option<TypeId>) -> Ty {
        ret.map_or(Ty::UNIT, |ty| self.written(ty))
    }

    /// Whether the path `path` names something of the standard library
    /// while the library is not loaded ([`Resolved::External`]).
    pub fn is_external(&self, path: ExprId) -> bool {
        self.paths
            .get(&path)
            .is_some_and(|resolution| matches!(resolution.target, Target::Item(Resolved::External)))
    }

    /// The value member the path `path` names, when it names one.
    pub fn value_member<'db>(&self, db: &'db dyn Db, path: ExprId) -> Option<&'db ValueMember> {
        self.paths.get(&path)?.member(db)
    }

    /// The value member the path `path` names, with the type that declares
    /// it, when it names one.
    pub(crate) fn value_ref(&self, path: ExprId) -> Option<ValueRef> {
        self.paths.get(&path)?.value_ref()
    }
}

impl PathResolution {
    /// The value member the path names, when it names one.
    pub fn member<'db>(&self, db: &'db dyn Db) -> Option<&'db ValueMember> {
        self.value_ref()?.member(db)
    }

    /// The value member the path names, with the type that declares it,
    /// when it names one.
    pub(crate) fn value_ref(&self) -> Option<ValueRef> {
        match self.target {
            Target::Value(value) => Some(value),
            Target::Item(_) | Target::Error => None,
        }
    }
}

impl ValueRef {
    /// The value member it refers to.
    pub fn member(self, db: &dyn Db) -> Option<&ValueMember> {
        item_data(db, self.ty).values.get(self.index)
    }
}

/// A name of a body that does not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// A segment of the [`Expr::Path`] `expr`, used in `scope` (`MR0127`,
    /// `MR0128`, `MR0129`).
    Path {
        expr: ExprId,
        scope: ScopeId,
        segment: usize,
        name: String,
        error: ResolveErrorKind,
    },
    /// The member `expr` of a namespace or a type (`MR0127`, `MR0128`,
    /// `MR0129`), looked up in `within`.
    Member {
        expr: ExprId,
        name: String,
        within: Resolved,
        error: ResolveErrorKind,
    },
    /// A path in a type written in the body, by its index among the body's
    /// type paths.
    Type(SignatureError),
    /// A capture of the lambda `expr`, by its position, that names no
    /// local (`MR0127`).
    Capture {
        expr: ExprId,
        index: usize,
        name: String,
    },
}

impl From<ExprTarget> for Target {
    fn from(target: ExprTarget) -> Self {
        match target {
            ExprTarget::Item(item) => Target::Item(item),
            ExprTarget::Value(ty, index) => Target::Value(ValueRef { ty, index }),
        }
    }
}

/// Where the names of `owner`'s body are looked up from.
pub(crate) fn body_scope(project: Project, file: SourceFile, owner: BodyOwner) -> Scope {
    Scope {
        project,
        file,
        within: match owner {
            BodyOwner::TopLevel | BodyOwner::Extension(..) => None,
            BodyOwner::Item(item) | BodyOwner::Value(item, _) | BodyOwner::Member(item, _) => {
                Some(item)
            }
        },
    }
}

/// How far a declaration whose types a body checks is visible (§4.5): its
/// written visibility, narrowed by the types around it, which `scope` is
/// inside.
fn exposed_visibility(db: &dyn Db, scope: Scope, written: Visibility) -> Visibility {
    match scope.within {
        Some(ty) => written.min(effective_visibility(db, scope.item(ty))),
        // At namespace level, `private` means the declaring file; no type is
        // narrower than that there, so it checks the same as `private`.
        None => written,
    }
}

/// Looks up the paths and types of a body. It reads no text range, so an
/// edit in another body leaves it equal.
#[salsa::tracked(returns(ref))]
pub fn body_resolution(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> BodyResolution {
    let body = body(db, file, owner);
    let scope = body_scope(project, file, owner);
    let mut resolution = BodyResolution::default();
    // A receiver comes before its member in the arena, so it is looked up
    // first.
    // Every path of a body is looked up from the same scope, so a name used
    // again gives the same answer.
    let mut cache = HashMap::new();
    for (id, expr) in body.exprs.iter() {
        match expr {
            Expr::Path(path) => {
                let found = cache
                    .entry(&path.segments[..])
                    .or_insert_with(|| resolve_expr_path(db, scope, &path.segments))
                    .clone();
                let (target, segments) = match found {
                    Ok((target, segments)) => (target.into(), segments),
                    Err(error) => {
                        resolution.errors.push(BodyError::Path {
                            expr: id,
                            scope: path.scope,
                            segment: error.segment,
                            name: path.segments[error.segment].clone(),
                            error: error.kind,
                        });
                        (Target::Error, 0)
                    }
                };
                resolution
                    .paths
                    .insert(id, PathResolution { target, segments });
            }
            Expr::Member { receiver, name, .. } => {
                let Some(PathResolution {
                    target: Target::Item(within),
                    ..
                }) = resolution.paths.get(receiver)
                else {
                    continue;
                };
                // A member of a value or of a function is looked up by type
                // checking.
                let is_static = match within {
                    Resolved::Namespace(_) | Resolved::External => true,
                    Resolved::Item(item) => !matches!(
                        item_data(db, *item).kind,
                        ItemKind::Function | ItemKind::Field | ItemKind::Macro
                    ),
                    Resolved::Functions(_) => false,
                };
                if !is_static {
                    continue;
                }
                let target = match resolve_static_member(db, scope, within, name) {
                    Ok(target) => target.into(),
                    Err(error) => {
                        resolution.errors.push(BodyError::Member {
                            expr: id,
                            name: name.clone(),
                            within: within.clone(),
                            error,
                        });
                        Target::Error
                    }
                };
                resolution.paths.insert(
                    id,
                    PathResolution {
                        target,
                        segments: 1,
                    },
                );
            }
            Expr::Lambda(lambda) => {
                for (index, capture) in lambda.captures.iter().enumerate() {
                    if let CaptureTarget::Unresolved(name) = &capture.target {
                        resolution.errors.push(BodyError::Capture {
                            expr: id,
                            index,
                            name: name.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let mut lower = ResolveTypes {
        db,
        scope,
        type_params: type_params(item_tree(db, file), signature_refs(db, file), scope.within),
        field: None,
        // Set per type below.
        member: Visibility::Private,
        errors: Vec::new(),
    };
    // A declaration's signature types are checked against how far it is
    // visible; any other type of a body is visible to no other code, so
    // no type is less visible than it.
    let exposed = exposed_visibility(db, scope, body.visibility);
    let outer = lower.type_params.len();
    resolution.types = body
        .types
        .iter()
        .map(|(id, ty)| {
            lower.type_params.truncate(outer);
            lower.member = if body.signature_types.contains(&id) {
                exposed
            } else {
                Visibility::Private
            };
            if let Some(own) = body.type_params.get(&id) {
                lower.type_params.extend(own.iter().cloned());
            }
            lower.ty(ty)
        })
        .collect();
    resolution
        .errors
        .extend(lower.errors.into_iter().map(BodyError::Type));
    resolution
}
