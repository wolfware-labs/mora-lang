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

use crate::items::{ItemIdx, parse};
use crate::type_ref::TypeRef;

pub use dump::hir_dump;
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
    /// a field's type and initializer, a getter, or an enum member's value.
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
    #[default]
    TopLevel,
    Function,
    /// The initializer of a field, a constant or an enum member.
    Initializer,
    Getter,
    Constructor,
    Destructor,
    Operator,
    /// An indexer's `get` accessor, or an indexer with an expression body.
    Get,
    /// An indexer's `set` accessor.
    Set,
    /// An extension block itself.
    Extension,
}

pub type ExprId = Idx<Expr>;
pub type StmtId = Idx<Stmt>;
pub type PatId = Idx<Pat>;
pub type LocalId = Idx<Local>;
pub type ScopeId = Idx<ScopeData>;
pub type TypeId = Idx<TypeRef>;

/// The statements and expressions of one owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    /// What kind of declaration it comes from.
    pub kind: BodyKind,
    /// The name of a declaration that is not in the item tree, as written:
    /// `IShape.Area`, an operator's symbol, an extension member's name.
    /// Empty for the others.
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
    /// A constructor's `: this(...)`, as a call of `this`.
    pub this_call: Option<ExprId>,
    /// The body itself: a block, or the expression of `=> e`. `None` for a
    /// function declared without a body.
    pub root: Option<ExprId>,
    /// The locals that take the name of a local in scope (`MR0180`), each
    /// with the one it would shadow.
    pub shadows: Vec<(LocalId, LocalId)>,
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
    Call {
        callee: ExprId,
        args: Vec<Arg>,
    },
    /// `a[i]` or `a?[i]`.
    Index {
        base: ExprId,
        args: Vec<Arg>,
        optional: bool,
    },
    Unary {
        op: UnaryOp,
        expr: ExprId,
    },
    /// `x++` and `x--`.
    Postfix {
        increment: bool,
        expr: ExprId,
    },
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
    Cast {
        ty: TypeId,
        expr: ExprId,
    },
    Await(ExprId),
    /// `a..b` with either end left out.
    Range {
        start: Option<ExprId>,
        end: Option<ExprId>,
    },
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
    Is {
        expr: ExprId,
        pat: PatId,
    },
    /// `e as T`.
    As {
        expr: ExprId,
        ty: TypeId,
    },
    /// `c ? a : b`.
    Conditional {
        condition: ExprId,
        then: ExprId,
        r#else: ExprId,
    },
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
    Nameof(ExprId),
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
    Int(String),
    Real(String),
    Char(String),
    String(String),
    Bool(bool),
    None,
}

/// A part of an interpolated string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpPart {
    /// Text as written.
    Text(String),
    /// `{value,alignment:format}`.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgMode {
    Ref,
    In,
    Move,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    This,
    Local(LocalId),
    /// A name that is no local in scope (`MR0127`).
    Unresolved(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Plus,
    Neg,
    Not,
    BitNot,
    PreIncrement,
    PreDecrement,
    /// `^i`, an index from the end.
    FromEnd,
}

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
    Deconstruct {
        pat: PatId,
        init: ExprId,
    },
    /// A local function.
    Function(LocalFunction),
    Block(Vec<StmtId>),
    If {
        condition: ExprId,
        then: StmtId,
        r#else: Option<StmtId>,
    },
    While {
        condition: ExprId,
        body: StmtId,
    },
    DoWhile {
        body: StmtId,
        condition: ExprId,
    },
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
    Break,
    Continue,
    Return(Option<ExprId>),
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
    /// The default value of each parameter that has one.
    pub defaults: Vec<(LocalId, ExprId)>,
    /// The return type as written.
    pub ret: Option<TypeId>,
    /// Whether it is `async`.
    pub is_async: bool,
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
    Type {
        ty: TypeId,
        binding: Option<PatId>,
    },
    /// `let x`, `var x`, or the name of a designation.
    Bind(LocalId),
    /// `_`.
    Discard,
    /// `< 0`.
    Relational {
        op: BinaryOp,
        value: ExprId,
    },
    Not(PatId),
    And(PatId, PatId),
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
    /// The name of each unresolved capture, by its lambda and position.
    pub captures: HashMap<(ExprId, usize), TextRange>,
    /// The segments of each path inside the body's types, indexed by
    /// [`crate::type_ref::PathRef::index`].
    pub type_paths: Vec<Vec<TextRange>>,
}

impl BodySourceMap {
    pub fn expr(&self, expr: ExprId) -> TextRange {
        self.exprs[u32::from(expr.into_raw()) as usize]
    }

    pub fn local(&self, local: LocalId) -> TextRange {
        self.locals[u32::from(local.into_raw()) as usize]
    }
}

/// Every body of a file and its source map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileBodies {
    /// Every owner: the top-level statements first, then the others in the
    /// order of the text.
    owners: Vec<BodyOwner>,
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
    let has_statements = crate::item_tree(db, file).has_statements();
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

    use crate::test_db::{TestDb, app, codes, file_named, messages};
    use crate::*;

    /// The HIR of `src/Program.mr` alone in an app.
    fn dump(text: &str) -> String {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        hir_dump(&db, project, file_named(&db, project, "src/Program.mr"))
    }

    /// The codes `src/Program.mr` alone in an app reports.
    fn reported(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        codes(diagnostics(&db, project))
    }

    #[test]
    fn locals_parameters_members_and_items_resolve() {
        let text = "public struct Point {\n    public X: int;\n    public Length(): int {\n        \
                    let d = X * X;\n        return d + Twice(X);\n    }\n}\n\
                    Twice(n: int): int => n * 2;\n\
                    let p = new Point { X = 3 };\nConsole.WriteLine(p.Length());\n";
        assert_eq!(
            dump(text),
            "top-level {\n    let p#0 = new Point { X = 3 };\n    \
             Console{external}.WriteLine{external}(p#0.Length());\n}\n\n\
             fn UsersApi.Point.Length() {\n    let d#0 = X{field UsersApi.Point.X} * X{field UsersApi.Point.X};\n    \
             return d#0 + Twice{fn UsersApi.Twice}(X{field UsersApi.Point.X});\n}\n\n\
             fn UsersApi.Twice(n#0) => n#0 * 2;\n"
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
            "top-level {\n    var total#0 = 0;\n    \
             for (var i#1 = 0; i#1 < 10; i#1++) {\n        \
             if (((i#1 % 2) == 0) && (i#1 > 2)) {\n            total#0 += i#1;\n        \
             } else {\n            continue;\n        }\n    }\n    \
             while (total#0 > 5) total#0 = (total#0 - 1);\n    \
             let s#2 = $\"{total#0,4:x} items\";\n    \
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
        let text = "class A {\n    F() {}\n    IShape.Area(): float => 1.0;\n    struct B { G() {} }\n}\n\
                    extension (a: A) {}\nH() {}\nconst K: int = 1;\n";
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
        assert_eq!(owners, ["F", "A#0", "G", "extension0.0", "H", "K"]);
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
}
