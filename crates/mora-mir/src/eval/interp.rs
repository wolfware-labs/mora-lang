//! The interpreter of constant evaluation: it runs the MIR of a constant's
//! initializer, of a field initializer or of a default value over constant
//! values (§14.6).
//!
//! The lowering of such a body holds only what §14.6 allows, which the scan
//! checks before anything runs, so any other form here is a compiler bug.

use mora_hir::{ItemId, Ty, ty_name};
use mora_span::{Db, TextRange};

use super::arith::{self, ArithError};
use super::{ConstValue, fail_evaluating};
use crate::dump::constant;
use crate::{
    BinOp, BlockId, Body, Callee, Const, LocalId, Operand, Place, Projection, Rvalue,
    StatementKind, TerminatorKind,
};

/// An operation that failed: where, why, in which type, and its operands
/// as text (`1500000000 * 2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The range of the statement that holds the operation.
    pub(crate) range: TextRange,
    /// Why it has no value.
    pub(crate) error: ArithError,
    /// The type of its result.
    pub(crate) ty: Ty,
    /// The operation with its operands' values, as code writes it; a
    /// concatenation too long to keep ([`ArithError::TooLong`]) gives the
    /// length of each operand instead, as in `<16777215 bytes> + <2 bytes>`.
    pub(crate) operation: String,
}

/// Why a run stopped without a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// An operation failed.
    Failed(Failure),
    /// A field initializer the body calls has no value yet.
    Dependency,
}

/// Runs `body` from its entry block to `return` and gives the value of
/// `_0`. An `Initializer` call takes its value from `initializer`; `None`
/// stops with [`Stop::Dependency`].
///
/// # Panics
///
/// On a form that a body constant evaluation runs never holds: a borrow, a
/// dereference, an interpolated string, a call of anything but a field
/// initializer, an unreachable block, or a read of a local without a value.
pub(crate) fn run(
    db: &dyn Db,
    body: &Body,
    initializer: &mut dyn FnMut(ItemId, u32) -> Option<ConstValue>,
) -> Result<ConstValue, Stop> {
    let mut machine = Machine {
        db,
        body,
        locals: vec![None; body.locals.len()],
    };
    let mut block = BlockId::ENTRY;
    loop {
        let data = body.block(block);
        for statement in &data.statements {
            match &statement.kind {
                StatementKind::Assign(place, rvalue) => {
                    let value = machine.rvalue(rvalue).map_err(|(error, operation)| {
                        Stop::Failed(Failure {
                            range: statement.span.range,
                            error,
                            ty: body.place_ty(place, db),
                            operation,
                        })
                    })?;
                    machine.write(place, value);
                }
                StatementKind::StorageDead(local) => *machine.slot(*local) = None,
                StatementKind::StorageLive(_) | StatementKind::Nop => {}
            }
        }
        block = match &data.terminator.kind {
            TerminatorKind::Goto(target) => *target,
            TerminatorKind::Switch {
                discr,
                targets,
                otherwise,
            } => {
                let discr = machine.scalar(discr);
                targets
                    .iter()
                    .find(|(value, _)| arith::equal(value, &discr))
                    .map_or(*otherwise, |&(_, target)| target)
            }
            TerminatorKind::Return => return Ok(machine.read(&Place::local(LocalId::RETURN))),
            TerminatorKind::Call {
                callee: Callee::Initializer(item, field),
                dest,
                next,
                ..
            } => {
                let value = initializer(*item, *field).ok_or(Stop::Dependency)?;
                if let Some(dest) = dest {
                    machine.write(dest, value);
                }
                match next {
                    Some(next) => *next,
                    None => machine.internal("a call that does not return"),
                }
            }
            TerminatorKind::Call { .. } => machine.internal("a call"),
            TerminatorKind::Unreachable => machine.internal("an unreachable block"),
        };
    }
}

/// The state of a run: the value of each local, `None` while it has none.
struct Machine<'a> {
    db: &'a dyn Db,
    body: &'a Body,
    locals: Vec<Option<ConstValue>>,
}

/// Why an operation has no value, and the operation as text.
type Failed = (ArithError, String);

impl Machine<'_> {
    /// The value of `rvalue`.
    fn rvalue(&self, rvalue: &Rvalue) -> Result<ConstValue, Failed> {
        let value = match rvalue {
            Rvalue::Use(operand) => return Ok(self.operand(operand)),
            Rvalue::Binary(op, left, right) => {
                let (left, right) = (self.operand(left), self.operand(right));
                return self.binary(*op, &left, &right);
            }
            Rvalue::Unary(op, operand) => {
                let value = self.scalar(operand);
                arith::unary(*op, &value)
                    .map_err(|error| (error, format!("{}({})", op.text(), constant(&value))))?
            }
            Rvalue::Cast(_, operand, ty) => {
                let value = self.scalar(operand);
                let Ty::Primitive(to) = ty else {
                    self.internal(&format!("a cast to {}", ty_name(self.db, ty)));
                };
                arith::cast(&value, *to).map_err(|error| {
                    let text = format!("({}){}", ty_name(self.db, ty), constant(&value));
                    (error, text)
                })?
            }
            Rvalue::Aggregate(kind, parts) => {
                let parts = parts.iter().map(|part| self.operand(part)).collect();
                return Ok(ConstValue::Aggregate(*kind, parts));
            }
            Rvalue::Borrow(..) => self.internal("a borrow"),
            Rvalue::Interpolate(_) => self.internal("an interpolated string"),
        };
        Ok(ConstValue::Scalar(value))
    }

    /// `op` on two values: scalars through [`arith::binary`], two
    /// aggregates compared field by field.
    fn binary(
        &self,
        op: BinOp,
        left: &ConstValue,
        right: &ConstValue,
    ) -> Result<ConstValue, Failed> {
        match (left, right) {
            (ConstValue::Scalar(left), ConstValue::Scalar(right)) => arith::binary(op, left, right)
                .map(ConstValue::Scalar)
                .map_err(|error| {
                    let text = match (error, left, right) {
                        (ArithError::TooLong(_), Const::String(left), Const::String(right)) => {
                            format!("<{} bytes> + <{} bytes>", left.len(), right.len())
                        }
                        _ => format!("{} {} {}", constant(left), op.text(), constant(right)),
                    };
                    (error, text)
                }),
            _ => {
                let equal = match op {
                    BinOp::Eq => true,
                    BinOp::Ne => false,
                    _ => self.internal(&format!("`{}` on an aggregate", op.text())),
                };
                Ok(ConstValue::Scalar(Const::Bool(
                    self.equal(left, right) == equal,
                )))
            }
        }
    }

    /// `==` on two values of one type, field by field for aggregates.
    fn equal(&self, left: &ConstValue, right: &ConstValue) -> bool {
        match (left, right) {
            (ConstValue::Scalar(left), ConstValue::Scalar(right)) => arith::equal(left, right),
            (ConstValue::Aggregate(_, left), ConstValue::Aggregate(_, right))
                if left.len() == right.len() =>
            {
                left.iter()
                    .zip(right)
                    .all(|(left, right)| self.equal(left, right))
            }
            _ => self.internal("`==` on values of different shapes"),
        }
    }

    /// The value of `operand`.
    fn operand(&self, operand: &Operand) -> ConstValue {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.read(place),
            Operand::Const(value) => ConstValue::Scalar(value.clone()),
        }
    }

    /// The value of `operand`, which is a scalar.
    fn scalar(&self, operand: &Operand) -> Const {
        match self.operand(operand) {
            ConstValue::Scalar(value) => value,
            ConstValue::Aggregate(..) => self.internal("an aggregate where a scalar goes"),
        }
    }

    /// The value at `place`.
    fn read(&self, place: &Place) -> ConstValue {
        let name = &self.body.name;
        let mut value = match self.locals.get(place.local.index()) {
            Some(Some(value)) => value,
            Some(None) => no_value(name, place.local),
            None => no_local(name, place.local),
        };
        for &projection in &place.projections {
            value = match (value, projection) {
                (ConstValue::Aggregate(_, fields), Projection::Field(n)) => field(name, fields, n),
                (ConstValue::Scalar(_), Projection::Field(n)) => {
                    fail_evaluating(name, &format!("field {n} of a scalar"))
                }
                (_, Projection::Deref) => fail_evaluating(name, "a dereference"),
            };
        }
        value.clone()
    }

    /// Stores `value` at `place`.
    fn write(&mut self, place: &Place, value: ConstValue) {
        let name = &self.body.name;
        let Some(slot) = self.locals.get_mut(place.local.index()) else {
            no_local(name, place.local)
        };
        if place.projections.is_empty() {
            *slot = Some(value);
            return;
        }
        let Some(mut target) = slot.as_mut() else {
            no_value(name, place.local)
        };
        for &projection in &place.projections {
            target = match (target, projection) {
                (ConstValue::Aggregate(_, fields), Projection::Field(n)) => {
                    field_mut(name, fields, n)
                }
                (ConstValue::Scalar(_), Projection::Field(n)) => {
                    fail_evaluating(name, &format!("field {n} of a scalar"))
                }
                (_, Projection::Deref) => fail_evaluating(name, "a dereference"),
            };
        }
        *target = value;
    }

    /// Where the value of `local` is kept.
    fn slot(&mut self, local: LocalId) -> &mut Option<ConstValue> {
        match self.locals.get_mut(local.index()) {
            Some(slot) => slot,
            None => no_local(&self.body.name, local),
        }
    }

    /// Panics with an internal error that names the body.
    fn internal(&self, what: &str) -> ! {
        fail_evaluating(&self.body.name, what)
    }
}

/// The field `n` of an aggregate of `fields` in the body `name`.
fn field<'v>(name: &str, fields: &'v [ConstValue], n: u32) -> &'v ConstValue {
    match fields.get(n as usize) {
        Some(field) => field,
        None => fail_evaluating(name, &format!("no field {n}")),
    }
}

/// The field `n`, to write, of an aggregate of `fields` in the body `name`.
fn field_mut<'v>(name: &str, fields: &'v mut [ConstValue], n: u32) -> &'v mut ConstValue {
    match fields.get_mut(n as usize) {
        Some(field) => field,
        None => fail_evaluating(name, &format!("no field {n}")),
    }
}

/// Panics on a read of `local`, which has no value, in the body `name`.
fn no_value(name: &str, local: LocalId) -> ! {
    fail_evaluating(name, &format!("_{} has no value", local.0))
}

/// Panics on a use of `local`, which the body `name` does not have.
fn no_local(name: &str, local: LocalId) -> ! {
    fail_evaluating(name, &format!("no local _{}", local.0))
}
