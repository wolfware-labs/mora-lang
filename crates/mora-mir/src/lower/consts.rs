//! The default values of parameters, each lowered as a body of its own
//! that constant evaluation runs (§14.6). The initializer of a constant is
//! the body of its owner, which [`super::mir`] lowers.

use mora_hir::{
    BodyOwner, ExprId, Inference, ItemId, Signature, Ty, instance_fields, signature, struct_field,
};
use mora_span::{Db, Project, SourceFile, Span};

use super::{BodyShape, Lowerer, Owner, fail_in, std_loaded};
use crate::eval::{ConstKey, ConstValue, Evaluated, const_value};
use crate::{LocalTy, Mir, Operand, Place, Rvalue, debug_validate};

/// The type a default value is passed as: the conversion inference records
/// on it, or its own type.
pub(crate) fn default_ty(inference: &Inference, default: ExprId) -> Option<Ty> {
    match inference.conversion(default) {
        Some(conversion) => Some(conversion.to.clone()),
        None => inference.expr(default).cloned(),
    }
}

/// The declared type of the constant `owner` of `file`, a constant at
/// namespace level or a member of a type; `None` for another owner.
pub(crate) fn const_ty(
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
        BodyOwner::Item(index) => match signature(db, item(index)) {
            Signature::Value(ty) => Some(ty.clone()),
            _ => None,
        },
        BodyOwner::Value(ty, index) => {
            struct_field(db, item(ty), index).map(|(_, field)| field.ty.clone())
        }
        BodyOwner::TopLevel | BodyOwner::Member(..) | BodyOwner::Extension(..) => None,
    }
}

/// The MIR of the default value `default` of a parameter declared in the
/// body of `owner`: one body without parameters that returns it, converted
/// to [`default_ty`]. Named `<owner name>: a default value`. It is checked
/// by [`debug_validate`] before it is returned.
///
/// Unlike [`super::mir`], it does not check that this milestone lowers the
/// default value: its caller does, with
/// [`exprs_later`](super::later::exprs_later) over the expressions of the
/// default, before it asks for it.
///
/// # Panics
///
/// When the default value holds what lowering cannot handle, as
/// [`super::mir`] does, a default value that a later milestone lowers
/// among them.
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
        let types: Vec<Ty> = match ty {
            Ty::Struct(item) => instance_fields(self.owner.db, *item)
                .map(|(_, field)| field.ty.clone())
                .collect(),
            Ty::Tuple(elems) => elems.iter().map(|elem| elem.ty.clone()).collect(),
            _ => Vec::new(),
        };
        if types.len() != parts.len() {
            fail_in(&self.name, "a constant value whose shape is not its type's");
        }
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
