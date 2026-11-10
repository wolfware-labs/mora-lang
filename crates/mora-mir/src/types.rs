//! The data types of MIR (§14.7): a control-flow graph of basic blocks per
//! body, over typed locals and places.

use mora_hir::{ItemId, ParamMode, Primitive, Signature, Ty, instance_fields, signature};
use mora_span::{Db, Span};

/// The MIR of one body owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mir {
    /// The owner's body first, then its local functions in the order of the
    /// text. [`Callee::Local`] indexes this list.
    pub bodies: Vec<Body>,
}

/// One function body as a control-flow graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    /// The name of the function, for dumps and messages.
    pub name: String,
    /// How many parameters it has; they are the locals `_1` to `_params`.
    pub params: usize,
    /// Every local; `_0` is the return slot.
    pub locals: Vec<LocalDecl>,
    /// Every basic block; block 0 is the entry.
    pub blocks: Vec<Block>,
    /// The span of the function.
    pub span: Span,
}

/// The index of a local in [`Body::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(
    /// The position in [`Body::locals`].
    pub u32,
);

impl LocalId {
    /// The return slot `_0`.
    pub const RETURN: LocalId = LocalId(0);

    /// The position in [`Body::locals`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The index of a block in [`Body::blocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(
    /// The position in [`Body::blocks`].
    pub u32,
);

impl BlockId {
    /// The block where execution starts.
    pub const ENTRY: BlockId = BlockId(0);

    /// The position in [`Body::blocks`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A local variable, parameter, temporary or the return slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDecl {
    /// Its type.
    pub ty: LocalTy,
    /// Whether it may be assigned more than once.
    pub mutable: bool,
    /// The source name it comes from, `None` for a temporary.
    pub source: Option<LocalSource>,
}

/// The type of a local: a value, or a borrow of one (§10.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalTy {
    /// The local holds a value of the type.
    Value(Ty),
    /// The local borrows a value of the type: an `in` or `ref` parameter, or
    /// `this`.
    Borrow(BorrowMode, Ty),
}

/// How a borrow lets its holder use the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BorrowMode {
    /// Read only.
    In,
    /// Read and write.
    Ref,
}

/// Where a local comes from in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSource {
    /// The name as written; `this` for the receiver.
    pub name: String,
    /// The local of the HIR body it was lowered from; `None` for the
    /// receiver, which the HIR body does not declare.
    pub hir: Option<mora_hir::LocalId>,
}

/// A basic block: statements, then one terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The statements, run in order.
    pub statements: Vec<Statement>,
    /// How the block ends.
    pub terminator: Terminator,
}

/// A statement of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// What it does.
    pub kind: StatementKind,
    /// The source it comes from.
    pub span: Span,
}

/// What a statement does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementKind {
    /// Evaluate the value and store it in the place.
    Assign(Place, Rvalue),
    /// The local's storage begins here.
    StorageLive(LocalId),
    /// The local's storage ends here.
    StorageDead(LocalId),
    /// Nothing.
    Nop,
}

/// A location that holds a value: a local and a path into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The local it starts from.
    pub local: LocalId,
    /// The steps from the local, outermost first.
    pub projections: Vec<Projection>,
}

impl Place {
    /// The place of a whole local.
    pub fn local(local: LocalId) -> Place {
        Place {
            local,
            projections: Vec::new(),
        }
    }
}

/// One step into a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Projection {
    /// The field of a struct, by its position among the instance fields
    /// in the declaration, or the element of a tuple.
    Field(u32),
    /// The value a borrow refers to.
    Deref,
}

/// A value used by a statement or terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    /// Read the place and leave it usable.
    Copy(Place),
    /// Read the place and give up the value.
    Move(Place),
    /// A constant.
    Const(Const),
}

/// A constant value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Const {
    /// An integer of the primitive type.
    Int(i128, Primitive),
    /// A floating-point number of the primitive type, as its IEEE 754 bits.
    Real(u64, Primitive),
    /// A `bool`.
    Bool(bool),
    /// A `char`.
    Char(char),
    /// A `string`.
    String(String),
    /// The unit value `()`.
    Unit,
}

impl Const {
    /// The type of the constant.
    pub fn ty(&self) -> Ty {
        match self {
            Const::Int(_, primitive) | Const::Real(_, primitive) => Ty::Primitive(*primitive),
            Const::Bool(_) => Ty::Primitive(Primitive::Bool),
            Const::Char(_) => Ty::Primitive(Primitive::Char),
            Const::String(_) => Ty::String,
            Const::Unit => Ty::UNIT,
        }
    }
}

/// A computation whose result is stored in a place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rvalue {
    /// The operand itself.
    Use(Operand),
    /// A borrow of the place.
    Borrow(BorrowMode, Place),
    /// A binary operation.
    Binary(BinOp, Operand, Operand),
    /// A unary operation.
    Unary(UnOp, Operand),
    /// The operand converted to the type.
    Cast(CastKind, Operand, Ty),
    /// A struct or tuple built from its parts, in order.
    Aggregate(AggregateKind, Vec<Operand>),
    /// A string built from text and operands.
    Interpolate(Vec<InterpPiece>),
}

/// The kind of a conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastKind {
    /// A conversion that keeps every value (§5.4).
    Implicit,
    /// A conversion between numeric types written out.
    Numeric,
}

/// What an aggregate builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AggregateKind {
    /// A struct, by its declaration.
    Struct(ItemId),
    /// A tuple.
    Tuple,
}

/// A piece of an interpolated string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpPiece {
    /// Literal text.
    Text(String),
    /// A value to format, from a hole `{value,alignment:format}`.
    Operand {
        /// The value.
        value: Operand,
        /// The width it is padded to, an integer: right-aligned when
        /// positive, left-aligned when negative.
        alignment: Option<Operand>,
        /// The format, as written after the colon.
        format: Option<String>,
    },
}

/// A binary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// `+`, checked.
    Add,
    /// `-`, checked.
    Sub,
    /// `*`, checked.
    Mul,
    /// `/`; an integer division by zero panics.
    Div,
    /// `%`; an integer remainder by zero panics.
    Rem,
    /// `+` that wraps around.
    WrappingAdd,
    /// `-` that wraps around.
    WrappingSub,
    /// `*` that wraps around.
    WrappingMul,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,
    /// `>>>`.
    UShr,
    /// `&`.
    BitAnd,
    /// `|`.
    BitOr,
    /// `^`.
    BitXor,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

/// A unary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// `-`, checked.
    Neg,
    /// `!`.
    Not,
    /// `~`.
    BitNot,
}

/// How a block ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminator {
    /// What it does.
    pub kind: TerminatorKind,
    /// The source it comes from.
    pub span: Span,
}

/// What a terminator does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminatorKind {
    /// Continue in the block.
    Goto(BlockId),
    /// Continue in the block whose constant equals the discriminant.
    Switch {
        /// The value tested.
        discr: Operand,
        /// The constants to match and the block for each.
        targets: Vec<(Const, BlockId)>,
        /// The block when no constant matches.
        otherwise: BlockId,
    },
    /// Leave the function with the value in `_0`.
    Return,
    /// Call a function.
    Call {
        /// What is called.
        callee: Callee,
        /// The arguments, in order. A borrowed argument is a copy of a
        /// temporary that holds the borrow.
        args: Vec<Operand>,
        /// Where the result goes, `None` when it is dropped. It is written
        /// after the callee returns, so it may overlap a place an argument
        /// borrows.
        dest: Option<Place>,
        /// The block after the call; `None` when the call does not return.
        next: Option<BlockId>,
    },
    /// Control never gets here.
    Unreachable,
}

impl TerminatorKind {
    /// The blocks control may go to after this terminator, in order: a
    /// switch's targets, then its `otherwise`; a call's `next`, which a call
    /// that does not return has none of.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            TerminatorKind::Goto(target) => vec![*target],
            TerminatorKind::Switch {
                targets, otherwise, ..
            } => targets
                .iter()
                .map(|(_, target)| *target)
                .chain([*otherwise])
                .collect(),
            TerminatorKind::Call { next, .. } => next.iter().copied().collect(),
            TerminatorKind::Return | TerminatorKind::Unreachable => Vec::new(),
        }
    }
}

/// What a call targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    /// A function or method of a project.
    Item(ItemId),
    /// A local function, by its index in [`Mir::bodies`].
    Local(usize),
    /// The initializer of a field of a struct, by the field's position
    /// among its instance fields.
    Initializer(ItemId, u32),
    /// A function of the standard library, by its path.
    External(Vec<String>),
}

impl Body {
    /// The ids of the blocks, in order.
    pub fn block_ids(&self) -> impl Iterator<Item = BlockId> + use<> {
        (0..self.blocks.len()).map(|index| BlockId(index as u32))
    }

    /// The declaration of the local.
    ///
    /// # Panics
    ///
    /// When the body has no such local.
    pub fn local(&self, id: LocalId) -> &LocalDecl {
        match self.locals.get(id.index()) {
            Some(decl) => decl,
            None => panic!("body {}: no local _{}", self.name, id.0),
        }
    }

    /// The block.
    ///
    /// # Panics
    ///
    /// When the body has no such block.
    pub fn block(&self, id: BlockId) -> &Block {
        match self.blocks.get(id.index()) {
            Some(block) => block,
            None => panic!("body {}: no block bb{}", self.name, id.0),
        }
    }

    /// The type of the value at `place`. It looks the type up and does not
    /// check the place.
    ///
    /// # Panics
    ///
    /// When the place is invalid: its local is missing, it dereferences a
    /// value local, or it takes a field of something that is not a struct or a
    /// tuple, or past its last field.
    pub fn place_ty(&self, place: &Place, db: &dyn Db) -> Ty {
        let (mut ty, mut borrowed) = match &self.local(place.local).ty {
            LocalTy::Value(ty) => (ty.clone(), false),
            LocalTy::Borrow(_, ty) => (ty.clone(), true),
        };
        for projection in &place.projections {
            match *projection {
                Projection::Deref => {
                    assert!(
                        borrowed,
                        "body {}: dereference of a value local in a place of _{}",
                        self.name, place.local.0
                    );
                    borrowed = false;
                }
                Projection::Field(n) => {
                    ty = self.field_ty(&ty, n, db);
                    borrowed = false;
                }
            }
        }
        ty
    }

    /// The type of the `n`-th field of the struct or tuple `ty`.
    fn field_ty(&self, ty: &Ty, n: u32, db: &dyn Db) -> Ty {
        match ty {
            Ty::Struct(item) => {
                let Signature::Struct(_) = signature(db, *item) else {
                    panic!(
                        "body {}: a struct type whose item is not a struct",
                        self.name
                    );
                };
                match instance_fields(db, *item).nth(n as usize) {
                    Some((_, field)) => field.ty.clone(),
                    None => panic!(
                        "body {}: field {n} out of range of a struct of {} instance fields",
                        self.name,
                        instance_fields(db, *item).count()
                    ),
                }
            }
            Ty::Tuple(elems) => match elems.get(n as usize) {
                Some(elem) => elem.ty.clone(),
                None => panic!(
                    "body {}: field {n} out of range of a tuple of {} elements",
                    self.name,
                    elems.len()
                ),
            },
            _ => panic!(
                "body {}: field of a type that is not a struct or a tuple",
                self.name
            ),
        }
    }
}

/// Whether MIR can hold a value of `ty`: no part of it is an error or a
/// type whose meaning a later part of the compiler gives. The never type is
/// concrete; a local of it is declared and never assigned.
pub(crate) fn is_concrete(ty: &Ty) -> bool {
    match ty {
        Ty::Unsupported(_) | Ty::Error => false,
        Ty::Tuple(elems) => elems.iter().all(|elem| is_concrete(&elem.ty)),
        Ty::Primitive(_) | Ty::String | Ty::Struct(_) | Ty::Never => true,
    }
}

/// The type of a parameter of mode `mode` and type `ty`, or of a receiver
/// taken that way, in the callee: a borrow for `in` and `ref`, the value
/// itself for `move` (§10.3).
pub(crate) fn param_ty(mode: ParamMode, ty: Ty) -> LocalTy {
    match mode {
        ParamMode::In => LocalTy::Borrow(BorrowMode::In, ty),
        ParamMode::Ref => LocalTy::Borrow(BorrowMode::Ref, ty),
        ParamMode::Move => LocalTy::Value(ty),
    }
}
