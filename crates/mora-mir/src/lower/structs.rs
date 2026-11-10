//! Structs: memberwise creation, fields and the initializers of fields
//! (§6.2, §6.9).

use mora_hir::{
    Arg, BodyOwner, ExprId, Field, ItemId, Path, Signature, Target, Ty, body, instance_fields,
    item_tree, signature,
};
use mora_span::Db;

use super::expr::Dest;
use super::{Lowered, Lowerer};
use crate::{AggregateKind, Callee, Operand, Place, Projection, Rvalue};

impl Lowerer<'_> {
    /// `new T { F = v, ... }` for the struct `T`: every instance field in
    /// the order of the declaration. The written values are evaluated in
    /// the order of the text; a field left out then takes the value of its
    /// initializer, which is a body of its own.
    pub(super) fn new_struct(
        &mut self,
        expr: ExprId,
        args: Option<&[Arg]>,
        inits: Option<&[(String, ExprId)]>,
    ) -> Lowered<Rvalue> {
        if args.is_some() {
            self.ice(expr, "a constructor call is not lowered");
        }
        let Ty::Struct(item) = self.ty(expr) else {
            self.ice(
                expr,
                "the creation of a value that is not a struct is not lowered",
            )
        };
        let inits = inits.unwrap_or_default();
        let values: Vec<ExprId> = inits.iter().map(|&(_, value)| value).collect();
        let mut written: Vec<Option<Operand>> =
            self.operands(&values)?.into_iter().map(Some).collect();
        let mut operands = Vec::new();
        for (n, (index, field)) in instance_fields(self.owner.db, item).enumerate() {
            let init = inits.iter().position(|(name, _)| *name == field.name);
            operands.push(match init.and_then(|init| written[init].take()) {
                Some(operand) => operand,
                None => self.initializer_call(expr, item, (n, index), field)?,
            });
        }
        Ok(Rvalue::Aggregate(AggregateKind::Struct(item), operands))
    }

    /// The value of the initializer of `field`, which `expr` leaves out: a
    /// call of its body, into a temporary. `n` is the field's position
    /// among the instance fields of the struct `item`, and `index` its
    /// position among the value members.
    fn initializer_call(
        &mut self,
        expr: ExprId,
        item: ItemId,
        (n, index): (usize, u32),
        field: &Field,
    ) -> Lowered<Operand> {
        let owner = BodyOwner::Value(item.item, index);
        if body(self.owner.db, item.file, owner).root.is_none() {
            self.ice(expr, "a field without an initializer is left out");
        }
        let dest = self.finish_call(
            expr,
            Callee::Initializer(item, self.field_number(expr, n)),
            Vec::new(),
            field.ty.clone(),
            Dest::Temp,
        );
        match dest {
            Some(place) => Ok(Operand::Copy(place)),
            None => self.ice(expr, "the initializer has no result"),
        }
    }

    /// The place of the field `name` of the struct at `base`, which `expr`
    /// names. Static and constant members are not stored in a value, and
    /// lowering them is for the static state of M1.11.
    pub(super) fn field_place(&self, expr: ExprId, mut base: Place, name: &str) -> Place {
        let db = self.owner.db;
        let Ty::Struct(item) = self.builder.body().place_ty(&base, db) else {
            self.ice(
                expr,
                "a member that is not a field of a struct is not lowered",
            )
        };
        let Some(n) = instance_fields(db, item).position(|(_, field)| field.name == name) else {
            let declared = match signature(db, item) {
                Signature::Struct(fields) => fields.iter().any(|field| field.name == name),
                _ => false,
            };
            if declared {
                self.ice(expr, "a static or constant member is not lowered (M1.11)");
            }
            self.ice(
                expr,
                "a member that is not a field of a struct is not lowered",
            )
        };
        base.projections
            .push(Projection::Field(self.field_number(expr, n)));
        base
    }

    /// The position `n` of a field, as MIR holds it.
    fn field_number(&self, expr: ExprId, n: usize) -> u32 {
        match u32::try_from(n) {
            Ok(n) => n,
            Err(_) => self.ice(expr, "a struct with more than u32::MAX fields"),
        }
    }

    /// The place `path` names: a field of `this`, then the fields its
    /// other segments name.
    pub(super) fn path_place(&self, expr: ExprId, path: &Path) -> Place {
        let resolution = self.owner.resolution.paths.get(&expr);
        let field = resolution.and_then(|resolution| match resolution.target {
            Target::Value(value) => {
                let member = item_tree(self.owner.db, value.ty.file)
                    .item(value.ty.item)
                    .values
                    .get(value.index)?;
                Some((member.name.clone(), resolution.segments.max(1)))
            }
            _ => None,
        });
        let Some((name, named)) = field else {
            self.ice(expr, "a name that is not a field of `this` is not lowered")
        };
        let mut place = self.field_place(expr, self.this_place(expr), &name);
        for segment in path.segments.iter().skip(named) {
            place = self.field_place(expr, place, segment);
        }
        place
    }
}

/// The instance field of the struct `item` whose initializer is the body
/// of the value member at `index`.
pub(super) fn field_of_value(db: &dyn Db, item: ItemId, index: u32) -> Option<&Field> {
    instance_fields(db, item)
        .find(|&(member, _)| member == index)
        .map(|(_, field)| field)
}
