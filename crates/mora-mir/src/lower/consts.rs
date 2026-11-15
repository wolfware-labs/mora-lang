//! The default values of parameters, each lowered as a body of its own
//! that constant evaluation runs (§14.6). The initializer of a constant is
//! the body of its owner, which [`super::mir`] lowers.

use mora_hir::{BodyOwner, ExprId, Inference, Ty};
use mora_span::{Db, Project, SourceFile};

use super::{BodyShape, Lowerer, Owner, std_loaded};
use crate::{Mir, debug_validate};

/// The type a default value is passed as: the conversion inference records
/// on it, or its own type.
pub(crate) fn default_ty(inference: &Inference, default: ExprId) -> Option<Ty> {
    match inference.conversion(default) {
        Some(conversion) => Some(conversion.to.clone()),
        None => inference.expr(default).cloned(),
    }
}

/// The MIR of the default value `default` of a parameter declared in the
/// body of `owner`: one body without parameters that returns it, converted
/// to [`default_ty`]. Named `<owner name>: a default value`. It is checked
/// by [`debug_validate`] before it is returned.
///
/// # Panics
///
/// When the default value holds what lowering cannot handle, as
/// [`super::mir`] does.
#[salsa::tracked(returns(ref))]
pub(crate) fn default_mir(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
    default: ExprId,
) -> Mir {
    let shared = Owner::new(db, project, file, owner);
    let ret = shared.expect(
        default_ty(shared.inference, default),
        "the default value has no type",
    );
    let shape = BodyShape {
        name: format!("{}: a default value", shared.name),
        ret,
        this: None,
        params: Vec::new(),
        root: default,
    };
    let mir = Mir {
        bodies: vec![Lowerer::lower(&shared, &shape)],
    };
    debug_validate(db, &mir, std_loaded(db, project));
    mir
}
