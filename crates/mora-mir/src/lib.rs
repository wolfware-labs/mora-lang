//! MIR, lowering from HIR, move and borrow checks, async lowering, monomorphization, ARC passes and constant evaluation (§14.7 to §14.12).

mod mir;

#[cfg(test)]
mod tests;

pub use mir::{
    AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const, InterpPiece,
    LocalDecl, LocalId, LocalSource, LocalTy, Mir, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, TerminatorKind, UnOp,
};
