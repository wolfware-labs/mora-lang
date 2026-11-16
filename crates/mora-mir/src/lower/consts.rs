//! The bodies that constant evaluation runs (§14.6), lowered with each
//! read of a constant left for the interpreter: the initializer of a
//! constant or of a field, and the default value of a parameter, lowered
//! as a body of its own. And the values that evaluation computes, as the
//! operands of the bodies that read them.

use mora_hir::{
    BodyOwner, ExprId, Inference, ItemId, Ty, infer, instance_fields, signature, struct_field,
};
use mora_span::{Db, Project, SourceFile, Span};

use super::{BodyShape, Lowerer, Owner, std_loaded};
use crate::eval::{ConstKey, ConstValue, Evaluated, const_value};
use crate::validate::debug_validate_evaluation;
use crate::{LocalTy, Mir, Operand, Place, Rvalue};

/// The type a default value is passed as: the conversion inference records
/// on it, or its own type.
pub(crate) fn default_ty(inference: &Inference, default: ExprId) -> Option<Ty> {
    match inference.conversion(default) {
        Some(conversion) => Some(conversion.to.clone()),
        None => inference.expr(default).cloned(),
    }
}

/// The declared type of the value the body of `owner` of `file` computes:
/// a constant or a static at namespace level, or a field or a constant of
/// a type; `None` for another owner.
pub(crate) fn value_ty(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Option<Ty> {
    let item = |item| ItemId {
        project,
        file,
        item,
    };
    match owner {
        BodyOwner::Item(index) => signature(db, item(index)).as_value().cloned(),
        BodyOwner::Value(ty, index) => {
            struct_field(db, item(ty), index).map(|(_, field)| field.ty.clone())
        }
        BodyOwner::TopLevel | BodyOwner::Member(..) | BodyOwner::Extension(..) => None,
    }
}

/// The type of the value `key`, a key of `project`, gives: the declared
/// type of a constant or of a field, or the type a default value is passed
/// as ([`default_ty`]). `None` for a key without one.
pub(crate) fn key_ty(db: &dyn Db, project: Project, key: ConstKey) -> Option<Ty> {
    let (file, owner) = (key.file(), key.owner(db));
    match key {
        ConstKey::Constant { .. } | ConstKey::Initializer { .. } => {
            value_ty(db, project, file, owner)
        }
        ConstKey::Default { default, .. } => default_ty(infer(db, project, file, owner), default),
    }
}

/// The MIR that constant evaluation runs for `key`, a key of `project`:
/// for a constant or a field initializer, the body of its owner, as
/// [`super::mir`] lowers it; for a default value, one body without
/// parameters that returns it, converted to [`default_ty`], named
/// `<owner name>: a default value`. A read of another constant is an
/// [`Operand::ConstRead`], so the MIR does not depend on the values of the
/// constants it reads. It is checked by [`debug_validate_evaluation`]
/// before it is returned.
///
/// Unlike [`super::mir`], it does not check that this milestone lowers the
/// body: constant evaluation does, with
/// [`evaluation_later`](super::later::evaluation_later) or
/// [`exprs_later`](super::later::exprs_later), before it asks for it.
///
/// # Panics
///
/// When the body holds what lowering cannot handle, as [`super::mir`]
/// does, a body that a later milestone lowers among them.
#[salsa::tracked(returns(ref))]
pub(crate) fn eval_mir(db: &dyn Db, project: Project, key: ConstKey) -> Mir {
    let file = key.file();
    let owner = key.owner(db);
    let mut shared = Owner::new(db, project, file, owner);
    shared.evaluation = true;
    let shape = match key {
        ConstKey::Constant { .. } | ConstKey::Initializer { .. } => shared.shape(owner),
        ConstKey::Default { default, .. } => BodyShape {
            name: format!("{}: a default value", shared.name),
            ret: shared.expect(key_ty(db, project, key), "the default value has no type"),
            this: None,
            params: Vec::new(),
            root: default,
        },
    };
    let mir = Mir {
        bodies: vec![Lowerer::lower(&shared, &shape)],
    };
    debug_validate_evaluation(db, &mir, std_loaded(db, project));
    mir
}

impl Lowerer<'_> {
    /// The value of the constant `expr` reads, the key `key` of `project`:
    /// a constant operand, or a temporary holding an `Aggregate` of
    /// constant operands, as [`Lowerer::value_operand`] builds it.
    ///
    /// # Panics
    ///
    /// When the constant has no value: a body that reads one is never
    /// lowered, since its evaluation reports an error or waits for a later
    /// milestone first.
    pub(super) fn const_operand(
        &mut self,
        expr: ExprId,
        project: Project,
        key: ConstKey,
    ) -> Operand {
        let Evaluated::Value(value) = const_value(self.owner.db, project, key) else {
            self.ice(expr, "a read of a constant without a value");
        };
        let ty = self.ty(expr);
        self.value_operand(value, &ty, self.span(expr))
    }

    /// `value` of type `ty` as an operand: a scalar is a constant operand,
    /// and a struct or a tuple a temporary holding an `Aggregate` of the
    /// operands of its fields, those that are aggregates themselves in
    /// temporaries made first.
    ///
    /// # Panics
    ///
    /// When the shape of `value` is not that of `ty`, which only a compiler
    /// bug can cause.
    pub(super) fn value_operand(&mut self, value: &ConstValue, ty: &Ty, span: Span) -> Operand {
        let (kind, parts) = match value {
            ConstValue::Scalar(constant) => return Operand::Const(constant.clone()),
            ConstValue::Aggregate(kind, parts) => (*kind, parts),
        };
        let what = "a constant value whose shape is not its type's";
        let types: Vec<Ty> = match ty {
            Ty::Tuple(elems) => elems.iter().map(|elem| elem.ty.clone()).collect(),
            ty => {
                let item = self.expect_in(ty.as_struct(), what);
                let fields = instance_fields(self.owner.db, item);
                fields.map(|(_, field)| field.ty.clone()).collect()
            }
        };
        self.expect_in((types.len() == parts.len()).then_some(()), what);
        let mut operands = Vec::with_capacity(parts.len());
        for (part, ty) in parts.iter().zip(&types) {
            operands.push(self.value_operand(part, ty, span));
        }
        let temp = self.builder.temp(LocalTy::Value(ty.clone()), span);
        self.builder
            .assign(Place::local(temp), Rvalue::Aggregate(kind, operands), span);
        Operand::Copy(Place::local(temp))
    }
}
