//! Function bodies and top-level statements as HIR (§7, §14.3).
//!
//! [`body`] lowers the statements of one owner: a function, or the
//! top-level statements of a file. Locals are resolved while lowering, by
//! their scopes (§7.10), so a [`Body`] names each local by its
//! [`LocalId`]; every other name stays a [`Path`] that
//! [`crate::body_resolution`] looks up. A body holds no text ranges, so an
//! edit elsewhere in the file leaves it equal; the ranges are in
//! [`body_source_map`].
//!
//! Every declaration with code has an owner ([`BodyOwner`]): functions,
//! getters, constructors, destructors, operators, indexer accessors,
//! extension members, and the initializers of fields, constants and enum
//! members. A parameter's default value belongs to its function's body.

mod dump;
mod lower;
mod owners;
mod resolve;

use std::collections::HashMap;
use std::sync::Arc;

use la_arena::{Arena, Idx};
use mora_span::{Db, SourceFile, TextRange};

use crate::arena::arena_index;
use crate::items::{ItemIdx, Visibility, item_tree, parse};
use crate::ty::ParamMode;
use crate::type_ref::TypeRef;

pub use dump::{escape_char, hir_dump, item_text, owner_name};
pub(crate) use resolve::{BodyError, body_scope};
pub use resolve::{BodyResolution, PathResolution, Target, ValueRef, body_resolution};

/// What a body belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BodyOwner {
    /// The top-level statements of the file (§4.6).
    TopLevel,
    /// A function of the file's item tree, or the initializer of a
    /// constant or static at namespace level.
    Item(ItemIdx),
    /// A value member of a type, by its position in [`crate::Item::values`]:
    /// a field's type and initializer, a getter, an enum member's value, or
    /// the field types of a union case.
    Value(ItemIdx, u32),
    /// A member of a type that is neither an item nor a value member, by
    /// its position among those in the order of the text: a constructor, a
    /// destructor, an operator, an indexer accessor, or a function or
    /// getter that implements an interface member.
    Member(ItemIdx, u32),
    /// An extension block of the file, by its position among them: member
    /// 0 is the block itself, which checks the receiver's type, and the
    /// others its members in the order of the text (§6.9).
    Extension(u32, u32),
}

/// What kind of declaration a body comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BodyKind {
    /// The top-level statements of a file.
    #[default]
    TopLevel,
    /// A function: an item, an implementation of an interface member, or an
    /// extension member.
    Function,
    /// The initializer of a field, a constant or an enum member.
    Initializer,
    /// A getter.
    Getter,
    /// A constructor.
    Constructor,
    /// A destructor.
    Destructor,
    /// An operator.
    Operator,
    /// An indexer's `get` accessor, or an indexer with an expression body.
    Get,
    /// An indexer's `set` accessor.
    Set,
    /// An extension block itself.
    Extension,
    /// A union case, whose fields' types are checked with it.
    Case,
}

/// An index into [`Body::exprs`].
pub type ExprId = Idx<Expr>;
/// An index into [`Body::stmts`].
pub type StmtId = Idx<Stmt>;
/// An index into [`Body::pats`].
pub type PatId = Idx<Pat>;
/// An index into [`Body::locals`].
pub type LocalId = Idx<Local>;
/// An index into [`Body::scopes`].
pub type ScopeId = Idx<ScopeData>;
/// An index into [`Body::types`].
pub type TypeId = Idx<TypeRef>;

/// The statements and expressions of one owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    /// What kind of declaration it comes from.
    pub kind: BodyKind,
    /// The name of the declaration the body belongs to, as written, which
    /// `MR0123` names: a value member's, `IShape.Area`, an operator's
    /// symbol, `this[]` for an indexer, the type's for a constructor, an
    /// extension member's, `extension` for an extension block itself.
    /// Empty for the top-level statements and the items, whose names the
    /// item tree holds.
    pub name: String,
    /// Every expression.
    pub exprs: Arena<Expr>,
    /// Every statement.
    pub stmts: Arena<Stmt>,
    /// Every pattern.
    pub pats: Arena<Pat>,
    /// Every local, parameter and pattern binding.
    pub locals: Arena<Local>,
    /// The scopes: each declaration opens one inside the scope it is in.
    pub scopes: Arena<ScopeData>,
    /// Every type written in the body, as written.
    pub types: Arena<TypeRef>,
    /// The type parameters of the local functions around a type, for the
    /// types inside a generic local function.
    pub type_params: HashMap<TypeId, Vec<String>>,
    /// The parameters of the function, in order.
    pub params: Vec<LocalId>,
    /// The default value of each parameter that has one.
    pub defaults: Vec<(LocalId, ExprId)>,
    /// The parameter and return types of a declaration whose signature the
    /// item tree does not hold, which are checked with the body.
    pub signature_types: Vec<TypeId>,
    /// The visibility written on that declaration, against which
    /// [`Body::signature_types`] are checked (`MR0123`). `Private`, which
    /// nothing is narrower than, for one the body checks nothing for.
    pub visibility: Visibility,
    /// The return type, as written, of a declaration whose signature the
    /// item tree does not hold; `None` when none is written or the item
    /// tree holds it.
    pub ret: Option<TypeId>,
    /// Whether that declaration is `async`; `false` when the item tree
    /// holds its signature, which says it ([`crate::FnSignature::is_async`]).
    pub is_async: bool,
    /// Whether that declaration is `static`, for a member that is not an
    /// item: an operator, which always is, a getter, or an implementation
    /// of an interface member. `false` for an item, whose
    /// [`crate::Item::is_static`] says it.
    pub is_static: bool,
    /// A constructor's `: this(...)`, as a call of `this`.
    pub this_call: Option<ExprId>,
    /// The body itself: a block, or the expression of `=> e`. `None` for a
    /// function declared without a body.
    pub root: Option<ExprId>,
    /// The locals that take the name of a local in scope (`MR0180`), each
    /// with the one it would shadow.
    pub shadows: Vec<(LocalId, LocalId)>,
    /// The `ref` parameters that have a default value (`MR0209`), in the
    /// order of the text.
    pub ref_defaults: Vec<LocalId>,
}

/// A local, a parameter or a pattern binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Local {
    /// The declared name, in NFC.
    pub name: String,
    /// How it was declared.
    pub kind: LocalKind,
    /// Its type as written, if one is.
    pub ty: Option<TypeId>,
}

/// How a [`Local`] was declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalKind {
    /// `let`, or a binding that cannot be reassigned.
    Let,
    /// `var`.
    Var,
    /// A parameter of the function, a lambda or a local function.
    Param,
    /// A local function.
    Function,
}

/// One scope: the local it declares and the scope it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeData {
    /// The enclosing scope; `None` for the body's outermost one.
    pub parent: Option<ScopeId>,
    /// The local this scope declares.
    pub local: Option<LocalId>,
}

impl Body {
    /// Whether `expr` is an expression the parser recovered from.
    pub(crate) fn is_missing(&self, expr: ExprId) -> bool {
        matches!(self.exprs[expr], Expr::Missing)
    }

    /// The locals in scope at `scope`, innermost first.
    pub fn locals_in_scope(&self, scope: ScopeId) -> impl Iterator<Item = LocalId> + '_ {
        std::iter::successors(Some(scope), |&scope| self.scopes[scope].parent)
            .filter_map(|scope| self.scopes[scope].local)
    }
}

/// A name that is not a local, as written: what it names is found by
/// [`crate::body_resolution`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    /// The segments, in NFC.
    pub segments: Vec<String>,
    /// The generic arguments of each segment, in order.
    pub args: Vec<Vec<TypeId>>,
    /// The scope it is used in, which gives the locals for a suggestion.
    pub scope: ScopeId,
}

/// An expression of a body (§7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// An expression the parser recovered from.
    Missing,
    /// A literal as written: its text is checked and converted later.
    Literal(Literal),
    /// An interpolated string (§3.5).
    Interpolated(Vec<InterpPart>),
    /// A local, a parameter or a binding.
    Local(LocalId),
    /// Any other name.
    Path(Path),
    /// `this`.
    This,
    /// A type keyword used as an expression, as in `int.Parse`.
    Type(TypeId),
    /// `(a, b)`, with the names of named elements.
    Tuple(Vec<(Option<String>, ExprId)>),
    /// `_` among the places of a deconstructing assignment, as in
    /// `(a, _) = pair;` (§5.5): the element is not stored.
    Discard,
    /// `[a, ..b]`: each element, and whether it is spread.
    Collection(Vec<(bool, ExprId)>),
    /// `x.m` or `x?.m`.
    Member {
        receiver: ExprId,
        name: String,
        optional: bool,
        args: Vec<TypeId>,
    },
    /// `f(args)`.
    Call { callee: ExprId, args: Vec<Arg> },
    /// `a[i]` or `a?[i]`.
    Index {
        base: ExprId,
        args: Vec<Arg>,
        optional: bool,
    },
    /// A prefix operator applied to `expr`.
    Unary { op: UnaryOp, expr: ExprId },
    /// `x++` and `x--`.
    Postfix { increment: bool, expr: ExprId },
    /// `a op b`.
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// `a = b`, or a compound assignment with its operator.
    Assign {
        op: Option<BinaryOp>,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// `e?` (§7.4).
    Propagate(ExprId),
    /// `(T)e`.
    Cast { ty: TypeId, expr: ExprId },
    /// `await e`.
    Await(ExprId),
    /// `a..b` with either end left out.
    Range {
        start: Option<ExprId>,
        end: Option<ExprId>,
    },
    /// `match (e) { ... }` as an expression.
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    /// `e with { F = v }`.
    With {
        expr: ExprId,
        inits: Vec<(String, ExprId)>,
    },
    /// `e is pattern`.
    Is { expr: ExprId, pat: PatId },
    /// `e as T`.
    As { expr: ExprId, ty: TypeId },
    /// `c ? a : b`.
    Conditional {
        condition: ExprId,
        then: ExprId,
        r#else: ExprId,
    },
    /// A lambda.
    Lambda(Lambda),
    /// `new T(args) { F = v }`, each part optional.
    New {
        ty: Option<TypeId>,
        args: Option<Vec<Arg>>,
        inits: Option<Vec<(String, ExprId)>>,
    },
    /// `new T[n]`, or `stackalloc T[n]` when `stack` is set.
    NewArray {
        ty: TypeId,
        size: ExprId,
        stack: bool,
    },
    /// `default` or `default(T)`.
    Default(Option<TypeId>),
    /// `nameof(e)`.
    Nameof(ExprId),
    /// `sizeof(T)`.
    Sizeof(TypeId),
    /// A block, as a function or lambda body or a `match` arm.
    Block(Vec<StmtId>),
    /// A quote, a splice or a macro call (§9), which a later part of the
    /// compiler expands.
    Macro,
}

/// A literal as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    /// An integer literal, as written.
    Int(String),
    /// A real literal, as written.
    Real(String),
    /// A character literal, quotes included.
    Char(String),
    /// A string literal, quotes included.
    String(String),
    /// `true` or `false`.
    Bool(bool),
    /// `none`.
    None,
}

/// A part of an interpolated string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpPart {
    /// Text: as written, and its value, `None` when the lexer reported
    /// that it has none.
    Text { text: String, value: Option<String> },
    /// `{value,alignment:format}`; the format as written after the
    /// colon.
    Hole {
        value: ExprId,
        alignment: Option<ExprId>,
        format: Option<String>,
    },
}

/// An argument of a call or an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    /// The name of a named argument.
    pub name: Option<String>,
    /// `ref`, `in` or `move` at the call site (§10.3).
    pub mode: Option<ArgMode>,
    /// The value.
    pub expr: ExprId,
}

/// How an argument is passed, as written at the call site (§10.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgMode {
    Ref,
    In,
    Move,
}

impl ArgMode {
    /// The marker as it is written.
    pub fn keyword(self) -> &'static str {
        match self {
            ArgMode::Ref => "ref",
            ArgMode::In => "in",
            ArgMode::Move => "move",
        }
    }
}

/// An arm of a `match`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchArm {
    /// The pattern.
    pub pat: PatId,
    /// The `when` guard.
    pub guard: Option<ExprId>,
    /// The value, or a [`Expr::Block`].
    pub body: ExprId,
}

/// A lambda (§7.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lambda {
    /// The parameters.
    pub params: Vec<LocalId>,
    /// The default value of each parameter that has one.
    pub defaults: Vec<(LocalId, ExprId)>,
    /// The capture list.
    pub captures: Vec<Capture>,
    /// Whether it is `async`.
    pub is_async: bool,
    /// Whether it is `static`, capturing nothing.
    pub is_static: bool,
    /// The value, or a [`Expr::Block`].
    pub body: ExprId,
}

/// An entry of a capture list, `[weak this]` or `[move conn]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// `weak` or `move`.
    pub weak: bool,
    /// What it captures.
    pub target: CaptureTarget,
}

/// What a [`Capture`] captures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// `this`.
    This,
    /// A local in scope.
    Local(LocalId),
    /// A name that is no local in scope (`MR0127`).
    Unresolved(String),
}

/// A prefix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `+x`.
    Plus,
    /// `-x`.
    Neg,
    /// `!x`.
    Not,
    /// `~x`.
    BitNot,
    /// `++x`.
    PreIncrement,
    /// `--x`.
    PreDecrement,
    /// `^i`, an index from the end.
    FromEnd,
}

impl UnaryOp {
    /// The operator as code writes it, `++` and `--` in either position.
    pub fn text(self) -> &'static str {
        match self {
            UnaryOp::Plus => "+",
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
            UnaryOp::BitNot => "~",
            UnaryOp::PreIncrement => "++",
            UnaryOp::PreDecrement => "--",
            UnaryOp::FromEnd => "^",
        }
    }
}

/// A binary operator; also the operator of a compound assignment or of a
/// relational pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Mul,
    Div,
    Rem,
    WrappingMul,
    Add,
    Sub,
    WrappingAdd,
    WrappingSub,
    Shl,
    Shr,
    UShr,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
    Coalesce,
}

impl BinaryOp {
    /// The operator as code writes it; a compound assignment adds `=`.
    pub fn text(self) -> &'static str {
        use BinaryOp as B;
        match self {
            B::Mul => "*",
            B::Div => "/",
            B::Rem => "%",
            B::WrappingMul => "*%",
            B::Add => "+",
            B::Sub => "-",
            B::WrappingAdd => "+%",
            B::WrappingSub => "-%",
            B::Shl => "<<",
            B::Shr => ">>",
            B::UShr => ">>>",
            B::Lt => "<",
            B::Gt => ">",
            B::Le => "<=",
            B::Ge => ">=",
            B::Eq => "==",
            B::Ne => "!=",
            B::BitAnd => "&",
            B::BitXor => "^",
            B::BitOr => "|",
            B::And => "&&",
            B::Or => "||",
            B::Coalesce => "??",
        }
    }
}

/// A statement of a body (§7.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    /// A statement the parser recovered from.
    Missing,
    /// An expression statement.
    Expr(ExprId),
    /// `let x = e;` and the other forms of §7.10.
    Local {
        local: LocalId,
        init: Option<ExprId>,
    },
    /// `let (a, b) = e;`.
    Deconstruct { pat: PatId, init: ExprId },
    /// A local function.
    Function(LocalFunction),
    /// `{ ... }`.
    Block(Vec<StmtId>),
    /// `if (c) s else t`.
    If {
        condition: ExprId,
        then: StmtId,
        r#else: Option<StmtId>,
    },
    /// `while (c) s`.
    While { condition: ExprId, body: StmtId },
    /// `do s while (c);`.
    DoWhile { body: StmtId, condition: ExprId },
    /// `for (init; condition; update) s`.
    For {
        init: Vec<StmtId>,
        condition: Option<ExprId>,
        update: Vec<ExprId>,
        body: StmtId,
    },
    /// `foreach (let x in xs)`; `by_ref` for `foreach (ref var x in span)`.
    Foreach {
        pat: PatId,
        by_ref: bool,
        iterable: ExprId,
        body: StmtId,
    },
    /// `break;`.
    Break,
    /// `continue;`.
    Continue,
    /// `return;` or `return e;`.
    Return(Option<ExprId>),
    /// `match (e) { ... }` as a statement.
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    /// `unsafe { }`.
    Unsafe(StmtId),
}

/// A local function (§7.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFunction {
    /// The local its name declares.
    pub local: LocalId,
    /// The parameters.
    pub params: Vec<LocalId>,
    /// The mode of each parameter, in the order of [`LocalFunction::params`]
    /// (§10.3).
    pub modes: Vec<ParamMode>,
    /// The default value of each parameter that has one.
    pub defaults: Vec<(LocalId, ExprId)>,
    /// The return type as written.
    pub ret: Option<TypeId>,
    /// Whether it is `async`.
    pub is_async: bool,
    /// Whether it declares type parameters, or a local function around it
    /// does, or the member whose body holds it does when that body
    /// declares its own signature: an explicit interface implementation or
    /// an extension member, with the extension's. The type parameters of a
    /// function item or of a type around the body are not counted: the
    /// item's own signature says whether it is generic.
    pub is_generic: bool,
    /// The block or the expression of `=> e`.
    pub body: Option<ExprId>,
}

/// A pattern (§7.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pat {
    /// A pattern the parser recovered from.
    Missing,
    /// A constant, compared for equality.
    Constant(ExprId),
    /// `User u` or `User`: the type, and the binding when there is one.
    Type { ty: TypeId, binding: Option<PatId> },
    /// `let x`, `var x`, or the name of a designation.
    Bind(LocalId),
    /// `_`.
    Discard,
    /// `< 0`.
    Relational { op: BinaryOp, value: ExprId },
    /// `not p`.
    Not(PatId),
    /// `p and q`.
    And(PatId, PatId),
    /// `p or q`.
    Or(PatId, PatId),
    /// `Circle(let r)`: the case's path, as an [`Expr::Path`], and its fields.
    Case {
        path: ExprId,
        fields: Vec<(Option<String>, PatId)>,
    },
    /// `(0, let y)`, with the names of named fields.
    Positional(Vec<(Option<String>, PatId)>),
    /// `T { Name: p } x`, each part but the members optional.
    Member {
        ty: Option<TypeId>,
        fields: Vec<(Option<String>, PatId)>,
        binding: Option<PatId>,
    },
    /// `[a, .. rest]`.
    List(Vec<PatId>),
    /// `..` or `.. p` inside a list pattern.
    Slice(Option<PatId>),
}

/// Where the parts of a [`Body`] are, for diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodySourceMap {
    /// Each expression, indexed like [`Body::exprs`].
    pub exprs: Vec<TextRange>,
    /// The name of each local, indexed like [`Body::locals`].
    pub locals: Vec<TextRange>,
    /// The segments of each [`Expr::Path`].
    pub path_segments: HashMap<ExprId, Vec<TextRange>>,
    /// The name of each [`Expr::Member`].
    pub member_names: HashMap<ExprId, TextRange>,
    /// The marker of each argument written with one, by the argument's
    /// expression.
    pub markers: HashMap<ExprId, TextRange>,
    /// Each statement but a local declaration, as written.
    pub stmts: HashMap<StmtId, TextRange>,
    /// The `let` written right before the name of a local, which the fix
    /// of `MR0182` turns into `var`.
    pub lets: HashMap<LocalId, TextRange>,
    /// The closing brace of each function and local function body written
    /// as a block, by the block's expression.
    pub ends: HashMap<ExprId, TextRange>,
    /// The closing brace of each block statement.
    pub block_ends: HashMap<StmtId, TextRange>,
    /// The name of each capture, or its `this`, by its lambda and position.
    pub captures: HashMap<(ExprId, usize), TextRange>,
    /// The segments of each path inside the body's types, indexed by
    /// [`crate::type_ref::PathRef::index`].
    pub type_paths: Vec<Vec<TextRange>>,
    /// Where the `ref` and the default of each of [`Body::ref_defaults`] are.
    pub ref_defaults: HashMap<LocalId, RefDefaultRanges>,
    /// Where each member initializer `F = v` of a `new` or `with`
    /// expression is written, by the expression of its value, when it has
    /// a name.
    pub inits: HashMap<ExprId, InitRanges>,
}

/// Where a member initializer `F = v` is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitRanges {
    /// The name of the member.
    pub name: TextRange,
    /// The whole initializer, from the name to the end of the value.
    pub init: TextRange,
}

/// Where a parameter with `ref` and a default value is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefDefaultRanges {
    /// The `ref` keyword.
    pub keyword: TextRange,
    /// The default value.
    pub value: TextRange,
    /// From the end of the parameter's type to the end of its default: the
    /// `=` and the value.
    pub default: TextRange,
}

impl BodySourceMap {
    /// Where `expr` is.
    pub fn expr(&self, expr: ExprId) -> TextRange {
        self.exprs[arena_index(expr)]
    }

    /// Where the name of `local` is.
    pub fn local(&self, local: LocalId) -> TextRange {
        self.locals[arena_index(local)]
    }
}

/// Every body of a file and its source map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileBodies {
    /// Every owner: the top-level statements first, then the others in the
    /// order of the text.
    owners: Vec<BodyOwner>,
    /// The body and source map of each owner.
    bodies: HashMap<BodyOwner, Lowered>,
}

/// A body and its source map, shared so that [`body`] and
/// [`body_source_map`] hand them out without copying.
type Lowered = (Arc<Body>, Arc<BodySourceMap>);

#[salsa::tracked(returns(ref))]
fn file_bodies(db: &dyn Db, file: SourceFile) -> FileBodies {
    owners::file(&parse(db, file).tree())
}

/// The body of `owner` in `file`: empty for an item without one.
#[salsa::tracked(returns(ref))]
pub fn body(db: &dyn Db, file: SourceFile, owner: BodyOwner) -> Arc<Body> {
    lowered(db, file, owner).map_or_else(Arc::default, |(body, _)| Arc::clone(body))
}

/// Where the parts of [`body`] are.
#[salsa::tracked(returns(ref))]
pub fn body_source_map(db: &dyn Db, file: SourceFile, owner: BodyOwner) -> Arc<BodySourceMap> {
    lowered(db, file, owner).map_or_else(Arc::default, |(_, map)| Arc::clone(map))
}

fn lowered(db: &dyn Db, file: SourceFile, owner: BodyOwner) -> Option<&Lowered> {
    file_bodies(db, file).bodies.get(&owner)
}

/// The owners of `file` that have a body: the top-level statements when
/// there are any, then the others in the order of the text.
pub fn body_owners(db: &dyn Db, file: SourceFile) -> Vec<BodyOwner> {
    let has_statements = item_tree(db, file).has_statements();
    file_bodies(db, file)
        .owners
        .iter()
        .copied()
        .filter(|&owner| has_statements || owner != BodyOwner::TopLevel)
        .collect()
}

#[cfg(test)]
mod tests {
    use salsa::Setter;

    use crate::test_db::{TestDb, app, codes, diagnostics_of, file_named, messages};
    use crate::*;

    #[test]
    fn a_local_function_records_the_modes_of_its_parameters() {
        let db = TestDb::new();
        let text = "F() {\n    G(ref a: int, b: int, move c: int, in d: int) {}\n}\n";
        let (_, functions) = local_functions_of_f(&db, text);
        let modes: Vec<_> = functions
            .iter()
            .map(|function| function.modes.clone())
            .collect();
        assert_eq!(
            modes,
            [vec![
                ParamMode::Ref,
                ParamMode::In,
                ParamMode::Move,
                ParamMode::In
            ]]
        );
    }

    #[test]
    fn the_modes_of_a_local_function_line_up_with_its_parameters() {
        // The parser recovers from the parameter without a name, which
        // declares no local and has no mode.
        let db = TestDb::new();
        let text = "F() {\n    G(ref a: int, move : int, move c: int) {}\n}\n";
        let (body, functions) = local_functions_of_f(&db, text);
        let found: Vec<_> = functions
            .iter()
            .map(|function| {
                function
                    .params
                    .iter()
                    .zip(&function.modes)
                    .map(|(&param, &mode)| (body.locals[param].name.clone(), mode))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            found,
            [vec![
                ("a".to_owned(), ParamMode::Ref),
                ("c".to_owned(), ParamMode::Move)
            ]]
        );
        assert_eq!(functions[0].params.len(), functions[0].modes.len());
    }

    #[test]
    fn a_local_function_is_generic_where_type_parameters_are_in_scope() {
        let db = TestDb::new();
        let text = "F() {\n    A() {}\n    B<T>() {}\n    C() {\n        D() {}\n    }\n    \
                    E<T>() {\n        G() {}\n    }\n}\n";
        assert_eq!(
            generic_local_functions(&db, text),
            [
                ("A", false),
                ("B", true),
                ("C", false),
                ("D", false),
                ("E", true),
                ("G", true),
            ]
        );
    }

    #[test]
    fn a_local_function_is_not_generic_for_the_item_around_it() {
        let db = TestDb::new();
        let text = "F<T>() {\n    G() {}\n    H<U>() {}\n}\n";
        assert_eq!(
            generic_local_functions(&db, text),
            [("G", false), ("H", true)]
        );
    }

    /// The name of each local function in the body of `F`, in name order,
    /// and whether it is generic.
    fn generic_local_functions<'db>(db: &'db TestDb, text: &str) -> Vec<(&'db str, bool)> {
        let (body, functions) = local_functions_of_f(db, text);
        let mut generic: Vec<(&str, bool)> = functions
            .iter()
            .map(|function| {
                (
                    body.locals[function.local].name.as_str(),
                    function.is_generic,
                )
            })
            .collect();
        generic.sort_unstable();
        generic
    }

    /// The body of `F` in `src/Program.mr` alone in an app, and every local
    /// function declared in it.
    fn local_functions_of_f<'db>(
        db: &'db TestDb,
        text: &str,
    ) -> (&'db Body, Vec<&'db LocalFunction>) {
        let project = app(db, &[("src/Program.mr", text)]);
        let file = file_named(db, project, "src/Program.mr");
        let tree = item_tree(db, file);
        let (f, _) = tree.items().find(|(_, item)| item.name == "F").expect("F");
        let body = body(db, file, BodyOwner::Item(f));
        let functions = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Function(function) => Some(function),
                _ => None,
            })
            .collect();
        (body, functions)
    }

    /// The HIR of `src/Program.mr` alone in an app.
    fn dump(text: &str) -> String {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        hir_dump(&db, project, file_named(&db, project, "src/Program.mr"))
    }

    /// The codes `src/Program.mr` alone in an app reports.
    fn reported(text: &str) -> Vec<String> {
        codes(&diagnostics_of(text))
    }

    #[test]
    fn locals_parameters_members_and_items_resolve() {
        let text = "public struct Point {\n    public X: int;\n    public Length(): int {\n        \
                    let d = X * X;\n        return d + Twice(X);\n    }\n}\n\
                    Twice(n: int): int => n * 2;\n\
                    let p = new Point { X = 3 };\nConsole.WriteLine(p.Length());\n";
        assert_eq!(
            dump(text),
            "top-level {\n    let p#0: Point = new Point { X = 3 };\n    \
             Console{external}.WriteLine{external}(p#0.Length());\n}\n\n\
             fn UsersApi.Point.Length() {\n    let d#0: int = X{field UsersApi.Point.X} * X{field UsersApi.Point.X};\n    \
             return d#0 + Twice{fn UsersApi.Twice}(X{field UsersApi.Point.X});\n}\n\n\
             fn UsersApi.Twice(n#0) => n#0 * 2;\n"
        );
    }

    #[test]
    fn a_deconstructing_assignment_lowers_its_places() {
        let text = "var a = 1;\nvar b = 2;\n(a, (b, _)) = (1, (2, 3));\n(a, b) += (1, 2);\n";
        assert_eq!(
            dump(text),
            "top-level {\n    var a#0: int = 1;\n    var b#1: int = 2;\n    \
             (a#0, (b#1, _)) = (1, (2, 3));\n    (a#0, b#1) += (1, 2);\n}\n"
        );
    }

    #[test]
    fn statements_and_operators_keep_their_structure() {
        let text = "var total = 0;\nfor (var i = 0; i < 10; i++) {\n    \
                    if (i % 2 == 0 && i > 2) { total += i; } else { continue; }\n}\n\
                    while (total > 5) total = total - 1;\n\
                    let s = $\"{total,4:x} items\";\n\
                    let t = match (total) { 0 => \"none\", var n when n > 1 => \"many\", _ => \"one\" };\n";
        assert_eq!(
            dump(text),
            "top-level {\n    var total#0: int = 0;\n    \
             for (var i#1: int = 0; i#1 < 10; i#1++) {\n        \
             if (((i#1 % 2) == 0) && (i#1 > 2)) {\n            total#0 += i#1;\n        \
             } else {\n            continue;\n        }\n    }\n    \
             while (total#0 > 5) total#0 = (total#0 - 1);\n    \
             let s#2: string = $\"{total#0,4:x} items\";\n    \
             let t#4 = match (total#0) {\n        0 => \"none\",\n        \
             var n#3 when n#3 > 1 => \"many\",\n        _ => \"one\",\n    };\n}\n"
        );
    }

    #[test]
    fn value_members_resolve_through_their_type() {
        let text = "enum Color { Red, Green }\n\
                    union Shape { Circle(r: float), Square(side: float);\n    \
                    static Unit(): Shape => Circle(1.0);\n}\n\
                    class Counter {\n    private _count: int;\n    public const Max: int = 3;\n    \
                    public Count: int => _count;\n    public Bump() { _count = _count + Max; }\n}\n\
                    let c = Color.Red;\nlet m = Counter.Max;\n";
        let dumped = dump(text);
        assert!(
            dumped.contains(
                "let c#0 = Color{enum UsersApi.Color}.Red{enum member UsersApi.Color.Red};"
            ),
            "{dumped}"
        );
        assert!(
            dumped.contains(
                "let m#1 = Counter{class UsersApi.Counter}.Max{const UsersApi.Counter.Max};"
            ),
            "{dumped}"
        );
        assert!(
            dumped.contains("=> Circle{case UsersApi.Shape.Circle}(1.0)"),
            "{dumped}"
        );
        assert!(
            dumped.contains("_count{field UsersApi.Counter._count} = (_count{field UsersApi.Counter._count} + Max{const UsersApi.Counter.Max});"),
            "{dumped}"
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_block_statement_records_its_closing_brace() {
        let db = TestDb::new();
        let text = "{\n    let x = 1;\n}\nlet y = 2;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let ends: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(id, _)| map.block_ends.get(&id))
            .map(|range| &text[std::ops::Range::<usize>::from(*range)])
            .collect();
        assert_eq!(ends, ["}"]);
        let block = body
            .stmts
            .iter()
            .find(|(_, stmt)| matches!(stmt, Stmt::Block(_)))
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(u32::from(map.block_ends[&block].start()), 17);
    }

    #[test]
    fn an_unsafe_block_records_its_closing_brace() {
        let db = TestDb::new();
        let text = "unsafe {\n    let x = 1;\n}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let block = body
            .stmts
            .iter()
            .find(|(_, stmt)| matches!(stmt, Stmt::Block(_)))
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(
            map.block_ends
                .get(&block)
                .map(|range| u32::from(range.start())),
            Some(24)
        );
    }

    #[test]
    fn the_dump_shows_the_never_type_as_an_exclamation_mark() {
        assert_eq!(
            dump("let x = Todo();\n"),
            "top-level {\n    let x#0: ! = Todo{external}();\n}\n"
        );
    }

    #[test]
    fn a_statement_records_its_range() {
        let db = TestDb::new();
        let text = "var i = 0;\nwhile (i < 3) i++;\nif (i > 1) { i--; }\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let ranges: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(id, _)| map.stmts.get(&id))
            .map(|range| &text[std::ops::Range::<usize>::from(*range)])
            .collect();
        // Every statement but a local declaration, inner ones first.
        assert_eq!(
            ranges,
            [
                "i++;",
                "while (i < 3) i++;",
                "i--;",
                "{ i--; }",
                "if (i > 1) { i--; }"
            ]
        );
    }

    #[test]
    fn interpolated_text_keeps_its_value() {
        let db = TestDb::new();
        let text = "let n = 1;\nlet a = $\"x\\t{{{n}}}\";\nlet b = $`\\q {n}`;\nlet c = $`\n    a {n}\n      b\n    `;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let values: Vec<Vec<(String, Option<String>)>> = body
            .exprs
            .iter()
            .filter_map(|(_, expr)| match expr {
                Expr::Interpolated(parts) => Some(
                    parts
                        .iter()
                        .filter_map(|part| match part {
                            InterpPart::Text { text, value } => Some((text.clone(), value.clone())),
                            InterpPart::Hole { .. } => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        let part = |text: &str, value: &str| (text.to_owned(), Some(value.to_owned()));
        assert_eq!(
            values,
            [
                vec![part("x\\t{{", "x\t{"), part("}}", "}")],
                vec![part("\\q ", "\\q ")],
                vec![part("\n    a ", "a "), part("\n      b\n    ", "\n  b")],
            ]
        );
    }

    #[test]
    fn a_private_field_is_not_found_from_outside_its_type() {
        let text = "class Counter {\n    private _count: int;\n}\nlet n = Counter._count;\n";
        assert_eq!(reported(text), ["MR0127"]);
    }

    #[test]
    fn segments_after_a_value_are_member_accesses() {
        let dumped = dump("enum Color { Red }\nlet n = Color.Red.Name.Length;\n");
        assert!(
            dumped.contains(
                "Color{enum UsersApi.Color}.Red{enum member UsersApi.Color.Red}.Name.Length"
            ),
            "{dumped}"
        );
    }

    #[test]
    fn every_kind_of_redeclaration_is_shadowing() {
        assert_eq!(reported("let x = 1;\nlet x = 2;\n"), ["MR0180"]);
        assert_eq!(reported("let x = 1;\n{\n    let x = 2;\n}\n"), ["MR0180"]);
        assert_eq!(reported("F(x: int) {\n    let x = 2;\n}\n"), ["MR0180"]);
        assert_eq!(reported("F(x: int, x: int) {}\n"), ["MR0180"]);
        assert_eq!(reported("let x = 1;\nlet f = (x: int) => x;\n"), ["MR0180"]);
        assert_eq!(
            reported("let x = 1;\nG(x: int): int => x;\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            reported("F() {\n    let x = 1;\n    G(x: int): int => x;\n}\n"),
            ["MR0180"]
        );
        assert_eq!(
            reported("let p = (1, 2);\nlet ok = p is (let a, let a);\n"),
            ["MR0180"]
        );
        assert_eq!(
            reported("let xs = [1];\nlet x = 0;\nforeach (let x in xs) {}\n"),
            ["MR0180"]
        );
    }

    #[test]
    fn blocks_side_by_side_may_reuse_a_name() {
        let text = "let ok = true;\nif (ok) {\n    let total = 1;\n} else {\n    let total = 2;\n}\n\
                    let total = 3;\nfor (var i = 0; i < 2; i++) {}\nfor (var i = 0; i < 2; i++) {}\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn the_message_names_the_parameter_that_is_shadowed() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[("src/Program.mr", "F(x: int) {\n    var x = 1;\n}\n")],
        );
        assert_eq!(
            messages(diagnostics(&db, project)),
            ["`x` is already the name of a parameter in scope"]
        );
    }

    #[test]
    fn is_bindings_end_with_their_statement() {
        let text =
            "let o = 1;\nif (o is var v && v > 0) {\n    Console.WriteLine(v);\n}\nlet v = 2;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let text = "let o = 1;\nif (o is not var v) {\n    return;\n}\nConsole.WriteLine(v);\n";
        assert_eq!(reported(text), ["MR0127"]);
    }

    #[test]
    fn an_initializer_does_not_see_its_own_local() {
        assert_eq!(reported("let x = x;\n"), ["MR0127"]);
    }

    #[test]
    fn a_local_function_can_call_itself_but_not_be_called_before_it() {
        let text = "Count(n: int): int => n == 0 ? 0 : Count(n - 1);\n";
        let dumped = dump(&format!("F() {{\n    {text}}}\n"));
        assert!(
            dumped.contains("Count#0(n#1): int => (n#1 == 0) ? 0 : Count#0(n#1 - 1);"),
            "{dumped}"
        );
        assert_eq!(
            reported("F() {\n    Later();\n    Later() {}\n}\n"),
            ["MR0127"]
        );
    }

    #[test]
    fn unknown_names_suggest_locals_and_members() {
        let db = TestDb::new();
        let text = "struct Point {\n    X: int;\n    F(total: int): int => totl + XX;\n}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let fixes: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| d.fixes.first().map(|fix| fix.message.clone()))
            .collect();
        assert_eq!(
            fixes,
            [
                Some("did you mean `total`?".to_owned()),
                Some("did you mean `X`?".to_owned())
            ]
        );
    }

    #[test]
    fn captures_must_name_locals() {
        assert_eq!(
            reported(
                "let conn = 1;\nlet f = [move conn] () => conn;\nlet g = [weak this] () => 1;\nlet h = [move other] () => 1;\n"
            ),
            ["MR0127"]
        );
    }

    #[test]
    fn every_declaration_with_code_has_a_body() {
        let text = "class Counter {\n    private _count: int = 1;\n    \
                    public Count: int => _count;\n    \
                    public Counter(start: int = 2) : this(start, 0) { _count = start; }\n    \
                    public Counter(start: int, extra: int) { _count = start + extra; }\n    \
                    ~Counter() { _count = 0; }\n    \
                    public this[i: int]: int { get => _count + i; set { _count = value; } }\n}\n\
                    struct Money {\n    Cents: int;\n    \
                    operator +(a: Money, b: Money): Money => new Money { Cents = a.Cents + b.Cents };\n}\n\
                    enum Level { Low = 1, High = Low + 1 }\n\
                    const Limit: int = 10;\n\
                    extension (m: Money) {\n    Doubled(): Money => m + m;\n    Zero: bool => m.Cents == 0;\n}\n";
        assert_eq!(
            dump(text),
            "init UsersApi.Counter._count => 1;\n\n\
             get UsersApi.Counter.Count => _count{field UsersApi.Counter._count};\n\n\
             new UsersApi.Counter(start#0 = 2) : this(start#0, 0) {\n    \
             _count{field UsersApi.Counter._count} = start#0;\n}\n\n\
             new UsersApi.Counter(start#0, extra#1) {\n    \
             _count{field UsersApi.Counter._count} = (start#0 + extra#1);\n}\n\n\
             drop UsersApi.Counter() {\n    _count{field UsersApi.Counter._count} = 0;\n}\n\n\
             get UsersApi.Counter.this[](i#0) => _count{field UsersApi.Counter._count} + i#0;\n\n\
             set UsersApi.Counter.this[](i#0, value#1) {\n    \
             _count{field UsersApi.Counter._count} = value#1;\n}\n\n\
             operator UsersApi.Money.+(a#0, b#1) => new Money { Cents = a#0.Cents + b#1.Cents };\n\n\
             init UsersApi.Level.Low => 1;\n\n\
             init UsersApi.Level.High => Low{enum member UsersApi.Level.Low} + 1;\n\n\
             init UsersApi.Limit => 10;\n\n\
             fn extension#0.Doubled(m#0) => m#0 + m#0;\n\n\
             get extension#0.Zero(m#0) => m#0.Cents == 0;\n"
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn types_outside_the_item_signatures_are_checked_once() {
        let text = "class A {\n    F: Strng;\n    A(x: Strng) {}\n    G: Strng => \"\";\n}\n\
                    struct S {\n    X: Strng;\n}\n\
                    extension (s: Strng) {\n    H(): Strng => s;\n}\n";
        assert_eq!(reported(text), ["MR0127"; 6]);
    }

    #[test]
    fn a_setter_value_and_constructor_parameters_are_locals() {
        assert_eq!(
            reported(
                "class A {\n    A(x: int) { let x = 1; }\n    public this[i: int]: int { get => 1; set { let value = 2; } }\n}\n"
            ),
            ["MR0180", "MR0180"]
        );
        // A default cannot read a parameter.
        assert_eq!(reported("F(a: int, b: int = a) {}\n"), ["MR0127"]);
    }

    #[test]
    fn shared_parameters_report_their_defaults_once() {
        let indexer = "class A {\n    this[i: int = Unknwn]: int { get => 1; set {} }\n}\n";
        assert_eq!(reported(indexer), ["MR0127"]);
        let receiver = "struct M { C: int; }\nextension (m: M = zz) {\n    A(): int => 1;\n    B(): int => 2;\n}\n";
        assert_eq!(reported(receiver), ["MR0127"]);
    }

    #[test]
    fn an_extension_member_default_cannot_read_the_receiver() {
        let text = "struct M { C: int; }\nextension (m: M) {\n    F(x: int = m.C): int => x;\n}\n";
        assert_eq!(reported(text), ["MR0127"]);
    }

    #[test]
    fn an_indexer_without_accessors_and_union_case_fields_check_their_types() {
        assert_eq!(
            reported("class A {\n    this[i: Strng]: Strng { }\n}\n"),
            ["MR0127", "MR0127"]
        );
        assert_eq!(reported("union U { C(x: Flaot), D }\n"), ["MR0127"]);
    }

    #[test]
    fn a_capture_without_a_name_reports_only_the_syntax_error() {
        assert_eq!(reported("let f = [move] () => 1;\n"), Vec::<String>::new());
    }

    #[test]
    fn a_generic_local_function_sees_its_type_parameters() {
        assert_eq!(
            reported("F() {\n    Id<T>(x: T): T => x;\n    let y: T = 1;\n}\n"),
            ["MR0127"]
        );
    }

    #[test]
    fn types_in_bodies_are_resolved() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "let x: strng = \"a\";\nlet n = (Missing)x;\n",
            )],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0127", "MR0127"]);
        assert_eq!(reported[0].fixes[0].message, "did you mean `string`?");
    }

    #[test]
    fn item_nodes_follow_the_item_tree() {
        let db = TestDb::new();
        let text = "class A {\n    F() {}\n    IShape.Area(): float => 1.0;\n    X: int = 1;\n    \
                    struct B { G() {} }\n}\n\
                    extension (a: A) {}\nH() {}\nconst K: int = 1;\nlet top = 1;\n";
        let file = db.file("src/A.mr", text);
        let nodes = crate::items::item_nodes(&parse(&db, file).tree());
        let tree = item_tree(&db, file);
        assert_eq!(nodes.len(), tree.items().count());
        let owners: Vec<_> = body_owners(&db, file)
            .into_iter()
            .map(|owner| match owner {
                BodyOwner::Item(item) => tree.item(item).name.clone(),
                BodyOwner::TopLevel => "top".to_owned(),
                BodyOwner::Value(ty, index) => format!("{}.{index}", tree.item(ty).name),
                BodyOwner::Member(ty, index) => format!("{}#{index}", tree.item(ty).name),
                BodyOwner::Extension(index, member) => format!("extension{index}.{member}"),
            })
            .collect();
        assert_eq!(
            owners,
            ["top", "F", "A#0", "A.0", "G", "extension0.0", "H", "K"]
        );
    }

    #[test]
    fn union_values_count_every_named_case() {
        let db = TestDb::new();
        let text = "union U { C(x: int), D, E(y: int);\n    const K: int = 1;\n}\n";
        let file = db.file("src/A.mr", text);
        let union = ItemIdx::from_raw(la_arena::RawIdx::from_u32(0));
        // `D` has no fields to check, so no body, but it is value 1.
        assert_eq!(
            body_owners(&db, file),
            [
                BodyOwner::Value(union, 0),
                BodyOwner::Value(union, 2),
                BodyOwner::Value(union, 3),
            ]
        );
    }

    #[test]
    fn an_edit_inside_one_body_resolves_only_that_body_again() {
        let mut db = TestDb::new();
        let text = |n: u32| {
            format!(
                "A(): int {{\n    return {n};\n}}\nB(): int {{\n    let y = 2;\n    return y;\n}}\n"
            )
        };
        let project = app(&db, &[("src/Program.mr", &text(1))]);
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(diagnostics(&db, project), &[]);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        assert_eq!(diagnostics(&db, project), &[]);
        let resolved = db
            .take_executed()
            .iter()
            .filter(|key| key.starts_with("body_resolution("))
            .count();
        assert_eq!(resolved, 1);
    }

    #[test]
    fn every_construct_lowers_and_prints() {
        let text = r#"public struct P {
    public X: int;
    public Y: int;
}
union Shape { Circle(r: float), Rect(w: float, h: float) }
Use(a: int): int => a;
Use(a: string): int => 0;
Use(ref a: int, in b: int, move c: string?): int => 0;
Id<T>(x: T): T => x;
G(o: Shape, p: P, xs: int[], n: int, s: string?, flag: bool) {
    var i = 0;
    var j = 1;
    for (i = 0, j = 1; i < n; i++, j--) {}
    let (a, b) = (1, 2);
    @Obsolete let marked = 1;
    let lits = (1, 2.5, 'c', "s", true, false, none);
    let one = (n,);
    let named = (Left: a, Right: b);
    let ops = (-n, !flag, ~n, ++i, --i, ^1, +n);
    let bins = (n / 2, n *% 2, n + 1, n +% 1, n -% 1, n << 1, n >> 1, n >>> 1, n <= 1, n >= 1, n != 1, n & 1, n ^ 1, n | 1, flag || flag, s ?? "d");
    i -= 1;
    i *= 2;
    i /= 2;
    i %= 2;
    i &= 1;
    i |= 1;
    i ^= 1;
    i <<= 1;
    i >>= 1;
    i >>>= 1;
    s ??= "x";
    i +%= 1;
    i -%= 1;
    i *%= 1;
    Use(ref i, in n, move s);
    let g = Id<int>(1);
    let m = p.Get<int, string>(name: 1);
    let f = [weak this, move p] async (u: int, v: int) => u + v;
    let st = static () => 1;
    let all = [1, ..xs];
    let q = new P { X = 1, Y = 2 };
    let k = match (o) {
        Shape.Circle(let r) => 1,
        Shape.Rect(> 0.0 and < 1.0, not 2.0 or 3.0) => 2,
        _ => 0,
    };
    let lp = xs is [1, .. var rest] && p is P { X: 1, Y: _ } pq;
    let e = xs?[0];
    let w = q with { X = 3, Y = 4 };
    let c = (long)n as long;
    let arr = new int[n];
    let sa = stackalloc int[2];
    let d = default(int);
    let nm = nameof(n);
    let sz = sizeof(int);
    let r = 1..n;
    let str = $"{n,4:x} {flag}";
    foreach (ref var x in xs) {}
    do { i++; } while (i < 3);
    unsafe { i = 0; }
    Pair(left: int, right: int): int => left + right;
}
"#;
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            dump(text),
            r#"fn UsersApi.Use(a#0) => a#0;

fn UsersApi.Use(a#0) => 0;

fn UsersApi.Use(a#0, b#1, c#2) => 0;

fn UsersApi.Id(x#0) => x#0;

fn UsersApi.G(o#0, p#1, xs#2, n#3, s#4, flag#5) {
    var i#6: int = 0;
    var j#7: int = 1;
    for (i#6 = 0, j#7 = 1; i#6 < n#3; i#6++, j#7--) {}
    let (let a#8, let b#9) = (1, 2);
    let marked#10: int = 1;
    let lits#11 = (1, 2.5, 'c', "s", true, false, none);
    let one#12: (int,) = (n#3,);
    let named#13 = (Left: a#8, Right: b#9);
    let ops#14 = (-n#3, !flag#5, ~n#3, ++i#6, --i#6, ^1, +n#3);
    let bins#15 = (n#3 / 2, n#3 *% 2, n#3 + 1, n#3 +% 1, n#3 -% 1, n#3 << 1, n#3 >> 1, n#3 >>> 1, n#3 <= 1, n#3 >= 1, n#3 != 1, n#3 & 1, n#3 ^ 1, n#3 | 1, flag#5 || flag#5, s#4 ?? "d");
    i#6 -= 1;
    i#6 *= 2;
    i#6 /= 2;
    i#6 %= 2;
    i#6 &= 1;
    i#6 |= 1;
    i#6 ^= 1;
    i#6 <<= 1;
    i#6 >>= 1;
    i#6 >>>= 1;
    s#4 ??= "x";
    i#6 +%= 1;
    i#6 -%= 1;
    i#6 *%= 1;
    Use{overloads UsersApi.Use}(ref i#6, in n#3, move s#4);
    let g#16 = Id<int>{fn UsersApi.Id}(1);
    let m#17 = p#1.Get<int, string>(name: 1);
    let f#20 = [weak this, move p#1] async (u#18, v#19) => u#18 + v#19;
    let st#21 = static () => 1;
    let all#22 = [1, ..xs#2];
    let q#23: P = new P { X = 1, Y = 2 };
    let k#25 = match (o#0) {
        Shape.Circle{case UsersApi.Shape.Circle}(let r#24) => 1,
        Shape.Rect{case UsersApi.Shape.Rect}((> 0.0 and < 1.0), (not 2.0 or 3.0)) => 2,
        _ => 0,
    };
    let lp#28: bool = (xs#2 is [1, .. var rest#26]) && (p#1 is P {X: 1, Y: _} let pq#27);
    let e#29 = xs#2?[0];
    let w#30: P = q#23 with { X = 3, Y = 4 };
    let c#31 = ((long)n#3) as long;
    let arr#32 = new int[n#3];
    let sa#33 = stackalloc int[2];
    let d#34 = default(int);
    let nm#35: string = nameof(n#3);
    let sz#36: int = sizeof(int);
    let r#37 = 1..n#3;
    let str#38: string = $"{n#3,4:x} {flag#5}";
    foreach (ref var x#39 in xs#2) {}
    do {
        i#6++;
    } while (i#6 < 3);
    unsafe {
        i#6 = 0;
    }
    Pair#40(left#41, right#42): int => left#41 + right#42;
}
"#
        );
    }

    #[test]
    fn members_outside_the_item_tree_lower_and_print() {
        let text = r#"interface IShape {
    Area(): float;
    Name: string;
}
class Square {
    private _side: float;
    IShape.Area<T>(scale: T, extra: float): float {
        return _side * extra;
    }
    IShape.Name: string => "square";
    ~Square() => Release(_side);
    public operator +(a: Square, b: Square): Square {
        return a;
    }
    public this[i: int]: float => _side * i;
    public this[name: string]: float {
        get => _side;
        set { _side = value; }
    }
}
Release(x: float) {}
extension<T> (items: T[]) {
    public First<U>(fallback: U, other: T): T {
        return other;
    }
    public Empty: T => items[0];
}
"#;
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            dump(text),
            r#"fn UsersApi.Square.IShape.Area(scale#0, extra#1) {
    return _side{field UsersApi.Square._side} * extra#1;
}

get UsersApi.Square.IShape.Name => "square";

drop UsersApi.Square() => Release{fn UsersApi.Release}(_side{field UsersApi.Square._side});

operator UsersApi.Square.+(a#0, b#1) {
    return a#0;
}

get UsersApi.Square.this[](i#0) => _side{field UsersApi.Square._side} * i#0;

get UsersApi.Square.this[](name#0) => _side{field UsersApi.Square._side};

set UsersApi.Square.this[](name#0, value#1) {
    _side{field UsersApi.Square._side} = value#1;
}

fn UsersApi.Release(x#0) {}

fn extension#0.First(items#0, fallback#1, other#2) {
    return other#2;
}

get extension#0.Empty(items#0) => items#0[0];
"#
        );
    }

    #[test]
    fn member_types_are_checked_in_every_position() {
        let text = "interface IShape {\n    Area(x: int): float;\n}\n\
                    class Sq {\n    IShape.Area(x: Strng): Flt { return 1.0; }\n    \
                    public operator +(a: Sq, b: Strng): Flt { return 1.0; }\n    \
                    public this[i: Strng]: Flt => 1.0;\n    \
                    public this[n: Bool]: Flt { get => 1.0; set {} }\n}\n\
                    extension (s: Sq) {\n    public G: Strng => \"\";\n}\n";
        assert_eq!(reported(text), ["MR0127"; 9]);
    }

    /// The codes a project of several files reports.
    fn reported_in(files: &[(&str, &str)]) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, files);
        codes(diagnostics(&db, project))
    }

    #[test]
    fn value_members_follow_their_visibility() {
        let store = "public class Store {\n    private const Secret: int = 1;\n    \
                     file const Local: int = 2;\n    const Folder: int = 3;\n    \
                     internal const Project: int = 4;\n    \
                     static Inside(): int => Store.Secret + Store.Local;\n}\n";
        // Inside the type, the file and the folder, all of them but `Secret`
        // from another type are visible.
        let same = "public class Other {\n    \
                    static F(): int => Store.Local + Store.Folder + Store.Project;\n}\n";
        assert_eq!(
            reported_in(&[
                ("src/Program.mr", "let x = 1;\n"),
                ("src/Store.mr", &format!("{store}{same}"))
            ]),
            Vec::<String>::new()
        );
        let hidden = "public class Other {\n    static F(): int => Store.Secret;\n}\n";
        assert_eq!(
            reported_in(&[
                ("src/Program.mr", "let x = 1;\n"),
                ("src/Store.mr", &format!("{store}{hidden}"))
            ]),
            ["MR0127"]
        );
        // Another file of the folder sees `Folder` and `Project` but not `Local`.
        let other_file =
            "class Use {\n    static F(): int => Store.Folder + Store.Project + Store.Local;\n}\n";
        assert_eq!(
            reported_in(&[
                ("src/Program.mr", "let x = 1;\n"),
                ("src/Store.mr", store),
                ("src/Use.mr", other_file),
            ]),
            ["MR0127"]
        );
        // Another folder sees only `Project`.
        let other_folder =
            "class Use {\n    static F(): int => App.Store.Project + App.Store.Folder;\n}\n";
        let reported = reported_in(&[
            ("src/Program.mr", "let x = 1;\n"),
            ("src/Store.mr", store),
            (
                "src/Feature/Use.mr",
                &other_folder.replace("App.", "UsersApi."),
            ),
        ]);
        assert_eq!(reported, ["MR0127"]);
    }

    #[test]
    fn an_interface_getter_implementation_is_not_a_value_member() {
        let text = "interface IShape {\n    Name: string;\n}\n\
                    class Box {\n    IShape.Name: string => \"box\";\n    Name: int;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn source_maps_point_at_names_and_expressions() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "let total = 1 + 2;\n")]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let text = file.text(&db);
        let (local, _) = body.locals.iter().next().unwrap();
        assert_eq!(&text[map.local(local)], "total");
        let sum = body
            .exprs
            .iter()
            .find(|(_, expr)| matches!(expr, Expr::Binary { .. }))
            .unwrap()
            .0;
        assert_eq!(&text[map.expr(sum)], "1 + 2");
    }

    #[test]
    fn source_maps_keep_jumps_lets_and_closing_braces() {
        let db = TestDb::new();
        let text = "while (true) {\n    break;\n}\nlet a = 1;\nvar b = 2;\nlet (c, d) = (1, 2);\n\
                    foreach (let e in [1]) {\n    continue;\n}\nif (1 is let f) {}\n\
                    if (1 is int g) {}\nF(): int {\n    G(): int => 1;\n    return 1;\n}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let text = file.text(&db);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let body = body(&db, file, BodyOwner::TopLevel);
        let mut jumps: Vec<_> = jump_ranges(body, map)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        jumps.sort_unstable();
        assert_eq!(jumps, ["break;", "continue;"]);
        let mut lets: Vec<_> = map
            .lets
            .iter()
            .map(|(&local, &range)| (body.locals[local].name.as_str(), &text[range]))
            .collect();
        lets.sort_unstable();
        assert_eq!(lets, [("a", "let"), ("e", "let"), ("f", "let")]);
        // Only a function body written as a block keeps its closing brace.
        assert!(map.ends.is_empty());
        let owner = body_owners(&db, file)
            .into_iter()
            .find(|&owner| owner != BodyOwner::TopLevel)
            .unwrap();
        let function = body_source_map(&db, file, owner);
        let end = mora_span::TextSize::of(text.trim_end()) - mora_span::TextSize::of('}');
        let ends: Vec<_> = function.ends.values().map(|range| range.start()).collect();
        assert_eq!(ends, [end]);
        let jumps: Vec<_> = jump_ranges(crate::body(&db, file, owner), function)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(jumps, ["return 1;"]);
    }

    /// The ranges of the `break`, `continue` and `return` statements.
    fn jump_ranges(body: &Body, map: &BodySourceMap) -> Vec<mora_span::TextRange> {
        body.stmts
            .iter()
            .filter(|(_, stmt)| matches!(stmt, Stmt::Break | Stmt::Continue | Stmt::Return(_)))
            .map(|(id, _)| map.stmts[&id])
            .collect()
    }

    #[test]
    fn a_body_keeps_the_return_type_no_signature_holds() {
        let db = TestDb::new();
        let text = "interface I {\n    M(): int;\n}\nclass C : I {\n    I.M(): int => 1;\n}\n\
                    F(): int => 1;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let returns: Vec<_> = body_owners(&db, file)
            .into_iter()
            .filter_map(|owner| {
                let body = body(&db, file, owner);
                let name = match owner {
                    BodyOwner::Item(item) => item_tree(&db, file).item(item).name.clone(),
                    BodyOwner::Member(..) => body.name.clone(),
                    _ => return None,
                };
                let resolution = body_resolution(&db, project, file, owner);
                Some((name, body.ret.map(|ret| resolution.written(ret))))
            })
            .collect();
        // An item's signature holds its return type; the member's body does.
        assert_eq!(
            returns,
            [
                ("M".to_owned(), None),
                ("I.M".to_owned(), Some(Ty::Primitive(Primitive::Int))),
                ("F".to_owned(), None),
            ]
        );
    }

    #[test]
    fn a_recovered_return_has_a_missing_value() {
        assert_eq!(
            dump("F(): int {\n    return\n}\nG() {\n    return;\n}\n"),
            "fn UsersApi.F() {\n    return <missing>;\n}\n\nfn UsersApi.G() {\n    return;\n}\n"
        );
    }

    #[test]
    fn literals_keep_their_kind() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "let x = (1, 2.5, 'c', \"s\", true, false, none);\n",
            )],
        );
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let literals: Vec<_> = body
            .exprs
            .iter()
            .filter_map(|(_, expr)| match expr {
                Expr::Literal(literal) => Some(literal.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            literals,
            [
                Literal::Int("1".to_owned()),
                Literal::Real("2.5".to_owned()),
                Literal::Char("'c'".to_owned()),
                Literal::String("\"s\"".to_owned()),
                Literal::Bool(true),
                Literal::Bool(false),
                Literal::None,
            ]
        );
    }

    #[test]
    fn each_value_member_and_expression_body_has_its_own_owner() {
        let text = "interface IShape {\n    Area(): float;\n}\n\
                    class Two {\n    private _a: int = 1;\n    private _b: int = 2;\n    \
                    public A: int => _a;\n    public B: int => _b;\n    \
                    public Two(x: int) => _a = x;\n    IShape.Area(): float => 1.0;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            dump(text),
            "init UsersApi.Two._a => 1;\n\n\
             init UsersApi.Two._b => 2;\n\n\
             get UsersApi.Two.A => _a{field UsersApi.Two._a};\n\n\
             get UsersApi.Two.B => _b{field UsersApi.Two._b};\n\n\
             new UsersApi.Two(x#0) => _a{field UsersApi.Two._a} = x#0;\n\n\
             fn UsersApi.Two.IShape.Area() => 1.0;\n"
        );
    }

    #[test]
    fn an_indexer_with_one_accessor_checks_its_types() {
        assert_eq!(
            reported("class A {\n    this[n: Bool]: Flt { get => 1.0; }\n}\n"),
            ["MR0127", "MR0127"]
        );
    }

    #[test]
    fn using_static_opens_static_value_members() {
        let text = "using static UsersApi.Counter;\n\
                    public class Counter {\n    public const Max: int = 3;\n    public Count: int => 1;\n}\n\
                    let n = Max;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert!(dump(text).contains("let n#0 = Max{const UsersApi.Counter.Max};"));
        // An instance member is not opened.
        assert_eq!(reported(&format!("{text}let c = Count;\n")), ["MR0127"]);
        // Two types that both have it make it ambiguous.
        let two = "using static UsersApi.A;\nusing static UsersApi.B;\n\
                   public class A {\n    public const Max: int = 1;\n}\n\
                   public class B {\n    public const Max: int = 2;\n}\nlet n = Max;\n";
        assert_eq!(reported(two), ["MR0128"]);
    }

    #[test]
    fn local_function_and_lambda_defaults_are_printed() {
        let dumped =
            dump("F() {\n    G(x: int = 1): int => x;\n    let h = (y: int = 2) => y;\n}\n");
        assert!(dumped.contains("G#0(x#1 = 1): int => x#1;"), "{dumped}");
        assert!(dumped.contains("let h#3 = (y#2 = 2) => y#2;"), "{dumped}");
    }

    #[test]
    fn item_text_names_an_item_and_its_kind() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "struct P {\n    public X: int;\n}\n\nFib(n: int): int => n;\n",
            )],
        );
        let file = file_named(&db, project, "src/Program.mr");
        let tree = item_tree(&db, file);
        let texts: Vec<String> = tree
            .items()
            .map(|(item, _)| {
                item_text(
                    &db,
                    ItemId {
                        project,
                        file,
                        item,
                    },
                )
            })
            .collect();
        assert!(
            texts.contains(&"UsersApi.P{struct UsersApi.P}".to_owned()),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"UsersApi.Fib{fn UsersApi.Fib}".to_owned()),
            "{texts:?}"
        );
    }

    #[test]
    fn escape_char_escapes_quotes_controls_and_invisible_characters() {
        let escaped = |ch: char, quote: char| {
            let mut out = String::new();
            escape_char(ch, quote, &mut out);
            out
        };
        assert_eq!(escaped('a', '"'), "a");
        assert_eq!(escaped('é', '"'), "é");
        assert_eq!(escaped('"', '"'), "\\\"");
        assert_eq!(escaped('"', '\''), "\"");
        assert_eq!(escaped('\'', '\''), "\\'");
        assert_eq!(escaped('\\', '"'), "\\\\");
        assert_eq!(escaped('\n', '"'), "\\n");
        assert_eq!(escaped('\r', '"'), "\\r");
        assert_eq!(escaped('\t', '"'), "\\t");
        assert_eq!(escaped('\0', '"'), "\\0");
        assert_eq!(escaped('\u{1}', '"'), "\\u{1}");
        for ch in [
            '\u{a0}', '\u{ad}', '\u{34f}', '\u{61c}', '\u{115f}', '\u{1160}', '\u{17b4}',
            '\u{17b5}', '\u{180b}', '\u{180f}', '\u{2000}', '\u{200b}', '\u{200f}', '\u{2028}',
            '\u{202e}', '\u{202f}', '\u{205f}', '\u{2060}', '\u{206f}', '\u{3000}', '\u{3164}',
            '\u{fe00}', '\u{fe0f}', '\u{feff}', '\u{ffa0}', '\u{fff9}', '\u{fffb}',
        ] {
            assert_eq!(escaped(ch, '"'), format!("\\u{{{:x}}}", u32::from(ch)));
        }
        // The neighbours of the ranges stay.
        for ch in [
            '\u{a1}',
            '\u{1161}',
            '\u{1ffff}',
            '\u{2010}',
            '\u{2027}',
            '\u{2070}',
            '\u{3001}',
            '\u{fe10}',
            '\u{fffc}',
        ] {
            assert_eq!(escaped(ch, '"'), ch.to_string());
        }
    }

    /// Every prefix of `text`, cut at each character, lowers and reports
    /// without panicking: the parser recovers from each, and lowering must
    /// handle whatever it leaves.
    fn every_prefix_lowers(text: &str) {
        for (cut, _) in text.char_indices() {
            let db = TestDb::new();
            let project = app(&db, &[("src/Program.mr", &text[..cut])]);
            let file = file_named(&db, project, "src/Program.mr");
            let _ = hir_dump(&db, project, file);
            let _ = diagnostics(&db, project);
        }
    }

    #[test]
    fn broken_code_lowers_without_panicking() {
        every_prefix_lowers(
            "class C {\n    private _x: int = 1;\n    public X: int => _x;\n    \
             public C(a: int = 2) : this(a) { _x = a; }\n    ~C() {}\n    \
             public operator +(a: C, b: C): C => a;\n    \
             public this[i: int]: int { get => i; set { _x = value; } }\n    \
             IShape.Area(): float => 1.0;\n}\n\
             enum E { A = 1 }\nunion U { K(x: int) }\n\
             extension (c: C) {\n    F(): int => c.X;\n}\n\
             G(o: U, xs: int[]) {\n    let (a, _) = (1, 2);\n    \
             foreach (let (p, q) in xs) {}\n    \
             match (o) { U.K(let k) when k > 0 => { return; } _ => {} }\n    \
             let f = [move a] async (x: int = 1) => await x?;\n    \
             H<T>(t: T): T => t;\n    if (o is U.K(var z) and not null) {}\n}\n",
        );
    }

    #[test]
    fn rarer_constructs_and_every_kind_of_target_print() {
        let text = "public struct P {\n    public X: int;\n}\n\
                    union Shape { Circle(r: float) }\n\
                    interface IShape {\n    Area(): float;\n}\n\
                    macro M(x: Expr) {\n}\n\
                    const Limit: int = 3;\n\
                    class Box {\n    public Count: int => 1;\n    \
                    public Twice(): int => Helper() * 2;\n    Helper(): int => Count;\n}\n\
                    F(o: Shape) {\n    \
                    let t: (int, Name: string) = (1, \"a\");\n    \
                    let g: (int) => string = x => \"\";\n    \
                    let a: int[] = [];\n    let s: string? = none;\n    \
                    let w: weak Box = this;\n    let i: any IShape = this;\n    \
                    let l: List<int> = [];\n    \
                    let ns = UsersApi;\n    let p = P;\n    let u = Shape;\n    \
                    let sh = IShape;\n    let m = M;\n    let lim = Limit;\n    \
                    let n = int.Parse(\"1\");\n    let skipped = F.Name;\n    \
                    match (o) {\n        UsersApi.Shape.Circle(let r) => { return; }\n        \
                    Shape s1 => {}\n        Shape => {}\n        (_) => {}\n    }\n    \
                    foreach (let (x, _) in [(1, 2)]) {}\n    let e = unknown;\n}\n";
        let dumped = dump(text);
        for expected in [
            "let t#1: (int, Name: string) = (1, \"a\");",
            "let g#3: (int) => string = (x#2) => \"\";",
            "let a#4: int[] = [];",
            "let s#5: string? = none;",
            "let w#6: weak Box = this;",
            "let i#7: any IShape = this;",
            "let l#8: List<int> = [];",
            "let ns#9 = UsersApi{namespace UsersApi};",
            "let p#10 = P{struct UsersApi.P};",
            "let u#11 = Shape{union UsersApi.Shape};",
            "let sh#12 = IShape{interface UsersApi.IShape};",
            "let m#13 = M{macro UsersApi.M};",
            "let lim#14: int = Limit{value UsersApi.Limit};",
            "let n#15 = int.Parse(\"1\");",
            "let skipped#16 = F{fn UsersApi.F}.Name;",
            "UsersApi.Shape.Circle{case UsersApi.Shape.Circle}(let r#17) => {",
            "Shape s1#18 => {},",
            "Shape{union UsersApi.Shape} => {},",
            "_ => {},",
            "foreach ((let x#19, _) in [(1, 2)]) {}",
            "let e#20 = unknown{error};",
            "fn UsersApi.Box.Twice() => Helper{fn UsersApi.Box.Helper}() * 2;",
            "fn UsersApi.Box.Helper() => Count{getter UsersApi.Box.Count};",
        ] {
            assert!(dumped.contains(expected), "{expected}\n{dumped}");
        }
    }

    #[test]
    fn unknown_members_of_namespaces_and_hidden_members_are_reported() {
        assert_eq!(reported("let x = UsersApi.Nope;\n"), ["MR0127"]);
        let hidden = "public class A {\n    private class Inner {}\n}\nlet x = A.Inner;\n";
        assert_eq!(reported(hidden), ["MR0129"]);
        assert_eq!(
            reported("Twice(): int => 2;\nlet x: Twice = 1;\n"),
            ["MR0127"]
        );
    }

    #[test]
    fn a_type_name_reaches_only_static_value_members() {
        let text = "public class Counter {\n    public const Max: int = 3;\n    \
                    public Count: int => 1;\n    public static Total: int = 0;\n}\n\
                    let a = Counter.Max;\nlet b = Counter.Total;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            reported(&format!("{text}let c = Counter.Count;\n")),
            ["MR0127"]
        );
    }

    #[test]
    fn more_constructs_print() {
        let text = "public struct P {\n    public X: int;\n}\n\
                    F(p: P, n: int) {\n    \
                    let made = new P(1);\n    let empty = new P {};\n    \
                    let isInt = n is int;\n    \
                    let f = [move nope] () => 1;\n    \
                    match (p) {\n        p.Q(_) => {}\n    }\n}\n";
        let dumped = dump(text);
        for expected in [
            "let made#2: P = new P(1);",
            "let empty#3: P = new P {};",
            "let isInt#4: bool = n#1 is int;",
            "let f#5 = [move nope{error}] () => 1;",
            "p#0.Q(_) => {},",
        ] {
            assert!(dumped.contains(expected), "{expected}\n{dumped}");
        }
    }

    #[test]
    fn a_field_may_take_the_name_of_its_type() {
        let text = "public struct Address {\n    public Line: string;\n}\n\
                    public class User {\n    public Address: Address;\n    \
                    public Street(): string => Address.Line;\n}\n\
                    public struct Order {\n    public Address: Address;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let dumped = dump(text);
        assert!(
            dumped.contains("Address{field UsersApi.User.Address}.Line"),
            "{dumped}"
        );
    }

    #[test]
    fn member_signatures_mention_no_less_visible_type() {
        let text = "struct Secret {}\ninterface IShape {\n    Area(x: Secret): int;\n}\n\
                    public class Vault {\n    public Vault(s: Secret) {}\n    \
                    public Current: Secret;\n    \
                    public operator +(a: Vault, b: Secret): Vault => a;\n    \
                    public this[k: Secret]: int => 1;\n    \
                    public Kept: Secret => default;\n    \
                    IShape.Area(x: Secret): int => 1;\n    \
                    Folder(s: Secret) {}\n}\n\
                    public extension (s: Secret) {\n    public F(x: Secret): int => 1;\n    \
                    public Zero: Secret => default;\n}\n\
                    public union U { C(x: Secret) }\n\
                    public class Grid {\n    public this[k: Secret]: int { get => 1; }\n    \
                    public this[k: Secret, j: int]: int { }\n}\n";
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0123"; 11]);
        let names: Vec<_> = reported
            .iter()
            .map(|d| d.message.split('`').nth(1).unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "Vault",
                "Current",
                "+",
                "this[]",
                "Kept",
                "extension",
                "F",
                "Zero",
                "C",
                "this[]",
                "this[]"
            ]
        );
    }

    #[test]
    fn a_type_name_reaches_only_static_functions() {
        let text = "public class Counter {\n    public Bump() {}\n    \
                    public static Make(): int => 1;\n}\nlet n = Counter.Make();\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(reported(&format!("{text}Counter.Bump();\n")), ["MR0127"]);
    }
}
