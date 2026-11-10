//! Structs: memberwise creation, fields and the initializers of fields
//! (§6.2, §6.9).

use mora_hir::{
    Arg, BodyOwner, ExprId, Field, ItemId, Path, Signature, Target, Ty, body, field_member,
    item_tree, signature, struct_field,
};
use mora_span::Db;

use super::expr::Dest;
use super::{Lowered, Lowerer};
use crate::{AggregateKind, Callee, Operand, Place, Projection, Rvalue};

impl Lowerer<'_> {
    /// `new T { F = v, ... }` for the struct `T`: every field in the order
    /// of the declaration. The written values are evaluated in the order
    /// of the text; a field left out then takes the value of its
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
        let Signature::Struct(fields) = signature(self.owner.db, item) else {
            self.ice(expr, "a struct type whose item is not a struct")
        };
        let inits = inits.unwrap_or_default();
        let values: Vec<ExprId> = inits.iter().map(|&(_, value)| value).collect();
        let mut written: Vec<Option<Operand>> =
            self.operands(&values)?.into_iter().map(Some).collect();
        let mut operands = Vec::with_capacity(fields.len());
        for (n, field) in fields.iter().enumerate() {
            self.instance_field(expr, item, n);
            let init = inits.iter().position(|(name, _)| *name == field.name);
            operands.push(match init.and_then(|init| written[init].take()) {
                Some(operand) => operand,
                None => self.initializer_call(expr, item, n, field)?,
            });
        }
        Ok(Rvalue::Aggregate(AggregateKind::Struct(item), operands))
    }

    /// The value of the initializer of `field`, the `n`-th field of the
    /// struct `item`, which `expr` leaves out: a call of its body, into a
    /// temporary.
    fn initializer_call(
        &mut self,
        expr: ExprId,
        item: ItemId,
        n: usize,
        field: &Field,
    ) -> Lowered<Operand> {
        let index = self.instance_field(expr, item, n);
        let owner = BodyOwner::Value(item.item, index);
        if body(self.owner.db, item.file, owner).root.is_none() {
            self.ice(expr, "a field without an initializer is left out");
        }
        let Ok(n) = u32::try_from(n) else {
            self.ice(expr, "a struct with more than u32::MAX fields")
        };
        let dest = self.finish_call(
            expr,
            Callee::Initializer(item, n),
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
    /// names.
    pub(super) fn field_place(&self, expr: ExprId, mut base: Place, name: &str) -> Place {
        let field = match self.builder.body().place_ty(&base, self.owner.db) {
            Ty::Struct(item) => match signature(self.owner.db, item) {
                Signature::Struct(fields) => fields
                    .iter()
                    .position(|field| field.name == name)
                    .map(|n| (item, n)),
                _ => None,
            },
            _ => None,
        };
        let Some((item, n)) = field else {
            self.ice(
                expr,
                "a member that is not a field of a struct is not lowered",
            )
        };
        self.instance_field(expr, item, n);
        let Ok(n) = u32::try_from(n) else {
            self.ice(expr, "a struct with more than u32::MAX fields")
        };
        base.projections.push(Projection::Field(n));
        base
    }

    /// The position among the value members of the struct `item` of its
    /// `n`-th field, which `expr` uses. Static and constant members are
    /// not stored in a value, and lowering them is for the static state
    /// of M1.11.
    fn instance_field(&self, expr: ExprId, item: ItemId, n: usize) -> u32 {
        match field_member(self.owner.db, item, n) {
            Some((index, member)) if member.is_instance_field() => index,
            Some(_) => self.ice(expr, "a static or constant member is not lowered (M1.11)"),
            None => self.ice(expr, "a field that is not a value member"),
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

/// The field of the struct `item` whose initializer is the body of the
/// value member at `index`: an instance field.
pub(super) fn field_of_value(db: &dyn Db, item: ItemId, index: u32) -> Option<&Field> {
    let member = item_tree(db, item.file)
        .item(item.item)
        .values
        .get(index as usize)?;
    if !member.is_instance_field() {
        return None;
    }
    struct_field(db, item, index).map(|(_, field)| field)
}
