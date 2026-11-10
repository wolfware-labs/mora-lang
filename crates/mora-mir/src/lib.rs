//! MIR, lowering from HIR, move and borrow checks, async lowering, monomorphization, ARC passes and constant evaluation (§14.7 to §14.12).

mod dump;
mod mir;
mod validate;

#[cfg(test)]
mod tests;

pub use dump::{write_body, write_body_in};
pub use mir::{
    AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const, InterpPiece,
    LocalDecl, LocalId, LocalSource, LocalTy, Mir, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, TerminatorKind, UnOp,
};
pub use validate::{debug_validate, validate};
