//! The types of a body's expressions and locals (§5.10, §7, §14.5).
//!
//! [`infer`] checks one body bidirectionally: a declared type, a parameter
//! type, a return type or a condition is the expected type of the
//! expression it applies to, and flows into literals, tuples and `c ? a : b`;
//! every other expression gets its type from its parts. A value converts to
//! the type expected of it only as §5.10 allows; until the standard library
//! brings `IFrom`, the numeric rows of §5.10 are a table in the checker
//! (D-135).
//!
//! What a later milestone gives meaning to (calls, lambdas, `new`, `match`,
//! patterns, collections, options, names of the standard library, and the
//! types of [`Ty::Unsupported`]) has no type yet: [`Inference::expr`] gives
//! `None` for it. An expression of unknown type, or of [`Ty::Error`], is
//! never the cause of a diagnostic, so nothing is reported twice and
//! nothing is reported about code this milestone does not check.

mod check;
mod constant;
mod diagnostic;
mod numeric;

use std::collections::HashMap;

use mora_span::{Db, Project, SourceFile};

use crate::arena::arena_index;
use crate::bodies::{ArgMode, BinaryOp, BodyOwner, ExprId, LocalId, TypeId, UnaryOp};
use crate::defs::ItemId;
use crate::items::Visibility;
use crate::resolve::{Resolved, Scope, item_data};
use crate::ty::{ParamMode, Primitive, Ty};

pub use constant::literal_constant;
pub(crate) use diagnostic::{Source, type_error};

/// The types of a body's expressions and locals, and what is wrong with
/// them. It holds no text range, so an edit in another body leaves it
/// equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inference {
    /// The type of each expression, indexed like [`crate::Body::exprs`].
    exprs: Vec<Option<Ty>>,
    /// The type of each local, indexed like [`crate::Body::locals`].
    locals: Vec<Option<Ty>>,
    /// The function each call that is checked calls.
    calls: HashMap<ExprId, Callee>,
    /// The parameter each argument of a checked call goes to.
    arguments: HashMap<ExprId, Vec<usize>>,
    /// The value of each operation made only of literals that took a target
    /// type and fits it.
    constants: HashMap<ExprId, Constant>,
    /// The value of each number literal and negated number literal.
    literals: HashMap<ExprId, Constant>,
    /// The conversion applied to each expression that changes type.
    conversions: HashMap<ExprId, Conversion>,
    /// The conversion applied to an element of a value that an assignment
    /// deconstructs, by the value and the element's position.
    element_conversions: HashMap<(ExprId, u32), Conversion>,
    /// The type errors, in the order they were found.
    pub(crate) errors: Vec<TypeError>,
}

/// The value of an operation made only of literals that takes its target
/// type (§3.5): the exact result, converted to that type. It is the
/// operation's value, to be used as is: evaluating the operators in the
/// target type could give another result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Constant {
    /// A value of an integer type.
    Int(i128),
    /// A value of `float` or `double`, as the bits of the `double` that
    /// holds it, exactly for a `float` too.
    Real(u64),
}

impl std::fmt::Display for Constant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Constant::Int(value) => write!(f, "{value}"),
            Constant::Real(bits) => write!(f, "{}", f64::from_bits(*bits)),
        }
    }
}

/// The text of a real constant of type `ty`, as it is written in code: the
/// shortest digits that give the value back in `ty`, so a `float` that holds
/// the nearest value to `0.1` prints `0.1`, and always with a decimal point
/// when the value is whole, as `5.0`. Infinity and not-a-number print as
/// `inf`, `-inf` and `NaN`.
pub fn real_text(bits: u64, ty: Primitive) -> String {
    let value = f64::from_bits(bits);
    let mut text = if ty == Primitive::Float {
        // A `float` constant is stored as the `double` that holds it exactly.
        (value as f32).to_string()
    } else {
        value.to_string()
    };
    if text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'-')
    {
        text.push_str(".0");
    }
    text
}

/// How a value changes type where inference converts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionKind {
    /// A widening that needs no cast (§5.10, §7.12).
    Implicit,
    /// A numeric conversion that a cast writes, or that a compound
    /// assignment applies to its result.
    Numeric,
}

/// A conversion of an expression's value to another type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversion {
    /// The type the value converts to.
    pub to: Ty,
    /// How the conversion comes about.
    pub kind: ConversionKind,
}

impl Inference {
    /// The conversion applied to the value of `expr`, when it changes type:
    /// to the type its context expects, to the promoted type of its
    /// operation, or by a cast. A compound assignment carries the cast
    /// back of its result to the type of its place.
    ///
    /// An expression has at most one record. A written cast `(T)x` always
    /// records `x`, even when `x` has type `T`. In `x op= y`, the record on
    /// `x` applies to reading `x`, never to the place that is stored. A
    /// tuple is never recorded as a whole: the elements of a value that an
    /// assignment deconstructs are in [`Inference::element_conversion`].
    pub fn conversion(&self, expr: ExprId) -> Option<&Conversion> {
        self.conversions.get(&expr)
    }

    /// The conversion of an element of `value`, the tuple that the
    /// assignment `(a, (b, c)) = value` deconstructs, when the element
    /// changes type on its way to its place. `index` counts the places that
    /// receive an element, depth first: `a` is 0, `b` is 1 and `c` is 2. A
    /// discard `_` counts as a place too. When the value is a tuple literal
    /// with a tuple in it, as in `(a, (b, c)) = (x, t)`, the nested part is
    /// deconstructed on its own: its conversions are keyed by `t`, and its
    /// indices restart at 0.
    pub fn element_conversion(&self, value: ExprId, index: u32) -> Option<&Conversion> {
        self.element_conversions.get(&(value, index))
    }

    /// What the call `call` calls, when the call is checked: the overload
    /// that overload resolution chose, or the one function of its name.
    pub fn callee(&self, call: ExprId) -> Option<Callee> {
        self.calls.get(&call).copied()
    }

    /// The parameter of [`Inference::callee`] that each argument of the
    /// call `call` goes to, by position, in the order of the arguments: a
    /// named argument goes to the parameter of its name (§6.9). A parameter
    /// no argument goes to takes its default value. `None` when the call is
    /// not checked.
    pub fn arguments(&self, call: ExprId) -> Option<&[usize]> {
        self.arguments.get(&call).map(Vec::as_slice)
    }

    /// The value of `expr` when it is an operation made only of literals
    /// that took its target type (§3.5): what the operation evaluates to.
    pub fn constant(&self, expr: ExprId) -> Option<Constant> {
        self.constants.get(&expr).copied()
    }

    /// The value of `expr` when it is a number literal, or `-` before one
    /// (§3.5), as the type inference gave it holds it. A literal whose
    /// value its type does not hold, which is reported, has none. The
    /// literals of an operation that has a [`Inference::constant`] have no
    /// value of their own.
    pub fn literal(&self, expr: ExprId) -> Option<Constant> {
        self.literals.get(&expr).copied()
    }

    /// The type of `expr`: `None` while it is unknown, because a later
    /// milestone gives it meaning, and [`Ty::Error`] when a diagnostic says
    /// why it has none.
    pub fn expr(&self, expr: ExprId) -> Option<&Ty> {
        self.exprs.get(arena_index(expr))?.as_ref()
    }

    /// The type of `local`: the declared one, or that of its initializer.
    /// `None` while it is unknown, as for [`Inference::expr`].
    pub fn local(&self, local: LocalId) -> Option<&Ty> {
        self.locals.get(arena_index(local))?.as_ref()
    }
}

/// What a name that the creation of a struct assigns names instead of an
/// instance field (`MR0147`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotAField {
    /// No member of the struct.
    Unknown,
    /// A getter.
    Getter,
    /// A `static` field.
    Static,
    /// A constant.
    Constant,
}

/// What a static member, a field initializer or a member of a nested type
/// uses that needs a value it does not have (`MR0149`, `MR0152`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstanceUse {
    /// `this`.
    This,
    /// An instance member of the type, or of a type around it, found by
    /// its bare name.
    Member { kind: InstanceMember, name: String },
}

/// What kind of instance member an [`InstanceUse::Member`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceMember {
    /// A field stored in each value.
    Field,
    /// A getter.
    Getter,
    /// A method.
    Method,
}

impl InstanceMember {
    /// What a message calls it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            InstanceMember::Field => "field",
            InstanceMember::Getter => "getter",
            InstanceMember::Method => "method",
        }
    }
}

/// What the left side of an assignment, or the operand of a step, is
/// instead of a place (`MR0186`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotAPlace {
    /// A method or a function.
    Function,
    /// A getter, which has no setter.
    Getter,
    /// A constant.
    Constant,
    /// An enum member or a union case.
    Case,
    /// The result of a call.
    Call,
    /// A field of a struct value that is not a place itself.
    FieldOfValue,
    /// `_`, which only the left side of a plain `=` discards to.
    Discard,
    /// Any other value, as a literal or an operation.
    Value,
}

impl NotAPlace {
    /// What the message calls it.
    pub(crate) fn what(self) -> &'static str {
        match self {
            NotAPlace::Function => "a method or a function",
            NotAPlace::Getter => "a getter, which has no setter",
            NotAPlace::Constant => "a constant",
            NotAPlace::Case => "an enum member or a union case",
            NotAPlace::Call => "the result of a call",
            NotAPlace::FieldOfValue => "a field of a value that is not a place",
            NotAPlace::Discard => "`_`, which only `=` assigns to",
            NotAPlace::Value => "a value",
        }
    }
}

/// Why a `new` has nothing to create (`MR0154`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewTarget {
    /// Its place gives it no target type.
    Missing,
    /// It is an argument of a call of `name`, whose overloads that can
    /// take the arguments have parameters of different types there.
    Overloads { name: String },
    /// Its target type is this one, which is no struct and no class.
    NotCreatable(Ty),
    /// It is not without a type after all: the type written is this one,
    /// which is no struct and no class.
    Written(Ty),
}

/// A type error in a body, by the expression it is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeError {
    /// `expr` is of type `found` where `expected` is expected, and `found`
    /// does not convert to it (`MR0203`).
    Mismatch {
        expr: ExprId,
        expected: Ty,
        found: Ty,
        context: Context,
    },
    /// The literal `expr`, a negated literal or an expression made only of
    /// literals, does not fit `ty`, or no integer type when `ty` is `None`
    /// (`MR0010`).
    Literal { expr: ExprId, ty: Option<Primitive> },
    /// The binary operator of `expr`, an operation or a compound
    /// assignment, is not defined for its operands `lhs` and `rhs`, of
    /// types `lhs_ty` and `rhs_ty` (`MR0204`).
    Binary {
        expr: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty,
        rhs_ty: Ty,
    },
    /// The operand `operand` of `op`, `&&` or `||`, is of type `ty`, not
    /// `bool` (`MR0204`). No type can declare these operators (§6.9), so
    /// each operand is checked on its own.
    Logical {
        op: BinaryOp,
        operand: ExprId,
        ty: Ty,
    },
    /// The prefix or postfix operator of `expr` is not defined for its
    /// operand, of type `ty` (`MR0204`); `++` and `--` are the prefix forms.
    Unary {
        expr: ExprId,
        op: UnaryOp,
        operand: ExprId,
        ty: Ty,
    },
    /// The cast `expr` of `operand` converts between types that are not
    /// both numeric (`MR0204`).
    Cast {
        expr: ExprId,
        operand: ExprId,
        from: Ty,
        to: Ty,
    },
    /// A call of one declaration, `signature`, with an argument too many,
    /// `expr`, or without an argument for the parameter `missing`, the call
    /// `expr` (`MR0205`).
    Arity {
        expr: ExprId,
        signature: String,
        missing: Option<String>,
    },
    /// The argument `arg` of a call of one declaration, `signature`, is
    /// written with the marker `given` where its parameter `param` has the
    /// mode `needs` (`MR0205`); `fixable` when writing `ref` before it
    /// would fix the call, because it is a place of a fitting type.
    Mode {
        arg: ExprId,
        signature: String,
        param: String,
        needs: ParamMode,
        given: Option<ArgMode>,
        fixable: bool,
    },
    /// No overload named `name` is applicable to the call `expr`; each
    /// candidate with its signature (`MR0205`).
    NoOverload {
        expr: ExprId,
        name: String,
        candidates: Vec<(ItemId, String)>,
    },
    /// Of the overloads named `name` applicable to the call `expr`, none is
    /// better than the others; each of them with its signature (`MR0206`).
    Ambiguous {
        expr: ExprId,
        name: String,
        candidates: Vec<(ItemId, String)>,
    },
    /// The named argument `arg`, or the positional one after a named one,
    /// is wrong (`MR0207`).
    NamedArgument { arg: ExprId, problem: NamedProblem },
    /// The callee `expr` has the type `ty`, which is not a function type
    /// (`MR0208`).
    NotCallable { expr: ExprId, ty: Ty },
    /// The memberwise creation `expr` of a struct, `ty`, does not assign
    /// the field `field`, which has no initializer (`MR0143`).
    MissingField { expr: ExprId, ty: Ty, field: String },
    /// The memberwise creation `expr` of a struct, `ty`, is not available
    /// here: its field `field`, which has no initializer, has the
    /// visibility `visibility` and is not visible from the body (`MR0144`).
    NoMemberwise {
        expr: ExprId,
        ty: Ty,
        field: String,
        visibility: Visibility,
    },
    /// `expr`, in `from`, creates the struct `item`, written `written` or,
    /// when `None`, the target type, with braces alone, while it is
    /// positional or declares a constructor, which turns memberwise creation
    /// off (`MR0153`). `writable` is set when every name in the braces is a
    /// field that the body may write after a constructor, so calling one
    /// keeps them valid.
    MemberwiseOff {
        expr: ExprId,
        item: ItemId,
        written: Option<TypeId>,
        from: Scope,
        writable: bool,
    },
    /// The creation of the struct `item` assigns `value` to its field
    /// `name`, the value member at `index`, which is not visible from
    /// `from` (`MR0129`). `fixable` is set when widening the field makes
    /// the assignment valid and no earlier assignment of the creation names
    /// the field, which would carry the fix.
    HiddenField {
        value: ExprId,
        item: ItemId,
        index: u32,
        name: String,
        from: Scope,
        fixable: bool,
    },
    /// The creation of the struct `ty`, memberwise or, when `memberwise`
    /// is not set, after a constructor call, assigns `value` to `name`,
    /// which is not an instance field of it but `member`; for a name that
    /// is no member, `suggestion` is the closest field, one not assigned
    /// yet in memberwise creation (`MR0147`).
    NotAField {
        value: ExprId,
        ty: Ty,
        name: String,
        member: NotAField,
        suggestion: Option<String>,
        memberwise: bool,
    },
    /// `expr` passes arguments to a constructor of the struct `ty`, which
    /// declares none and is not positional (`MR0205`).
    NoConstructor { expr: ExprId, ty: Ty },
    /// `expr`, a `new`, has no type that `new` can create (`MR0154`).
    NewTarget { expr: ExprId, problem: NewTarget },
    /// The memberwise creation of the struct `ty` assigns `value` to the
    /// field `name` again: `first` is the value of its first assignment,
    /// and `previous` the value written just before `value` (`MR0148`).
    RepeatedField {
        value: ExprId,
        first: ExprId,
        previous: ExprId,
        ty: Ty,
        name: String,
    },
    /// The body of a static member uses `this` or an instance member at
    /// `expr`, which names it (`MR0149`).
    StaticThis { expr: ExprId, used: InstanceUse },
    /// A member of the type `nested` uses, at `expr`, the instance `kind`
    /// `name` of `outer`, a type it is nested in, which needs a value of
    /// `outer` that it does not have (`MR0149`). Never `this`, which is a
    /// value of `nested`.
    NestedThis {
        expr: ExprId,
        kind: InstanceMember,
        name: String,
        outer: String,
        nested: String,
    },
    /// The initializer of the instance field `field` uses `this` or an
    /// instance member at `expr`, which names it (`MR0152`).
    InitializerThis {
        expr: ExprId,
        used: InstanceUse,
        field: String,
    },
    /// The assignment `expr` is used as a value (`MR0185`); `split` is the
    /// statement of a block it is the value of, when a fix can write it as a
    /// statement before that one.
    AssignmentValue { expr: ExprId, split: Option<ExprId> },
    /// `expr` names a type or a namespace, `named`, where a value is
    /// expected (`MR0127`).
    TypeAsValue { expr: ExprId, named: Resolved },
    /// `expr`, a type keyword, stands where a value is expected (`MR0127`).
    TypeKeywordAsValue { expr: ExprId },
    /// `expr`, the left side of an assignment or the operand of a step, is
    /// not a place (`MR0186`).
    NotAPlace { expr: ExprId, problem: NotAPlace },
    /// The callee `expr` names methods of a value, none of which is visible
    /// from `from`; `item` is the first (`MR0129`).
    NotVisible {
        expr: ExprId,
        item: ItemId,
        from: Scope,
    },
}

/// What is wrong with an argument's name (`MR0207`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamedProblem {
    /// A positional argument after a named one.
    AfterNamed,
    /// A name given twice.
    Duplicate { name: String },
    /// A name the function `function` has no parameter for.
    Unknown { name: String, function: String },
    /// A name for a parameter given by position too.
    AlreadyGiven { name: String },
}

/// What a checked call calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A function or a method, by its declaration.
    Item(ItemId),
    /// A local function of the body.
    Local(LocalId),
}

impl Callee {
    /// The declaration of a [`Callee::Item`].
    pub fn item(self) -> Option<ItemId> {
        match self {
            Callee::Item(item) => Some(item),
            Callee::Local(_) => None,
        }
    }
}

/// What expects the type of a [`TypeError::Mismatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// A declared, parameter, return or field type.
    Value,
    /// A condition, which expects `bool`.
    Condition,
    /// A `ref` argument, which has exactly its parameter's type (§10.3).
    Ref,
    /// The other branch of `c ? a : b`, which gives the expected type.
    Branch(ExprId),
}

impl Context {
    /// Whether a value of type `found` stands where it expects `expected`:
    /// a `ref` argument has exactly its parameter's type (§10.3), and any
    /// other value converts to the expected type (§5.10).
    fn accepts(self, found: &Ty, expected: &Ty) -> bool {
        match self {
            Context::Ref => same(found, expected),
            Context::Value | Context::Condition | Context::Branch(_) => converts(found, expected),
        }
    }
}

/// Checks the body of `owner` and gives the types of its expressions and
/// locals.
#[salsa::tracked(returns(ref))]
pub fn infer(db: &dyn Db, project: Project, file: SourceFile, owner: BodyOwner) -> Inference {
    check::infer_body(db, project, file, owner)
}

/// The type errors of the body of `owner`, found by checking it again
/// rather than through [`infer`]. The diagnostics of a file run once per
/// edit of that file, and each body's memo of [`infer`] would otherwise be
/// one more memo that every later revision has to verify.
pub(crate) fn type_errors(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    owner: BodyOwner,
) -> Vec<TypeError> {
    check::infer_body(db, project, file, owner).errors
}

/// Whether everything about `ty` is known, so that a diagnostic can be
/// about it, or a later pass hold a value of it: an unsupported type or an
/// error inside it makes it unknown.
pub fn is_known(ty: &Ty) -> bool {
    match ty {
        Ty::Primitive(_) | Ty::String | Ty::Struct(_) | Ty::Never => true,
        Ty::Tuple(elems) => elems.iter().all(|elem| is_known(&elem.ty)),
        Ty::Unsupported(_) | Ty::Error => false,
    }
}

/// Whether two types are the same, the names of tuple elements aside.
fn same(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(&a.ty, &b.ty))
        }
        _ => a == b,
    }
}

/// Whether a value of `from` converts implicitly to `to` (§5.10): the same
/// type, one of the numeric rows, or the never type, which converts to
/// every type (§11.4).
pub(crate) fn converts(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Never, _) => true,
        (Ty::Primitive(from), Ty::Primitive(to)) => numeric::converts(*from, *to),
        _ => same(from, to),
    }
}

/// Whether a value of the primitive type `from` converts implicitly to `to`
/// (§5.10): the same type, an integer type to a wider one of the same
/// signedness or a wider signed one, an integer type to `float` or `double`
/// where every value is exact, and `float` to `double`. Nothing converts to
/// or from `nint` or `nuint` but themselves.
pub fn converts_implicitly(from: Primitive, to: Primitive) -> bool {
    numeric::converts(from, to)
}

/// `ty` as code writes it, as messages and the HIR dump show it.
pub fn ty_name(db: &dyn Db, ty: &Ty) -> String {
    match ty {
        Ty::Primitive(primitive) => primitive.keyword().to_owned(),
        Ty::String => "string".to_owned(),
        Ty::Tuple(elems) => {
            let mut parts: Vec<String> = elems
                .iter()
                .map(|elem| match &elem.name {
                    Some(name) => format!("{name}: {}", ty_name(db, &elem.ty)),
                    None => ty_name(db, &elem.ty),
                })
                .collect();
            if let [only] = parts.as_mut_slice() {
                only.push(',');
            }
            format!("({})", parts.join(", "))
        }
        Ty::Struct(item) => item_data(db, *item).name.clone(),
        Ty::Unsupported(_) | Ty::Error => "_".to_owned(),
        Ty::Never => "!".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use salsa::Setter;

    use crate::test_db::{TestDb, app, codes, diagnostics_of, file_named, messages};
    use crate::*;

    /// The codes `src/Program.mr` alone in an app reports.
    fn reported(text: &str) -> Vec<String> {
        codes(&diagnostics_of(text))
    }

    /// The type of each local of the top-level statements of `text`, `?`
    /// for one that is unknown.
    fn local_types(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.locals
            .iter()
            .map(|(local, data)| {
                let ty = inference
                    .local(local)
                    .map_or_else(|| "?".to_owned(), |ty| super::ty_name(&db, ty));
                format!("{}: {ty}", data.name)
            })
            .collect()
    }

    /// The value inference records for each number literal and negated
    /// number literal in the top-level statements of `text`, in expression
    /// order.
    fn literal_values(text: &str) -> Vec<Constant> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.exprs
            .iter()
            .filter_map(|(id, _)| inference.literal(id))
            .collect()
    }

    #[test]
    fn records_the_value_of_each_literal_in_its_type() {
        assert_eq!(
            literal_values(
                "let b: sbyte = -128;\nlet x: long = -1;\nlet d: double = 2;\nlet f: float = 0.1;\n\
                 let r = -1.5;\nlet k = -1u;\nlet c = 'a';\n"
            ),
            [
                Constant::Int(-128),
                Constant::Int(-1),
                Constant::Real(2.0f64.to_bits()),
                Constant::Real(f64::from(0.1f32).to_bits()),
                Constant::Real((-1.5f64).to_bits()),
                Constant::Int(1),
            ]
        );
        // A literal that does not fit its type has no value, and the parts
        // of an operation on literals are not values of their own.
        assert_eq!(literal_values("let b: byte = 256;\n"), []);
        assert_eq!(
            literal_values("let b: byte = 200 + 55;\nlet n = 2L;\n"),
            [Constant::Int(2)]
        );
    }

    /// The conversions inference records in the top-level statements of
    /// `text`, in expression order, as `<expression>: <kind> <type>`; a
    /// local shows as its name, an assignment as `=`, any other expression
    /// as `_`.
    fn conversions(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.exprs
            .iter()
            .filter_map(|(id, expr)| {
                let conversion = inference.conversion(id)?;
                let label = match expr {
                    Expr::Local(local) => body.locals[*local].name.clone(),
                    Expr::Assign { .. } => "=".to_owned(),
                    _ => "_".to_owned(),
                };
                let kind = match conversion.kind {
                    ConversionKind::Implicit => "Implicit",
                    ConversionKind::Numeric => "Numeric",
                };
                Some(format!(
                    "{label}: {kind} {}",
                    super::ty_name(&db, &conversion.to)
                ))
            })
            .collect()
    }

    /// The conversions of the elements of the value `pair` that the
    /// assignment in `text` deconstructs, by leaf position, and the
    /// conversion of the value itself.
    fn element_conversions(text: &str, leaves: u32) -> (Vec<Option<String>>, Option<Conversion>) {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        assert_eq!(diagnostics(&db, project), &[]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let value = body
            .exprs
            .iter()
            .find_map(|(_, expr)| match expr {
                Expr::Assign { op: None, rhs, .. } => Some(*rhs),
                _ => None,
            })
            .unwrap();
        let elements = (0..leaves)
            .map(|index| {
                inference
                    .element_conversion(value, index)
                    .map(|conversion| {
                        assert_eq!(conversion.kind, ConversionKind::Implicit);
                        super::ty_name(&db, &conversion.to)
                    })
            })
            .collect();
        (elements, inference.conversion(value).cloned())
    }

    #[test]
    fn records_deconstructed_elements_one_by_one() {
        let text = "let a: long = 0;\nlet b: int = 0;\nlet pair: (int, int) = (1, 2);\n\
                    (a, b) = pair;\n";
        let (elements, whole) = element_conversions(text, 2);
        assert_eq!(elements, [Some("long".to_owned()), None]);
        assert_eq!(whole, None);
    }

    #[test]
    fn records_nested_deconstructed_elements_by_leaf() {
        let text = "let a: int = 0;\nlet b: long = 0;\nlet c: double = 0.0;\n\
                    let t: (int, (int, int)) = (1, (2, 3));\n(a, (b, c)) = t;\n";
        let (elements, whole) = element_conversions(text, 3);
        assert_eq!(
            elements,
            [None, Some("long".to_owned()), Some("double".to_owned())]
        );
        assert_eq!(whole, None);
    }

    #[test]
    fn records_coerce_widening() {
        assert_eq!(
            conversions("let n: int = 1;\nlet x: long = n;\n"),
            ["n: Implicit long"]
        );
    }

    #[test]
    fn records_struct_initializer_widening() {
        let text = "struct S { public L: long; }\nlet n: int = 1;\nlet s = new S { L = n };\n";
        assert_eq!(conversions(text), ["n: Implicit long"]);
    }

    #[test]
    fn records_binary_promotion() {
        let text = "let b1: byte = 1;\nlet b2: byte = 2;\nlet r = b1 + b2;\n";
        assert_eq!(conversions(text), ["b1: Implicit int", "b2: Implicit int"]);
    }

    #[test]
    fn records_only_the_operand_that_widens() {
        let text = "let i: int = 1;\nlet l: long = 2;\nlet r = i < l;\n";
        assert_eq!(conversions(text), ["i: Implicit long"]);
    }

    #[test]
    fn records_unary_widening() {
        assert_eq!(
            conversions("let u: uint = 1u;\nlet r = -u;\n"),
            ["u: Implicit long"]
        );
        assert_eq!(
            conversions("let b: byte = 1;\nlet r = ~b;\n"),
            ["b: Implicit int"]
        );
    }

    #[test]
    fn a_step_records_nothing() {
        assert_eq!(
            conversions("let b: byte = 1;\nb++;\n++b;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn records_shift_left_operand() {
        let text = "let small: byte = 1;\nlet r = small << 2;\n";
        assert_eq!(conversions(text), ["small: Implicit int"]);
        let text = "let n: int = 1;\nlet c: byte = 2;\nlet r = n << c;\n";
        assert_eq!(conversions(text), ["c: Implicit int"]);
    }

    #[test]
    fn records_conditional_branch() {
        let text = "let c = true;\nlet i: int = 1;\nlet l: long = 2;\nlet r = c ? i : l;\n";
        assert_eq!(conversions(text), ["i: Implicit long"]);
        let text = "let c = true;\nlet i: int = 1;\nlet l: long = 2;\nlet r = c ? l : i;\n";
        assert_eq!(conversions(text), ["i: Implicit long"]);
    }

    #[test]
    fn records_targeted_conditional_branches_once() {
        let text = "let c = true;\nlet i: int = 1;\nlet r: long = c ? i : 2;\n";
        assert_eq!(conversions(text), ["i: Implicit long"]);
    }

    #[test]
    fn records_explicit_cast() {
        assert_eq!(
            conversions("let n: int = 1;\nlet r = (byte)n;\n"),
            ["n: Numeric byte"]
        );
        assert_eq!(
            conversions("let n: int = 1;\nlet r = (int)n;\n"),
            ["n: Numeric int"]
        );
    }

    #[test]
    fn records_compound_cast_back() {
        let text = "let b: byte = 1;\nb += 1;\n";
        assert_eq!(
            conversions(text),
            ["b: Implicit int", "_: Implicit int", "=: Numeric byte"]
        );
    }

    #[test]
    fn a_compound_assignment_of_one_type_records_no_cast_back() {
        assert_eq!(
            conversions("let n: int = 1;\nn += 2;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn same_type_records_nothing() {
        assert_eq!(
            conversions("let n: int = 1;\nlet x: int = n;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn literals_take_their_target_type() {
        let text = "let a: byte = 255;\nlet b: float = 1.5;\nlet c = 3_000_000_000;\n\
                    let d = 18446744073709551615;\nlet e: sbyte = -128;\nlet f = 1.5f;\n\
                    let g = 2u;\nlet h = -2147483648;\nlet i = 2.5;\nlet j: double = 1;\n\
                    let k: (byte, float) = (1, 2.0);\nlet l = 'x';\nlet m = \"s\";\n";
        assert_eq!(
            local_types(text),
            [
                "a: byte",
                "b: float",
                "c: long",
                "d: ulong",
                "e: sbyte",
                "f: float",
                "g: uint",
                "h: int",
                "i: double",
                "j: double",
                "k: (byte, float)",
                "l: char",
                "m: string",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_literal_that_does_not_fit_its_type_is_reported_once() {
        let text = "let a: byte = 256;\nlet b: sbyte = -129;\nlet c = 4294967296u;\n\
                    let d: float = 1e39;\nlet e = -18446744073709551615;\n\
                    let f: float = 16777217;\nlet g = 9223372036854775808L;\n";
        assert_eq!(reported(text), ["MR0010"; 7]);
        // The lexer reports a literal larger than any `ulong`, and the
        // checker says nothing more about it.
        assert_eq!(
            reported("let a = 18446744073709551616;\nlet b: float = 1e400;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn operands_are_promoted() {
        let text = "let b: byte = 1;\nlet u: uint = 2;\nlet x: float = 1.5;\nlet n = 3;\n\
                    let l = 4L;\nlet s: short = 5;\n\
                    let bb = b + b;\nlet iu = n + u;\nlet nf = n + x;\nlet half = x * 0.5;\n\
                    let twice = b * 2;\nlet ll = l + 1;\nlet shifted = b << 1;\nlet neg = -u;\n\
                    let inv = ~s;\nlet nd = n * 0.5;\nlet cmp = b < u;\nlet wrap = n +% 1;\n\
                    let big = b + 300;\nlet cat = \"a\" + \"b\";\nlet both = true & false;\n";
        let types = local_types(text);
        assert_eq!(
            types[6..],
            [
                "bb: int",
                "iu: long",
                "nf: double",
                "half: float",
                "twice: int",
                "ll: long",
                "shifted: int",
                "neg: long",
                "inv: int",
                "nd: double",
                "cmp: bool",
                "wrap: int",
                "big: int",
                "cat: string",
                "both: bool",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn operators_not_defined_for_their_operands_are_mr0204() {
        let text = "let l = 1L;\nlet u = 2ul;\nlet x = 1.5;\nlet n = 1;\nlet t = true;\n\
                    let a = l + u;\nlet b = x & x;\nlet c = -u;\nlet d = !n;\nlet e = \"s\" + n;\n\
                    let f = t < t;\nlet g = (string)n;\nvar h = t;\nh++;\nlet i = x << 1;\n\
                    let j = x +% x;\nlet k = ~x;\nlet m = n == t;\nlet o = (int)'c';\n";
        assert_eq!(reported(text), ["MR0204"; 13]);
    }

    #[test]
    fn values_that_do_not_convert_are_mr0203() {
        let text = "let l = 1L;\nlet x: int = l;\nlet b: bool = 1;\nlet c: char = 65;\n\
                    let s: string = 1;\nif (1) {}\nwhile (\"s\") {}\nvar small: byte = 1;\n\
                    small += 1;\nlet pick = true ? 1 : \"a\";\nlet f: float = 1.5 * 2.0;\n\
                    let t: (int, string) = (1, 2);\n";
        // `small += 1` narrows back to `byte`, and `1.5 * 2.0` is made only
        // of literals, so it takes the type `float`.
        assert_eq!(reported(text), ["MR0203"; 8]);
    }

    #[test]
    fn return_values_and_parameters_follow_the_signature() {
        let text = "Half(x: int): long => x / 2;\nName(x: int): string {\n    return x;\n}\n\
                    Wide(x: long = 1): int => 0;\nNarrow(x: byte = 300) {}\n\
                    Unit() {\n    return 1;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0203", "MR0010", "MR0203"]);
    }

    #[test]
    fn struct_fields_and_constants_have_their_types() {
        let text = "struct P {\n    X: int;\n    Y: string;\n    \
                    Sum(): int => X + Y;\n}\nconst Limit: long = 10;\n\
                    F(p: P): int => p.X;\nG(p: P): int => p.Y;\nH(): int => Limit;\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0204", "MR0203", "MR0203"]);
    }

    #[test]
    fn unknown_types_cause_no_diagnostic() {
        let text = "let a = Missing + 1;\nlet b = Console.ReadLine() + 1;\nlet c: Unknwn = 1;\n\
                    let d: int = Console.ReadLine();\nlet e = [1, 2];\nlet f = e[0] + \"s\";\n\
                    let g: int? = 1;\nlet h = (x: int) => x + \"s\";\nlet k = d + g;\n";
        // `h`'s lambda adds a string to an `int`, which is checked.
        assert_eq!(reported(text), ["MR0127", "MR0127", "MR0204"]);
    }

    #[test]
    fn mixed_operands_get_a_cast_fix() {
        let db = TestDb::new();
        let text = "let l = 1L;\nlet u = 2ul;\nlet a = l + (u + u);\nlet x: float = 1.5;\n\
                    let b = l * x;\nlet c = -u;\nvar d: byte = 1;\nd += 1;\nif (l) {}\n\
                    let s: string = l;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let fixed: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| {
                let fix = &d.fixes[0];
                let edits: Vec<_> = fix
                    .edits
                    .iter()
                    .map(|edit| (u32::from(edit.span.range.start()), edit.replacement.clone()))
                    .collect();
                (fix.message.clone(), edits)
            })
            .collect();
        let at = |pattern: &str| u32::try_from(text.find(pattern).unwrap()).unwrap();
        let end = |pattern: &str| at(pattern) + u32::try_from(pattern.len()).unwrap();
        let last = |pattern: &str| u32::try_from(text.rfind(pattern).unwrap()).unwrap();
        assert_eq!(
            fixed,
            [
                (
                    "convert it explicitly with `(long)`".to_owned(),
                    vec![
                        (at("(u + u)") + 1, "(long)(".to_owned()),
                        (end("u + u"), ")".to_owned())
                    ]
                ),
                (
                    "convert it explicitly with `(double)`".to_owned(),
                    vec![(at("l * x"), "(double)".to_owned())]
                ),
                (
                    "convert it explicitly with `(long)`".to_owned(),
                    vec![(at("u;\nvar"), "(long)".to_owned())]
                ),
                (
                    "compare it with zero".to_owned(),
                    vec![(at("l) {}") + 1, " != 0".to_owned())]
                ),
                (
                    "format it with interpolation".to_owned(),
                    vec![
                        (last("l;\n"), "$\"{".to_owned()),
                        (last("l;\n") + 1, "}\"".to_owned())
                    ]
                ),
            ]
        );
    }

    #[test]
    fn operations_on_literals_take_their_target_type() {
        let text = "let n = 1;\nlet a: float = 1.5 * 2.0;\nlet b: byte = 1 + 2;\n\
                    let c: sbyte = -(64 * 2);\nlet d: byte = ~0 & 255;\nlet e: float = 1 / 3;\n\
                    let f: ulong = 18446744073709551615 - 1;\nlet g: double = (1 + 2) * 0.5;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let wrong = "let n = 1;\nlet a: byte = 200 + 100;\nlet b: sbyte = -(64 * 2) - 1;\n\
                     let c: long = 9223372036854775807 * 4;\nlet d: float = 1e38 * 10.0;\n\
                     let e: byte = 1 + n;\nlet f: int = 1.5 * 2.0;\nlet g: byte = 1 / 0;\n";
        // Not made only of literals, a real for an integer type, and a
        // division by zero follow the usual rules.
        assert_eq!(
            reported(wrong),
            [
                "MR0010", "MR0010", "MR0010", "MR0010", "MR0203", "MR0203", "MR0203"
            ]
        );
    }

    #[test]
    fn a_compound_assignment_narrows_back_to_its_place() {
        let text = "var b: byte = 1;\nb += 1;\nb +%= 300;\nb <<= 1;\nvar i = 0;\ni += 2L;\n\
                    i *= 1.5;\nvar s = \"\";\ns += \"x\";\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let wrong = "var s = \"\";\ns += 1;\nvar l = 1L;\nl += 1ul;\nvar f = true;\nf += f;\n";
        assert_eq!(reported(wrong), ["MR0204"; 3]);
    }

    #[test]
    fn messages_name_the_types() {
        let db = TestDb::new();
        let text = "let l = 1L;\nlet u = 2ul;\nlet a = l + u;\nlet b: int = l;\n\
                    let c: byte = 300;\nlet d: byte = 200 + 100;\nlet e = (int)'c';\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        assert_eq!(
            messages(diagnostics(&db, project)),
            [
                "`+` is not defined for `long` and `ulong`",
                "mismatched types: expected `int`, found `long`",
                "the literal `300` does not fit in `byte`",
                "the value of `200 + 100` does not fit in `byte`",
                "cannot cast `char` to `int`",
            ]
        );
    }

    #[test]
    fn an_edit_inside_one_body_infers_only_that_body_again() {
        let mut db = TestDb::new();
        let text = |n: u32| format!("A(): int {{\n    return {n};\n}}\nB(): int => 2;\n");
        let project = app(&db, &[("src/Program.mr", &text(1))]);
        let file = file_named(&db, project, "src/Program.mr");
        let infer_all = |db: &TestDb| {
            for owner in body_owners(db, file) {
                infer(db, project, file, owner);
            }
        };
        infer_all(&db);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        infer_all(&db);
        let inferred = db
            .take_executed()
            .iter()
            .filter(|key| key.starts_with("infer("))
            .count();
        assert_eq!(inferred, 1);
    }

    #[test]
    fn a_deconstructing_assignment_checks_each_place() {
        let body = |stmt: &str| {
            let text = format!(
                "struct P(X: int, Y: string);\nstruct Q {{\n    X: int;\n}}\n\
                 F(p: (int, string), xs: int[], s: P, q: Q) {{\n    var a = 1;\n    \
                 var b = 2;\n    {stmt}\n}}\n"
            );
            messages(&diagnostics_of(&text))
        };
        let mismatch = |expected: &str, found: &str| {
            vec![format!(
                "mismatched types: expected `{expected}`, found `{found}`"
            )]
        };
        let none = Vec::<String>::new;
        assert_eq!(body("(a, b) = (1, 2);"), none());
        assert_eq!(
            body("(a, b) = (\"x\", \"y\");"),
            [mismatch("int", "string"), mismatch("int", "string")].concat()
        );
        assert_eq!(body("(a, _) = (1, \"x\");"), none());
        // Each element of a literal is checked, and reported, where it is.
        let text = "F() {\n    var a = 1;\n    var b = 2;\n    (a, b) = (1, \"x\");\n}\n";
        let at = diagnostics_of(text)[0].primary.span.range;
        assert_eq!(&text[std::ops::Range::<usize>::from(at)], "\"x\"");
        assert_eq!(body("(_, a) = (1, \"x\");"), mismatch("int", "string"));
        assert_eq!(
            body("(a, (b, _)) = (1, (\"z\", 3));"),
            mismatch("int", "string")
        );
        // A discard counts toward the arity, and a value of another shape
        // does not deconstruct.
        assert_eq!(body("(a, _) = 5;"), mismatch("(int, _)", "int"));
        assert_eq!(body("(a, _) = \"s\";"), mismatch("(int, _)", "string"));
        assert_eq!(
            body("(a, _) = (1, 2, 3);"),
            mismatch("(int, _)", "(int, int, int)")
        );
        assert_eq!(
            body("(a, b, _) = (1, 2);"),
            mismatch("(int, int, _)", "(int, int)")
        );
        assert_eq!(
            body("(a, _) = (\"x\", 2, 3);"),
            mismatch("(int, _)", "(string, int, int)")
        );
        assert_eq!(body("(a, (b, _)) = (1, 5);"), mismatch("(int, _)", "int"));
        assert_eq!(
            body("let t = (\"x\", 2);\n    (a, _) = t;"),
            mismatch("int", "string")
        );
        assert_eq!(
            body("let t = (1, 2, 3);\n    (a, _) = t;"),
            mismatch("(int, _)", "(int, int, int)")
        );
        // A value that is not a tuple literal is checked element by element.
        assert_eq!(body("(a, b) = p;"), mismatch("int", "string"));
        // A place of unknown type checks nothing, and the others still do.
        assert_eq!(body("(a, xs[0]) = (\"x\", 2);"), mismatch("int", "string"));
        // A positional struct deconstructs, unchecked as in a declaration;
        // any other struct does not.
        assert_eq!(body("(a, b) = s;"), none());
        assert_eq!(body("(a, b) = q;"), mismatch("(int, int)", "Q"));
        assert_eq!(body("(a, b) = 1;"), mismatch("(int, int)", "int"));
        // Parentheses around places and discards only group them.
        assert_eq!(body("((a, _)) = (1, 2);"), none());
        assert_eq!(body("(a, (_)) = (1, \"x\");"), none());
        // `_` alone on the left of `=` discards the value (§5.7); anywhere
        // else it is no place, and as a value it names nothing.
        assert_eq!(body("_ = 1;"), none());
        assert_eq!(body("_ = (a, 1);"), none());
        assert_eq!(
            codes(&diagnostics_of("F() {\n    _ = new { X = 1 };\n}\n")),
            ["MR0154"]
        );
        assert_eq!(
            codes(&diagnostics_of("F() {\n    _ += 1;\n}\n")),
            ["MR0186"]
        );
        // No operator takes these tuples: a tuple on the left of `op=` is
        // one `MR0204`, whatever its elements are.
        for text in [
            "F() {\n    (_, _) += (1, 2);\n}\n",
            "F(i: int) {\n    var j = i;\n    (j, _) += (1, 2);\n}\n",
            "F(i: int) {\n    var j = i;\n    (j, j) += (1, 2);\n}\n",
            "F() {\n    (One(), 1) += (1, 2);\n}\nOne(): int => 1;\n",
        ] {
            assert_eq!(codes(&diagnostics_of(text)), ["MR0204"], "{text}");
        }
        // The operation is checked as any operator is: a struct on the right
        // waits for its operators (M3.6), and an error in the operands is the
        // one mistake.
        let q = "public struct Q {\n    public mut Y: int;\n    \
                 public static operator +(a: (int, int), b: Q): (int, int) => a;\n}\n\
                 public struct V {\n    public Y: int;\n}\n";
        for (line, expected) in [
            ("(x, y) += q;", vec![]),
            ("(x, y) <<= q;", vec![]),
            ("(x, y) += v;", vec![]),
            ("(x, y) += Nope;", vec!["MR0127"]),
            ("(x, y) += new { Y = 1 };", vec!["MR0154"]),
            ("(x, x) += (_, 1);", vec!["MR0127"]),
            ("(x, y) += (q, 1);", vec!["MR0204"]),
            // A name that did not resolve in the left tuple is the mistake
            // too, as one in the right operand is.
            ("(x, Nope) += (1, 2);", vec!["MR0127"]),
            ("(int, x) += 1;", vec!["MR0127"]),
            ("((x, Nope), y) += ((1, 2), 3);", vec!["MR0127"]),
            ("(x, _) += One(1);", vec!["MR0205"]),
            ("(x, y) += (Nope, 1);", vec!["MR0127"]),
            // A tuple is never shifted: the left side decides, whatever the
            // count is, and a bad count is a second mistake.
            ("(x, y) <<= \"a\";", vec!["MR0204", "MR0203"]),
            ("(x, _) <<= \"a\";", vec!["MR0204", "MR0203"]),
            ("(x, _) <<= (1, 2);", vec!["MR0204", "MR0203"]),
            ("(x, _) <<= 1;", vec!["MR0204"]),
            ("(x, _) >>>= Nope;", vec!["MR0204", "MR0127"]),
            ("(x, Nope) <<= 1;", vec!["MR0127"]),
            ("(x, Q) += (1, 2);", vec!["MR0127"]),
            // An element whose type is not known yet is no mistake: no
            // operator takes the tuple.
            ("(x, [1, 2]) += (1, 2);", vec!["MR0204"]),
            ("(x, Console.ReadLine()) += (1, 2);", vec!["MR0204"]),
            // `??=` waits for options (M3.5), and checks no element as a
            // place before then.
            ("(x, _) ??= (1, 2);", vec![]),
            ("(x, One()) ??= (1, 2);", vec![]),
            ("(Console.ReadLine(), x) ??= (1, 2);", vec![]),
        ] {
            let text = format!(
                "{q}One(): int => 1;\nF(q: Q, v: V) {{\n    var x = 1;\n    var y = 2;\n    {line}\n}}\n"
            );
            assert_eq!(codes(&diagnostics_of(&text)), expected, "{line}");
        }
        // `??=` waits for options (M3.5), with a tuple on the left as well.
        assert_eq!(
            codes(&diagnostics_of(
                "F(i: int) {\n    var j = i;\n    (j, j) ??= (1, 2);\n}\n"
            )),
            [] as [&str; 0]
        );
        // `_` under `++` or `--` is no place either.
        for step in ["_++;", "++_;", "_--;", "--_;", "(_)++;"] {
            let text = format!("F() {{\n    {step}\n}}\n");
            assert_eq!(codes(&diagnostics_of(&text)), ["MR0186"], "{step}");
        }
        // A type keyword on the left is `MR0127` alone.
        for line in [
            "int = 1;",
            "int++;",
            "--int;",
            "int += 1;",
            "var i = 1;\n    (int, i) = (1, 2);",
            "string = \"a\";",
        ] {
            let text = format!("F() {{\n    {line}\n}}\n");
            assert_eq!(codes(&diagnostics_of(&text)), ["MR0127"], "{line}");
        }
        assert_eq!(
            codes(&diagnostics_of("F() {\n    int = int;\n}\n")),
            ["MR0127", "MR0127"]
        );
        assert_eq!(
            codes(&diagnostics_of("F(a: int) {\n    a += (_, 1).Item1;\n}\n")),
            ["MR0127"]
        );
    }

    #[test]
    fn members_and_extension_functions_return_their_own_type() {
        let mismatches = |text: &str| {
            codes(&diagnostics_of(text))
                .into_iter()
                .filter(|code| code == "MR0203")
                .count()
        };
        let member = "interface I {\n    M(): int;\n    N(): int;\n}\nclass C : I {\n    \
                      I.M(): int {\n        return \"x\";\n    }\n    \
                      async I.N(): int {\n        return \"x\";\n    }\n}\n";
        assert_eq!(mismatches(member), 2);
        let extension = "struct S {\n    X: int;\n}\nextension (s: S) {\n    \
                         F(): int => \"x\";\n    G() {\n        return 1;\n    }\n    \
                         H(): int {\n        return 1;\n    }\n    \
                         async K(): int {\n        return \"x\";\n    }\n}\n";
        assert_eq!(mismatches(extension), 3);
    }

    #[test]
    fn diagnostics_check_the_bodies_of_an_edited_file_only() {
        let mut db = TestDb::new();
        let text = |n: u32| format!("A(): int {{\n    return {n};\n}}\nB(): int => \"b\";\n");
        let project = app(
            &db,
            &[
                ("src/Program.mr", &text(1)),
                ("src/Other.mr", "C(): int => \"c\";\n"),
            ],
        );
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(codes(diagnostics(&db, project)), ["MR0203", "MR0203"]);
        db.take_executed();
        file.set_text(&mut db).to(text(12345));
        assert_eq!(codes(diagnostics(&db, project)), ["MR0203", "MR0203"]);
        let executed = db.take_executed();
        // No memo of `infer` is made for the diagnostics, and only the
        // edited file's are built again.
        assert!(
            !executed.iter().any(|key| key.starts_with("infer(")),
            "{executed:?}"
        );
        let rebuilt = executed
            .iter()
            .filter(|key| key.starts_with("body_diagnostics("))
            .count();
        assert_eq!(rebuilt, 1);
    }

    /// The one diagnostic `text` reports.
    fn only_diagnostic(text: &str) -> mora_diag::Diagnostic {
        let mut reported = diagnostics_of(text);
        assert_eq!(reported.len(), 1, "{text}: {reported:#?}");
        reported.remove(0)
    }

    /// The note of the one diagnostic `text` reports.
    fn note(text: &str) -> String {
        only_diagnostic(text).note.unwrap_or_default()
    }

    /// The one diagnostic `text` reports, which points at `used`, the
    /// first text of the instance member or `this` it is about, and offers
    /// no fix (`MR0149`, `MR0152`).
    fn instance_use(text: &str, used: &str) -> mora_diag::Diagnostic {
        let d = only_diagnostic(text);
        let at = usize::from(d.primary.span.range.start());
        assert_eq!(&text[at..at + used.len()], used);
        assert_eq!(d.fixes.len(), 0);
        d
    }

    /// Each expression of the top-level statements of `text`, as written,
    /// with its type, `?` for one that is unknown.
    fn expr_types(text: &str) -> Vec<String> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let map = body_source_map(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        body.exprs
            .iter()
            .map(|(expr, _)| {
                let ty = inference
                    .expr(expr)
                    .map_or_else(|| "?".to_owned(), |ty| super::ty_name(&db, ty));
                format!("{}: {ty}", &text[map.expr(expr)])
            })
            .collect()
    }

    #[test]
    fn equality_and_ordering_cover_bool_char_and_string() {
        let text = "let a = \"a\" == \"b\";\nlet b = 'a' != 'b';\nlet c = true == false;\n\
                    let d = \"a\" < \"b\";\nlet e = 'a' >= 'b';\nlet f = true & false;\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(reported("let f = true & 1;\n"), ["MR0204"]);
    }

    #[test]
    fn an_expression_body_of_a_unit_function_is_not_a_return_value() {
        let text = "Unit() => 1;\nAlso(): () => 1;\nBlock(): int {\n    return 1;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_call_of_a_local_function_with_params_is_not_checked() {
        // Its arguments are collected into a span later (D-137), so any
        // count of them is left to that check.
        let text = "F() {\n    Sum(params xs: int[]) {}\n    Sum(1, 2);\n    Sum();\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        // Without `params`, the count is checked.
        let text = "F() {\n    Sum(xs: int[]) {}\n    Sum(1, 2);\n}\n";
        assert_eq!(reported(text), ["MR0205"]);
    }

    #[test]
    fn a_local_function_checks_what_it_returns_async_or_not() {
        assert_eq!(reported("F() {\n    G(): int => \"s\";\n}\n"), ["MR0203"]);
        // `return v` in an `async` function gives its declared type (§12).
        assert_eq!(
            reported("F() {\n    async H(): int => \"s\";\n}\n"),
            ["MR0203"]
        );
        assert_eq!(
            reported("async F(): int {\n    return \"s\";\n}\n"),
            ["MR0203"]
        );
    }

    #[test]
    fn expressions_inside_matches_patterns_and_arguments_are_checked() {
        let n = "let n = 1;\n";
        for (text, code) in [
            ("let a = match (n) { 1 => !n, _ => true };\n", "MR0204"),
            (
                "let a = match (n) { var m when n => 1, _ => 2 };\n",
                "MR0203",
            ),
            ("let b = n is < -true;\n", "MR0204"),
            ("let c = (n, n) is (1, < -true);\n", "MR0204"),
            ("Console.WriteLine(!n);\n", "MR0204"),
        ] {
            assert_eq!(reported(&format!("{n}{text}")), [code], "{text}");
        }
    }

    #[test]
    fn every_part_of_an_operation_on_literals_takes_the_target_type() {
        assert_eq!(
            expr_types("let c: sbyte = -(1 + 2);\n"),
            [
                "1: sbyte",
                "2: sbyte",
                "1 + 2: sbyte",
                "-(1 + 2): sbyte",
                // The top-level statements, a block, have no value.
                "let c: sbyte = -(1 + 2);: ?"
            ]
        );
    }

    #[test]
    fn operations_on_literals_follow_their_operators() {
        let fine = "let a: byte = +(1 + 2);\nlet b: float = -(1.5 * 2.0);\n";
        assert_eq!(reported(fine), Vec::<String>::new());
        let huge = "18446744073709551615 * 18446744073709551615 * 18446744073709551615";
        for (text, code) in [
            ("let d: double = 1e308 * 10.0;\n".to_owned(), "MR0010"),
            (format!("let l: long = -({huge});\n"), "MR0010"),
            (format!("let l: double = {huge} * 1.5;\n"), "MR0010"),
            ("let x: double = ~(1.5 + 1.0);\n".to_owned(), "MR0204"),
            ("let b: byte = 1u + 1;\n".to_owned(), "MR0203"),
            ("let f: float = 1.5f * 2.0;\n".to_owned(), "MR0203"),
        ] {
            assert_eq!(reported(&text), [code], "{text}");
        }
    }

    #[test]
    fn negating_the_smallest_value_too_large_to_compute_is_mr0010() {
        // `-(2^63 * 2^63) * 2` is `-2^127`, whose negation overflows.
        let text = "let x: long = -(-(9223372036854775808 * 9223372036854775808) * 2);\n";
        assert_eq!(reported(text), ["MR0010"]);
    }

    #[test]
    fn a_real_the_lexer_reported_is_not_reported_again() {
        let text =
            "let a: double = -1e400;\nlet b: float = -1e400;\nlet c: double = 1e400 + 1.0;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_real_operation_that_is_not_a_number_fits() {
        let text = "let a: double = 0.0 / 0.0;\nlet b: float = 0.0 / 0.0;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn only_arithmetic_prefix_operators_fold_a_huge_literal_operation() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        // `!` and `^` are not arithmetic: the operation follows §7.12, where
        // `ulong * ulong` is a `ulong`, and `!` on it is MR0204.
        assert_eq!(reported(&format!("let a: bool = !{huge};\n")), ["MR0204"]);
        assert_eq!(
            reported(&format!("let b: long = ^{huge};\n")),
            Vec::<String>::new()
        );
        assert_eq!(reported(&format!("let c: long = ~{huge};\n")), ["MR0010"]);
        // Dividing by zero is not folded, so it is not MR0010.
        assert_eq!(
            reported(&format!("let d: ulong = {huge} / 0;\n")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn operators_on_a_struct_are_left_to_its_declarations() {
        let text = "public struct Money {\n    public Cents: int;\n    \
                    public static operator +(a: Money, b: Money): Money => a;\n    \
                    public static operator -(a: Money): Money => a;\n}\n\
                    F(a: Money, b: Money) {\n    let c: int = a + b;\n    let d = -a;\n    \
                    let e = a << 1;\n    let f = 1 << a;\n    var g = a;\n    g++;\n    \
                    g += b;\n    let h = !a;\n    let i = ~a;\n    let j = a == b;\n    \
                    let k = a < b;\n    let m = 1 + a;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        // Comparing a struct with a number is still not defined.
        let mixed = text.replace("let k = a < b;", "let k = a == 1;");
        assert_eq!(reported(&mixed), ["MR0204"]);
    }

    #[test]
    fn tuples_of_different_types_compare_to_an_unknown_result() {
        let text = "let t: (byte, byte) = (1, 2);\nlet a = t == (1, 2);\nlet b = t != (1L, 2);\n\
                    let c = t == t;\nlet d: (Left: byte, Right: byte) = t;\nlet e = t == d;\n";
        assert_eq!(
            local_types(text),
            [
                "t: (byte, byte)",
                "a: ?",
                "b: ?",
                "c: bool",
                "d: (Left: byte, Right: byte)",
                "e: bool"
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
        // A tuple compared with something that is not a tuple of its length
        // is not defined.
        let wrong = "let t = (1, 2);\nlet a = t < t;\nlet b = t == 1;\nlet c = 1 != t;\n\
                     let d = t == (1, 2, 3);\n";
        assert_eq!(reported(wrong), ["MR0204"; 4]);
    }

    #[test]
    fn logical_operators_take_bool_operands() {
        let text = "let n = 1;\nlet a = n && true;\nlet b = true || n;\nlet c = 1 && 2;\n\
                    let d: bool = n > 0 && n < 9;\n";
        // `1 && 2` has two operands that are not `bool`.
        assert_eq!(reported(text), ["MR0204"; 4]);
        assert_eq!(
            note("let n = 1;\nlet a = n || true;\n"),
            "`&&` and `||` combine `bool` values; compare a number explicitly, as in `x != 0`"
        );
        // An operand of unknown type does not hide the other one.
        let text = "let e: Unknwn = 1;\nlet a = e && 1;\nlet b = Console.A() || \"s\";\n";
        let reported = diagnostics_of(text);
        let shown: Vec<_> = reported
            .iter()
            .map(|d| format!("{} {}", d.code, &text[d.primary.span.range]))
            .collect();
        assert_eq!(shown, ["MR0127 Unknwn", "MR0204 1", "MR0204 \"s\""]);
        assert_eq!(
            reported[1].message,
            "`&&` takes `bool` operands, and this one is `int`"
        );
    }

    #[test]
    fn a_compound_assignment_is_named_and_fixed_through_its_value() {
        let text = "var u = 1ul;\nlet l = 2L;\nu += l;\nvar f: float = 1.5;\nf -= l;\n\
                    var s = \"\";\ns += 1;\nvar x = 1.5;\nx <<= 1;\n";
        let reported = diagnostics_of(text);
        let shown: Vec<_> = reported
            .iter()
            .map(|d| {
                let fix = d.fixes.first().map(|fix| {
                    let edit = &fix.edits[0];
                    let at = usize::try_from(u32::from(edit.span.range.start())).unwrap();
                    (
                        fix.message.clone(),
                        text[at..].lines().next().unwrap().to_owned(),
                    )
                });
                (d.message.clone(), fix)
            })
            .collect();
        let fixed = |message: &str, at: &str| Some((message.to_owned(), at.to_owned()));
        assert_eq!(
            shown,
            [
                (
                    "`+=` is not defined for `ulong` and `long`".to_owned(),
                    fixed("convert it explicitly with `(ulong)`", "l;")
                ),
                (
                    "`-=` is not defined for `float` and `long`".to_owned(),
                    fixed("convert it explicitly with `(float)`", "l;")
                ),
                (
                    "`+=` is not defined for `string` and `int`".to_owned(),
                    None
                ),
                ("`<<=` cannot shift a `double`".to_owned(), None),
            ]
        );
    }

    #[test]
    fn every_cast_fix_for_mixed_operands_type_checks() {
        const NUMERIC: [&str; 12] = [
            "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "nint", "nuint",
            "float", "double",
        ];
        const OPERATORS: [&str; 11] = ["+", "-", "*", "/", "%", "&", "|", "^", "+%", "-%", "*%"];
        let mut text = String::new();
        let mut index = 0;
        for place in NUMERIC {
            for value in NUMERIC {
                for op in OPERATORS {
                    // Writing to a `String` cannot fail.
                    let _ = write!(
                        text,
                        "F{index}(p: {place}, x: {value}) {{\n    var q = p;\n    q {op}= x;\n    \
                         let r = p {op} x;\n}}\n"
                    );
                    index += 1;
                }
            }
        }
        let fixed = apply_fixes(&text);
        assert!(fixed.edits > 100, "{}", fixed.edits);
        assert_eq!(fixed.remaining, Vec::<String>::new());
    }

    /// `text` with every fix its diagnostics offer applied.
    fn apply_fixes(text: &str) -> crate::test_support::Fixed {
        crate::test_support::apply_fixes(text, diagnostics_of)
    }

    #[test]
    fn a_number_in_a_logical_operation_is_compared_with_zero() {
        let text = "F(n: int, x: double, b: byte, c: bool, s: string) {\n    \
                    let a = n && c;\n    let d = c || x;\n    let e = (n + 1) && c;\n    \
                    let f = (n & 1) || c;\n    let g = (c ? n : 2) && c;\n    \
                    let h = c && (b ^ b);\n    let i = s && c;\n}\n";
        let fixed = apply_fixes(text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        let fixed = fixed.text;
        for expected in [
            "let a = n != 0 && c;",
            "let d = c || x != 0;",
            "let e = (n + 1 != 0) && c;",
            "let f = ((n & 1) != 0) || c;",
            "let g = ((c ? n : 2) != 0) && c;",
            "let h = c && ((b ^ b) != 0);",
            "let i = s && c;",
        ] {
            assert!(fixed.contains(expected), "{expected}\n{fixed}");
        }
        // A string has no fix.
        assert_eq!(reported(&fixed), ["MR0204"]);
    }

    #[test]
    fn an_integer_only_operator_on_a_float_says_so() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        for (text, sym) in [
            (
                "let p = 1L;\nlet x: float = 1;\nlet a = p & x;\n".to_owned(),
                "&",
            ),
            (
                "let l = 1L;\nlet f: float = 1;\nlet a = l | f;\n".to_owned(),
                "|",
            ),
            ("var g: float = 1;\nlet l = 1L;\ng &= l;\n".to_owned(), "&="),
            (format!("let a: long = 1.5 & {huge};\n"), "&"),
            (
                "let y = 1.5;\nlet z: nint = 1;\nlet a = y & z;\n".to_owned(),
                "&",
            ),
            ("let y = 1.5;\nlet a = y ^ y;\n".to_owned(), "^"),
        ] {
            assert_eq!(
                note(&text),
                format!("`{sym}` is defined only on integer types"),
                "{text}"
            );
            assert!(diagnostics_of(&text)[0].fixes.is_empty(), "{text}");
        }
    }

    #[test]
    fn an_integer_too_large_to_compute_in_a_real_is_named_as_such() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        let label = |text: &str| {
            let reported = diagnostics_of(text);
            assert_eq!(codes(&reported), ["MR0010"], "{text}");
            reported[0].primary.message.clone().unwrap_or_default()
        };
        assert_eq!(
            label(&format!("let a: double = {huge} * 1.5;\n")),
            "an integer in it is too large to compute"
        );
        assert_eq!(
            label(&format!("let b: double = -({huge} * 1.5) * 2.0;\n")),
            "an integer in it is too large to compute"
        );
        assert_eq!(
            label("let c: double = 1e308 * 10.0;\n"),
            "this would be infinity as a `double`"
        );
        assert_eq!(
            label(&format!("let d: double = {huge} * 1.5 + 1e308 * 10.0;\n")),
            "this would be infinity as a `double`"
        );
        assert_eq!(
            reported(&format!("let e: long = {huge} * 1.5;\n")),
            ["MR0204"]
        );
    }

    #[test]
    fn integer_operators_on_literals_fold_as_integers_for_a_float() {
        let db = TestDb::new();
        let text = "let a: float = ~1;\nlet b: double = 1 & 2;\nlet c: double = (1 | 2) * 3;\n\
                    let d: float = ~1 ^ 2;\nlet e: double = 1 +% 2;\nlet f: double = -0;\n\
                    let g: double = -0.0;\nlet h: int = -1;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        assert_eq!(diagnostics(&db, project), &[]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let inits: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local { init, .. } => *init,
                _ => None,
            })
            .collect();
        let values: Vec<_> = inits.iter().map(|&init| inference.constant(init)).collect();
        let real = |value: f64| Some(Constant::Real(value.to_bits()));
        assert_eq!(
            values,
            [
                real(-2.0),
                real(0.0),
                real(9.0),
                real(-4.0),
                // `+%` is not an operator of §3.5, so the operation follows
                // §7.12; a negated literal is a literal, not an operation.
                None,
                None,
                None,
                None,
            ]
        );
        assert_eq!(
            inference.expr(inits[4]),
            Some(&Ty::Primitive(Primitive::Int))
        );
        let dumped = hir_dump(&db, project, file);
        assert!(dumped.contains("let f#5: double = -0;"), "{dumped}");
    }

    #[test]
    fn an_operation_on_literals_has_its_exact_value_in_the_target_type() {
        let db = TestDb::new();
        let text = "let a: double = 9007199254740993 - 9007199254740992;\n\
                    let b: float = 1 / 3;\nlet c: byte = 200 + 55;\nlet d: float = 0.1 * 1;\n\
                    let e: double = 0.1 * 1;\nlet f: float = 1 + 2;\nlet g: int = 1 + n;\n\
                    let h: byte = 200 + 100;\nlet n = 1;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let values: Vec<_> = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local {
                    init: Some(init), ..
                } => Some(inference.constant(*init)),
                _ => None,
            })
            .collect();
        let float = |value: f32| Some(Constant::Real(f64::from(value).to_bits()));
        assert_eq!(
            values,
            [
                Some(Constant::Real(1.0f64.to_bits())),
                float(0.0),
                Some(Constant::Int(255)),
                float(0.1),
                Some(Constant::Real(0.1f64.to_bits())),
                float(3.0),
                // Not made only of literals, and not fitting.
                None,
                None,
                // A literal on its own is not an operation.
                None,
            ]
        );
        assert_eq!(Constant::Real(0.5f64.to_bits()).to_string(), "0.5");
        let float = f64::from(0.1f32).to_bits();
        assert_eq!(real_text(5f64.to_bits(), Primitive::Double), "5.0");
        assert_eq!(real_text(float, Primitive::Float), "0.1");
        assert_eq!(real_text(float, Primitive::Double), "0.10000000149011612");
        assert_eq!(real_text((-0.0f64).to_bits(), Primitive::Double), "-0.0");
        assert_eq!(real_text((-3f64).to_bits(), Primitive::Float), "-3.0");
        assert_eq!(
            real_text(1e21f64.to_bits(), Primitive::Double),
            "1000000000000000000000.0"
        );
        assert_eq!(real_text(f64::NAN.to_bits(), Primitive::Double), "NaN");
        assert_eq!(real_text(f64::INFINITY.to_bits(), Primitive::Float), "inf");
        assert_eq!(
            real_text(f64::NEG_INFINITY.to_bits(), Primitive::Double),
            "-inf"
        );
        let dumped = hir_dump(&db, project, file);
        assert!(
            dumped.contains("let a#0: double = 9007199254740993 - 9007199254740992{= 1};"),
            "{dumped}"
        );
        assert!(dumped.contains("let b#1: float = 1 / 3{= 0};"), "{dumped}");
    }

    #[test]
    fn a_native_integer_literal_fits_every_target() {
        assert_eq!(
            note("let n: nint = 3000000000;\n"),
            "a literal of type `nint` must be from -2147483648 to 2147483647, the range of \
             `int`, so that it is valid on every target; cast a `long` for a larger value"
        );
        assert_eq!(
            note("let u: nuint = 5000000000;\n"),
            "a literal of type `nuint` must be from 0 to 4294967295, the range of `uint`, so \
             that it is valid on every target; cast a `ulong` for a larger value"
        );
        assert_eq!(
            reported("let n: nint = 2147483647;\nlet m: nint = (nint)3000000000L;\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_operand_of_unknown_type_leaves_the_result_unknown() {
        let text = "let a: int = !Missing;\nlet b: int = Missing && true;\n\
                    let c: int = true || Missing;\nlet d: int = Missing == 1;\n\
                    let e: int = 1 < Missing;\nlet f: int = !Console.A();\n\
                    let g: int = Console.A() != 1;\nlet h: int = Console.A() || true;\n";
        assert_eq!(reported(text), ["MR0127"; 5]);
        assert_eq!(
            local_types(
                "let f = !Console.A();\nlet g = Console.A() == 1;\nlet h = true && Console.A();\n"
            ),
            ["f: ?", "g: ?", "h: ?"]
        );
        // With known operands, the result is a `bool` even after an error.
        let known = "let a: int = !true;\nlet b: int = 1 == 1;\nlet c: int = \"a\" < 1;\n\
                     let d: int = 1 && true;\n";
        assert_eq!(
            reported(known),
            ["MR0203", "MR0203", "MR0204", "MR0203", "MR0204", "MR0203"]
        );
    }

    #[test]
    fn a_real_divided_by_an_integer_zero_is_folded() {
        assert_eq!(reported("let a: double = 1.5 / 0;\n"), ["MR0010"]);
        assert_eq!(reported("let b: double = 1.5 / 0.0;\n"), ["MR0010"]);
        assert_eq!(reported("let c: int = 1 / 0;\n"), Vec::<String>::new());
    }

    #[test]
    fn the_remainder_of_the_smallest_value_by_minus_one_is_zero() {
        let text = "let a: long = (-(9223372036854775808 * 9223372036854775808) * 2) % -1;\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_bitwise_operator_on_a_real_and_a_huge_integer_is_not_folded() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        assert_eq!(
            reported(&format!("let a: long = 1.5 & {huge};\n")),
            ["MR0204"]
        );
        assert_eq!(
            reported(&format!("let b: long = {huge} | 1;\n")),
            ["MR0010"]
        );
    }

    #[test]
    fn an_infinity_at_any_step_does_not_fit() {
        let huge = "(18446744073709551615 * 18446744073709551615 * 18446744073709551615)";
        for (text, expected) in [
            (
                "let a: double = (1e308 * 10.0) - (1e308 * 10.0);\n".to_owned(),
                vec!["MR0010"],
            ),
            (
                "let b: double = (1e308 * 10.0) * 0.0;\n".to_owned(),
                vec!["MR0010"],
            ),
            (
                "let c: float = -(1e308 * 10.0);\n".to_owned(),
                vec!["MR0010"],
            ),
            ("let d: double = 0.0 / 0.0;\n".to_owned(), vec![]),
            // A real for an integer type follows the usual rules.
            ("let e: int = 1e308 * 10.0;\n".to_owned(), vec!["MR0203"]),
            (format!("let f: long = {huge} + 1.5;\n"), vec!["MR0204"]),
            (
                "let g: long = ~(1e308 * 10.0);\n".to_owned(),
                vec!["MR0204"],
            ),
        ] {
            assert_eq!(reported(&text), expected, "{text}");
        }
        // The parts of a fitting real operation take the target type.
        assert_eq!(expr_types("let h: double = 1 + 0.5;\n")[0], "1: double");
    }

    #[test]
    fn tuples_of_tuples_compare_by_shape_at_every_level() {
        let t = "let t = ((1, 2), 3);\n";
        for (text, expected) in [
            ("let a = t == (1, 3);\n", vec!["MR0204"]),
            ("let b = t == ((1, 2, 3), 3);\n", vec!["MR0204"]),
            ("let c = t != ((1, 2), (3, 4));\n", vec!["MR0204"]),
            ("let d = t == ((1L, 2), 3);\n", vec![]),
        ] {
            assert_eq!(reported(&format!("{t}{text}")), expected, "{text}");
        }
        let types = local_types(&format!("{t}let d = t == ((1L, 2), 3);\nlet e = t == t;\n"));
        assert_eq!(types[1..], ["d: ?", "e: bool"]);
    }

    #[test]
    fn literal_operands_take_the_other_operand_type() {
        let text = "let x: float = 1.5;\nlet y = x * 1e39;\nlet z = x * -0.5;\nlet w = 0.5 * x;\n\
                    let n = -1u;\nlet c = 1 ?? 2;\nlet u: ushort = 65535;\n";
        assert_eq!(
            local_types(text),
            [
                "x: float",
                "y: double",
                "z: float",
                "w: float",
                "n: long",
                "c: ?",
                "u: ushort"
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn operands_of_unknown_type_cause_no_diagnostic() {
        let text = "let e: Unknwn = 1;\nlet a = -e;\nlet b = e << 1;\nvar c = e;\nc++;\n\
                    let d = true ? e : \"s\";\nlet f = true ? Console.A() : 1;\n";
        assert_eq!(reported(text), ["MR0127"]);
        assert_eq!(
            reported("let a = -\"s\";\nlet b = -true;\nlet c = -'c';\n"),
            ["MR0204"; 3]
        );
    }

    #[test]
    fn initializers_check_against_their_declared_type() {
        let text = "const Limit: byte = 300;\nclass A {\n    X: byte = 300;\n}\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0010", "MR0010"]);
    }

    #[test]
    fn tuples_match_by_length_and_element_types_but_not_names() {
        let text = "let t: (int, int) = (1, 2, 3);\nlet p = (1, 2);\n\
                    let q: (Left: int, Right: int) = p;\nlet r: (int,) = p;\n";
        assert_eq!(reported(text), ["MR0203", "MR0203"]);
    }

    #[test]
    fn members_after_a_value_and_optional_members() {
        let text = "struct P {\n    X: int;\n}\nstatic Origin: P = new P { X = 1 };\n\
                    let p = Origin;\nlet v = p?.X;\nlet x = Origin.X;\n";
        assert_eq!(local_types(text), ["p: P", "v: ?", "x: int"]);
        assert_eq!(
            reported(&format!("{text}let s: string = Origin.X;\n")),
            ["MR0203"]
        );
    }

    #[test]
    fn a_parameter_without_a_name_leaves_the_others_unknown() {
        let text = "F(a: int = !1, : string, c: bool): bool => c;\nlet n = 1;\n";
        assert_eq!(reported(text), ["MR0204"]);
    }

    #[test]
    fn notes_say_why_a_value_does_not_convert() {
        for (text, expected) in [
            (
                "let b: bool = 1;\n",
                "there is no implicit conversion to or from `bool`",
            ),
            (
                "let c: char = 65;\n",
                "there is no implicit conversion to or from `char`",
            ),
            (
                "let d: double = 9007199254740993;\n",
                "a `double` holds every integer from -9007199254740992 to 9007199254740992 exactly, and only some beyond",
            ),
            (
                "let g: float = 16777217;\n",
                "a `float` holds every integer from -16777216 to 16777216 exactly, and only some beyond",
            ),
            (
                "let n: nint = 1;\nlet i = 2;\nlet a = n + i;\n",
                "`nint` and `nuint` mix with other integer types only through an explicit cast",
            ),
            (
                "let s = \"s\" + 1;\n",
                "`+` joins two `string` values; interpolation formats other values as text, as in `$\"{a}{b}\"`",
            ),
            ("let s = \"s\" - 1;\n", ""),
            (
                "let u = true < false;\n",
                "`bool` values are not ordered and are not numbers",
            ),
            (
                "let v = !1;\n",
                "`!` negates a `bool`; compare a number explicitly, as in `x == 0`",
            ),
            ("let w = ~1.5;\n", "`~` is defined only on integer types"),
        ] {
            assert_eq!(note(text), expected, "{text}");
        }
        let reported = diagnostics_of("let s: string = (1, 2);\nlet f: float = 1e39;\n");
        assert!(reported[0].fixes.is_empty());
        assert_eq!(
            reported[1].message,
            "the literal `1e39` is too large for a `float`"
        );
    }

    #[test]
    fn mixed_and_negated_native_integers_get_a_cast_fix() {
        let text = "let n: nint = 1;\nlet b: byte = 2;\nlet k = b + n;\nlet nu: nuint = 1;\nlet x = -nu;\n";
        let fixes: Vec<_> = diagnostics_of(text)
            .iter()
            .map(|d| d.fixes[0].message.clone())
            .collect();
        assert_eq!(
            fixes,
            [
                "convert it explicitly with `(nint)`",
                "convert it explicitly with `(nint)`"
            ]
        );
    }

    #[test]
    fn a_cast_between_char_and_an_integer_points_to_a_function() {
        let named = "a cast `(T)x` converts only from one numeric type to another, and a `char` is not a number; convert between `char` and integers with a named conversion function";
        let plain = "a cast `(T)x` converts only from one numeric type to another";
        for (text, expected) in [
            ("let a = (int)'c';\n", named),
            ("let l = 1L;\nlet b = (char)l;\n", named),
            ("let c = (int)true;\n", plain),
            ("let d = (string)'c';\n", plain),
        ] {
            assert_eq!(note(text), expected, "{text}");
        }
        let text = "let t = true;\nlet c = (int)t;\n";
        let reported = diagnostics_of(text);
        let operand = reported[0].secondary[0].span.range;
        assert_eq!(&text[operand], "t");
    }

    #[test]
    fn types_are_named_as_code_writes_them() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "struct P {}\n")]);
        let file = file_named(&db, project, "src/Program.mr");
        let point = crate::test_db::item_named(&db, project, file, "P");
        let int = TupleElem {
            name: None,
            ty: Ty::Primitive(Primitive::Int),
        };
        let named = TupleElem {
            name: Some("Id".to_owned()),
            ty: Ty::Struct(point),
        };
        assert_eq!(
            super::ty_name(&db, &Ty::Tuple(vec![named, int.clone()])),
            "(Id: P, int)"
        );
        assert_eq!(super::ty_name(&db, &Ty::Tuple(vec![int])), "(int,)");
        assert_eq!(super::ty_name(&db, &Ty::Error), "_");
        assert_eq!(super::ty_name(&db, &Ty::Never), "!");
    }

    #[test]
    fn the_never_type_converts_to_every_type_and_nothing_to_it() {
        let int = Ty::Primitive(Primitive::Int);
        for to in [int.clone(), Ty::String, Ty::UNIT, Ty::Never] {
            assert!(super::converts(&Ty::Never, &to), "{to:?}");
        }
        assert!(!super::converts(&int, &Ty::Never));
        assert!(!super::converts(&Ty::String, &Ty::Never));
        assert!(super::is_known(&Ty::Never));
    }

    #[test]
    fn a_diverging_call_has_the_never_type() {
        let text = "let c = 1 < 2;\nlet x = Todo();\nlet y: int = panic(\"no\");\n\
                    let a = c && Todo();\nlet b = Unreachable() || c;\nlet n = 1 + Todo();\n\
                    let k = c ? 1 : Todo();\nlet j = c ? Todo() : 2L;\nlet m = !Todo();\n\
                    let s = Std.Todo();\nlet t = -Todo();\nlet u = Todo() << 1;\n\
                    let w = Todo() == 1;\nlet v = Console.Todo();\n\
                    let p = Todo() == Todo();\nlet q = Todo() + Todo();\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            local_types(text),
            [
                "c: bool", "x: !", "y: int", "a: bool", "b: bool", "n: int", "k: int", "j: long",
                "m: bool", "s: !", "t: !", "u: !", "w: bool", "v: ?", "p: bool", "q: !",
            ]
        );
        // A value that never exists is never converted.
        assert_eq!(conversions(text), Vec::<String>::new());
    }

    #[test]
    fn the_initializer_of_a_struct_field_has_the_field_type() {
        assert_eq!(
            reported("struct P {\n    X: long = 7;\n    static S: int = 1;\n}\n"),
            Vec::<String>::new()
        );
        assert_eq!(reported("struct P {\n    X: string = 7;\n}\n"), ["MR0203"]);
        assert_eq!(
            reported("struct P {\n    static S: bool = 1;\n}\n"),
            ["MR0203"]
        );
    }

    #[test]
    fn this_is_the_struct_a_method_is_declared_in() {
        let decls = "struct P {\n    X: int;\n    Get(): int => this.X;\n    \
                     Same(): P => this;\n}\nclass C {\n    M(): int => this.Y;\n}\n";
        assert_eq!(reported(decls), Vec::<String>::new());
        assert_eq!(
            reported("struct P {\n    X: int;\n    Get(): string => this.X;\n}\n"),
            ["MR0203"]
        );
    }

    #[test]
    fn a_member_index_or_method_of_the_never_type_has_the_never_type() {
        let text = "let a = Todo().Length;\nlet b = Todo()[0];\nlet c = Todo().Foo(1);\n\
                    let d: int = Todo().X.Y;\nlet e = panic(\"no\")[Todo()];\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            local_types(text),
            ["a: !", "b: !", "c: !", "d: int", "e: !"]
        );
        assert_eq!(conversions(text), Vec::<String>::new());
    }

    #[test]
    fn a_call_has_the_return_type_of_what_it_calls() {
        let text = "Add(a: int, b: int): long => a + b;\nstruct P {\n    X: int;\n    \
                    Get(): int => X;\n    static Make(): string => \"\";\n}\n\
                    class C {\n    static Make(n: int): string => \"\";\n}\n\
                    static Origin: P = new P { X = 1 };\nasync Later(): int => 1;\n\
                    Id<T>(x: T): T => x;\nSum(params xs: int[]): int => 0;\n\
                    let s = Add(1, 2);\nlet p = Origin;\nlet g = p.Get();\nlet h = Origin.Get();\n\
                    let c = C.Make(1);\nlet l = Later();\nlet i = Id(1);\nlet m = Sum(1, 2);\n\
                    let w = p.Make();\nlet o = Console.ReadLine();\n";
        assert_eq!(
            local_types(text),
            [
                "s: long",
                "p: P",
                "g: int",
                "h: int",
                "c: string",
                // Async, generic, `params`, static through a value, and the
                // standard library are not checked yet.
                "l: ?",
                "i: ?",
                "m: ?",
                "w: ?",
                "o: ?",
            ]
        );
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn arguments_are_checked_against_the_one_declaration() {
        let decls = "F(a: int, b: int = 2): int => a;\nB(x: byte) {}\n\
                     struct P {\n    X: int;\n    Set(v: int) {}\n}\n\
                     Local() {\n    G(x: byte): long => x;\n    let y: int = G(1);\n}\n";
        assert_eq!(reported(decls), ["MR0203"]);
        for (call, expected) in [
            ("let x = F(1);\nlet y = F(b: 3, a: 1);\nB(1);\n", vec![]),
            ("F(\"s\");\n", vec!["MR0203"]),
            ("B(300);\n", vec!["MR0010"]),
            ("F();\n", vec!["MR0205"]),
            ("F(1, 2, 3);\n", vec!["MR0205"]),
            ("F(c: 1);\n", vec!["MR0207"]),
            ("F(1, a: 2);\n", vec!["MR0207"]),
            ("F(a: 1, 2);\n", vec!["MR0207"]),
            ("F(a: 1, a: 2);\n", vec!["MR0207"]),
            ("F(p: 1, q: 2);\n", vec!["MR0207"]),
            ("UseP(p: P) {\n    p.Set(\"s\");\n}\n", vec!["MR0203"]),
        ] {
            let mut expected = expected;
            expected.insert(0, "MR0203");
            assert_eq!(reported(&format!("{decls}{call}")), expected, "{call}");
        }
        let messages =
            messages(&diagnostics_of(&format!("{decls}F();\nF(1, 2, 3);\nF(c: 1);\n"))[1..]);
        assert_eq!(
            messages,
            [
                "missing an argument for `a` in this call of `F(a: int, b: int = ...): int`",
                "too many arguments in this call of `F(a: int, b: int = ...): int`",
                "`F` has no parameter named `c`",
            ]
        );
    }

    #[test]
    fn overload_resolution_picks_the_best_applicable_overload() {
        let decls = "O(x: int): int => 1;\nO(x: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): string => \"\";\n\
                     Q(x: int, y: long) {}\nQ(x: long, y: int) {}\n\
                     D(x: int): int => 1;\nD(x: int, y: int = 0): string => \"\";\n";
        let text = format!(
            "{decls}let a = O(1);\nlet b = O(\"s\");\nlet c = P(1);\nlet d = P(1L);\n\
             let e = D(1);\nlet f = D(1, 2);\nlet u = O(Console.A());\n"
        );
        assert_eq!(
            local_types(&text),
            [
                "a: int",
                "b: string",
                "c: int",
                "d: string",
                "e: int",
                "f: string",
                "u: ?"
            ]
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        let n = "let n = 1;\n";
        for (call, code) in [
            ("O(true);\n", "MR0205"),
            ("Q(1, 1);\n", "MR0206"),
            ("Q(n, n);\n", "MR0206"),
        ] {
            assert_eq!(reported(&format!("{decls}{n}{call}")), [code], "{call}");
        }
        let reported = diagnostics_of(&format!("{decls}O(true);\nQ(1, 1);\n"));
        assert_eq!(
            reported[0].message,
            "no overload of `O` takes these arguments"
        );
        assert_eq!(reported[0].secondary.len(), 2);
        assert_eq!(reported[1].message, "the call of `Q` is ambiguous");
        assert_eq!(reported[1].secondary.len(), 2);
    }

    /// Which declaration of `O`, counted from 0, the call of `O` in the
    /// top-level statements of `text` calls, `None` when it is unresolved.
    fn called_overload(text: &str) -> Option<usize> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let call = body
            .exprs
            .iter()
            .find(|(_, expr)| {
                matches!(expr, Expr::Call { callee, .. }
                if matches!(&body.exprs[*callee], Expr::Path(path) if path.segments == ["O"]))
            })
            .map(|(id, _)| id)
            .unwrap();
        let Some(Callee::Item(called)) = inference.callee(call) else {
            return None;
        };
        item_tree(&db, file)
            .items()
            .filter(|(_, item)| item.name == "O")
            .position(|(idx, _)| Scope::file(project, file).item(idx) == called)
    }

    /// The parameter each argument of the call of `O` in the top-level
    /// statements of `text` goes to.
    fn bound_arguments(text: &str) -> Option<Vec<usize>> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let inference = infer(&db, project, file, BodyOwner::TopLevel);
        let call = body
            .exprs
            .iter()
            .find(|(_, expr)| {
                matches!(expr, Expr::Call { callee, .. }
                if matches!(&body.exprs[*callee], Expr::Path(path) if path.segments == ["O"]))
            })
            .map(|(id, _)| id)
            .unwrap();
        inference.arguments(call).map(<[usize]>::to_vec)
    }

    #[test]
    fn the_parameter_of_each_argument_is_recorded() {
        let one = "O(a: int, b: int = 2, c: int = 3): int => a;\n";
        assert_eq!(
            bound_arguments(&format!("{one}let x = O(1, c: 5);\n")),
            Some(vec![0, 2])
        );
        assert_eq!(
            bound_arguments(&format!("{one}let x = O(c: 5, a: 1);\n")),
            Some(vec![2, 0])
        );
        let overloads = "O(x: int): int => 1;\nO(y: string, x: int): int => 2;\n";
        assert_eq!(
            bound_arguments(&format!("{overloads}let a = O(x: 1, y: \"s\");\n")),
            Some(vec![1, 0])
        );
        assert_eq!(bound_arguments(&format!("{one}let x = O();\n")), None);
    }

    #[test]
    fn the_chosen_overload_is_recorded() {
        let text = "O(x: int): int => 1;\nO(x: string): string => \"\";\nlet a = O(\"s\");\n";
        assert_eq!(called_overload(text), Some(1));
    }

    #[test]
    fn an_argument_marker_must_suit_the_parameter_mode() {
        let text = "Bump(ref x: int) {}\nRead(x: int) {}\nTake(move x: int) {}\n\
                    var n = 1;\nBump(n);\nBump(in n);\nRead(ref n);\nRead(move n);\n\
                    Take(n);\nTake(move n);\nRead(in n);\n";
        assert_eq!(reported(text), ["MR0205"; 4]);
        let fixed = apply_fixes(text);
        assert_eq!(fixed.edits, 4);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        let expected = "Bump(ref n);\nBump(ref n);\nRead(n);\nRead(n);\n";
        assert!(fixed.text.contains(expected), "{}", fixed.text);
        // A local function's markers are checked against its modes too.
        assert_eq!(
            reported("F() {\n    G(x: int) {}\n    var n = 1;\n    G(ref n);\n}\n"),
            ["MR0205"]
        );
        assert_eq!(
            reported("F() {\n    G(ref x: int) {}\n    var n = 1;\n    G(n);\n    G(ref n);\n}\n"),
            ["MR0205"]
        );
    }

    #[test]
    fn a_marker_fix_keeps_a_comment_and_needs_an_argument() {
        let fixed = apply_fixes("Read(x: int) {}\nvar n = 1;\nRead(move /* n */ n);\n");
        assert!(fixed.text.contains("Read(/* n */ n);"), "{}", fixed.text);
        // The parser reports the missing argument, so the call is not
        // checked and its marker reports nothing more.
        assert_eq!(
            reported("Read(x: int) {}\nRead(move );\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn calling_a_value_that_is_not_a_function_is_mr0208() {
        let text = "struct P {\n    X: int;\n}\nstatic Origin: P = new P { X = 1 };\n\
                    const Max: int = 1;\nlet n = 1;\nn();\nlet p = Origin;\np.X();\n\
                    Origin.X();\nMax();\n(1, 2)();\n";
        assert_eq!(reported(text), ["MR0208"; 5]);
        assert_eq!(
            messages(&diagnostics_of("let n = 1;\nn(2);\n")),
            ["cannot call a value of type `int`"]
        );
        // A getter has the type its body declares, and a method is called.
        // A static field, getter or constant called through a value is
        // `MR0155` once statics check it (D-152), and nothing before.
        let getters = "struct S {\n    public X: int;\n    public G: int => X;\n    \
                       public static K: int => 1;\n    public M(): int => X;\n    \
                       public static mut F: int = 1;\n    public const N: int = 2;\n}\n";
        for (call, expected) in [
            ("s.G();", vec!["MR0208"]),
            ("s.M();", vec![]),
            ("s.K();", vec![]),
            ("s.F();", vec![]),
            ("s.N();", vec![]),
            // A name the struct does not declare may be an extension
            // member, which comes later.
            ("s.Nope();", vec![]),
        ] {
            let text = format!("{getters}H(s: S) {{\n    {call}\n}}\n");
            assert_eq!(reported(&text), expected, "{call}");
        }
        // Through the type, or by its bare name, a static member is checked
        // now: a getter or a constant of type `int` is not callable.
        // Through a value a static overload is not checked before statics,
        // and an instance overload is called as any method (D-152).
        let statics = "struct S {\n    public X: int;\n    public G: int => X;\n    \
                       public static K: int => 1;\n    public const N: int = 2;\n    \
                       public static S2(x: int): int => 1;\n    public S2(): int => X;\n}\n";
        for (call, expected) in [
            ("S.K();", vec!["MR0208"]),
            ("S.N();", vec!["MR0208"]),
            ("s.S2(1);", vec![]),
            ("s.S2();", vec![]),
            ("s.S2(1, 2);", vec![]),
        ] {
            let text = format!("{statics}H(s: S) {{\n    {call}\n}}\n");
            assert_eq!(reported(&text), expected, "{call}");
        }
        let bare = "struct S {\n    public X: int;\n    public G: int => X;\n    \
                    F(): int {\n        G();\n        return 1;\n    }\n}\n";
        assert_eq!(reported(bare), ["MR0208"]);
        // Calls of unknown things, and arguments inside them, still work.
        let unknown = "let f = (x: int) => x;\nf(1);\nlet s: string = Console.ReadLine(!1);\n";
        assert_eq!(reported(unknown), ["MR0204"]);
    }

    #[test]
    fn a_failed_call_still_types_its_literal_arguments() {
        let types = expr_types("F(a: int): int => a;\nF(1, 2);\n");
        assert!(types.contains(&"2: int".to_owned()), "{types:?}");
    }

    #[test]
    fn named_argument_messages_say_what_is_wrong() {
        let text = "Greet(name: string, greeting: string = \"Hi\") {}\n\
                    Greet(name: \"Ada\", name: \"Bo\");\nGreet(\"Ada\", name: \"Bo\");\n\
                    Greet(greeting: \"Hi\", \"Ada\");\n";
        assert_eq!(
            messages(&diagnostics_of(text)),
            [
                "the argument `name` is named twice",
                "the parameter `name` is given by position and by name",
                "a positional argument after a named one",
            ]
        );
    }

    #[test]
    fn a_local_function_default_value_is_its_own() {
        let text = "F() {\n    G(x: int, y: int = 1): int => x;\n    let z: string = G(1);\n}\n";
        assert_eq!(reported(text), ["MR0203"]);
    }

    #[test]
    fn signatures_in_messages_show_modes_and_return_types() {
        let text = "Bump(ref x: int) {}\nTake(move x: int): int => x;\nRead(x: int) {}\n\
                    var n = 1;\nBump(n);\nTake(in n);\nRead();\n";
        assert_eq!(
            messages(&diagnostics_of(text)),
            [
                "the argument for `ref x` in `Bump(ref x: int)` must be marked `ref`",
                "the parameter `x` of `Take(move x: int): int` does not take an argument marked `in`",
                "missing an argument for `x` in this call of `Read(x: int)`",
            ]
        );
    }

    #[test]
    fn overloads_with_unknown_or_marked_arguments() {
        let decls = "O(x: int): int => 1;\nO(x: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): string => \"\";\n\
                     R(x: double): int => 1;\nR(x: string): string => \"\";\n\
                     M(ref x: int): int => 1;\nM(x: string): string => \"\";\n\
                     struct S {\n    X: int;\n    Get(): int => X;\n}\n\
                     static Origin: S = new S { X = 1 };\n";
        let text = format!(
            "{decls}let g: int? = 1;\nlet a = O(g);\nlet b = P(Console.A());\nlet c = R(1.5);\n\
             var n = 1;\nlet d = M(ref n);\nlet s = Origin;\nlet e = s?.Get();\n"
        );
        assert_eq!(
            local_types(&text),
            [
                "g: _", "a: ?", "b: ?", "c: int", "n: int", "d: int", "s: S", "e: ?"
            ]
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        assert_eq!(reported(&format!("{decls}var n = 1;\nM(n);\n")), ["MR0205"]);
    }

    #[test]
    fn unknown_argument_leaves_overloads_unresolved() {
        let decls = "O(x: int, y: int): int => 1;\nO(x: long, y: string): string => \"\";\n\
                     P(x: int): int => 1;\nP(x: long): int => 2;\n";
        let text = format!(
            "{decls}let s: string = O(1, Console.ReadLine());\nlet t = O(1, Console.ReadLine());\n\
             let u = P(Console.ReadLine());\n"
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        let types = local_types(&text);
        assert_eq!(types[1..], ["t: ?", "u: int"]);
    }

    #[test]
    fn a_literal_prefers_its_natural_type() {
        let text = "A(x: int): int => 1;\nA(x: uint): string => \"\";\n\
                    B(x: long): int => 1;\nB(x: float): string => \"\";\n\
                    C(x: byte): string => \"\";\nC(x: int): int => 1;\n\
                    D(x: float): string => \"\";\nD(x: double): int => 1;\n\
                    E(x: byte): int => 1;\nE(x: short): string => \"\";\n\
                    let a = A(1);\nlet b = B(1);\nlet c = C(1);\nlet d = D(0.1);\nlet e = E(1);\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            local_types(text),
            ["a: int", "b: int", "c: int", "d: int", "e: int"]
        );
    }

    #[test]
    fn an_async_callee_is_not_checked() {
        let text = "async Later(ref x: int): int => 1;\nvar n = 1;\nlet a = Later(n);\n\
                    let b = Later(\"s\", 2);\nF() {\n    async G(x: int): int => x;\n    G(\"s\");\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(local_types(text)[1..], ["a: ?", "b: ?"]);
    }

    #[test]
    fn named_argument_order_is_checked_for_every_call() {
        for text in [
            "Console.WriteLine(a: 1, 2);\n",
            "Id<T>(x: T): T => x;\nlet a = Id(x: 1, 2);\n",
        ] {
            assert_eq!(reported(text), ["MR0207"], "{text}");
        }
        assert_eq!(reported("let n = 1;\nn(a: 1, 2);\n"), ["MR0208", "MR0207"]);
    }

    #[test]
    fn a_recovered_argument_list_is_not_checked() {
        for call in ["F(,1);\n", "F(1,,2);\n", "F(;\n", "F(1\n"] {
            assert_eq!(
                reported(&format!("F(a: int, b: int) {{}}\n{call}")),
                Vec::<String>::new(),
                "{call}"
            );
        }
    }

    #[test]
    fn ref_is_offered_only_for_a_place_of_a_fitting_type() {
        let text = "Bump(ref x: int) {}\nstruct P {\n    X: int;\n}\nvar n = 1;\n\
                    Bump(\"s\");\nBump(n + 1);\nBump(n);\n";
        let fixes: Vec<_> = diagnostics_of(text)
            .iter()
            .filter(|d| d.code.to_string() == "MR0205")
            .map(|d| d.fixes.len())
            .collect();
        assert_eq!(fixes, [0, 0, 1]);
        assert_eq!(
            reported("Bump(ref x: int) {}\nBump(ref 1);\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_name_every_overload_takes_by_position_is_mr0207() {
        let text = "O(x: int) {}\nO(x: string) {}\nO(1, x: 2);\nO(y: 1);\n";
        assert_eq!(reported(text), ["MR0207", "MR0205"]);
    }

    #[test]
    fn a_callee_has_its_own_type() {
        let types = expr_types("const Max: int = 1;\nMax();\n");
        assert!(types.contains(&"Max: int".to_owned()), "{types:?}");
    }

    #[test]
    fn ranking_does_not_depend_on_declaration_order() {
        let text = "E(x: short): string => \"\";\nE(x: byte): int => 1;\nlet e = E(1);\n\
                    H(x: int): int => 1;\nH(x: long): string => \"\";\n\
                    let h: int = H(18446744073709551615 * 18446744073709551615);\n";
        assert_eq!(local_types(text), ["e: int", "h: int"]);
        // The value too large to compute is still reported once the
        // overload is chosen.
        assert_eq!(reported(text), ["MR0010"]);
    }

    #[test]
    fn overloads_that_tie_are_ambiguous() {
        // Neither takes a default, and each fits one argument better.
        let text = "S(x: byte) {}\nS(x: sbyte) {}\nS(1);\n\
                    Q(x: byte, y: short) {}\nQ(x: short, y: byte) {}\nQ(1, 1);\n";
        assert_eq!(reported(text), ["MR0206", "MR0206"]);
    }

    #[test]
    fn the_one_applicable_overload_is_called_whatever_the_other_arguments() {
        let text = "O(x: int, y: byte): int => 1;\nO(x: string, y: byte): string => \"\";\n\
                    let a = O(Console.A(), 300);\n";
        // No overload takes `300` as its `byte`, whatever the argument of
        // unknown type is, so the call is MR0205.
        assert_eq!(reported(text), ["MR0205"]);
        let text = "O(x: int, y: byte): int => 1;\nO(x: string, y: string): string => \"\";\n\
                    let a = O(Console.A(), 1);\n";
        assert_eq!(called_overload(text), Some(0));
        assert_eq!(local_types(text), ["a: int"]);
        // Two applicable overloads leave the call unresolved.
        let text = "O(x: int, y: int): int => 1;\nO(x: string, y: int): int => 2;\n\
                    let a = O(Console.A(), 1);\n";
        assert_eq!(called_overload(text), None);
    }

    #[test]
    fn an_unknown_parameter_type_leaves_overloads_unresolved() {
        let text = "F(x: long): int => 1;\nF(x: int?): string => \"\";\nvar n = 1;\n\
                    let s: string = F(n);\nG(x: int): int => 1;\nG(x: List<int>): int => 2;\n\
                    let g = G(1);\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(local_types(text), ["n: int", "s: string", "g: int"]);
    }

    #[test]
    fn a_ref_argument_has_exactly_its_parameter_type() {
        let text = "Bump(ref x: long) {}\nvar n = 1;\nBump(ref n);\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0203"]);
        assert!(found[0].fixes.is_empty());
        assert_eq!(
            found[0].note.as_deref(),
            Some(
                "a `ref` argument is the caller's place itself, which the function reads and \
                 writes, so it has exactly the parameter's type"
            )
        );
        // Without `ref`, the marker and the type are both wrong, and `ref`
        // alone would not fix it, so it is not offered.
        let missing = diagnostics_of("Bump(ref x: long) {}\nvar n = 1;\nBump(n);\n");
        assert_eq!(codes(&missing), ["MR0205", "MR0203"]);
        assert!(missing[0].fixes.is_empty());
        let overloads = "R(ref x: long): int => 1;\nR(ref x: int): string => \"\";\n\
                         Read(x: long): int => 1;\nvar n = 1;\nlet r = R(ref n);\n\
                         let i = Read(in n);\n";
        assert_eq!(local_types(overloads)[1..], ["r: string", "i: int"]);
        assert_eq!(reported(overloads), Vec::<String>::new());
    }

    #[test]
    fn a_constant_or_a_function_is_not_a_place() {
        let text = "Bump(ref x: int) {}\nconst Max: int = 1;\nTwice(): int => 2;\n\
                    Bump(Max);\nBump(Twice);\nstruct P {\n    X: int;\n    M() {\n        Bump(X);\n    }\n}\n";
        let fixes: Vec<_> = diagnostics_of(text).iter().map(|d| d.fixes.len()).collect();
        assert_eq!(fixes, [0, 0, 1]);
    }

    #[test]
    fn a_recovered_argument_list_keeps_an_agreed_type() {
        let text = "F(a: int, b: int): int => a;\nO(x: int): int => 1;\nO(x: string): string => \"\";\n\
                    let a = F(,1);\nlet o = O(,1);\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(local_types(text), ["a: int", "o: ?"]);
        // The order of named arguments and what is called are checked still.
        assert_eq!(reported("F(a: int, b: int) {}\nF(b: 1, 2,\n"), ["MR0207"]);
        assert_eq!(
            reported("F(a: int, b: int) {}\nF(a: 1,\n"),
            Vec::<String>::new()
        );
        assert_eq!(reported("let n = 1;\nn(,1);\n"), ["MR0208"]);
        // An unclosed list after a missing argument ends with only that one.
        let dumped = {
            let db = TestDb::new();
            let project = app(&db, &[("src/Program.mr", "F(a: int) {}\nF(1,\n")]);
            hir_dump(&db, project, file_named(&db, project, "src/Program.mr"))
        };
        assert!(dumped.contains("(1, <missing>);"), "{dumped}");
    }

    #[test]
    fn return_types_agree_whatever_their_element_names() {
        let text = "T(x: int): (a: int, b: int) => (1, 2);\nT(x: string): (int, int) => (1, 2);\n\
                    let t = T(Console.ReadLine());\n";
        assert_eq!(local_types(text), ["t: (a: int, b: int)"]);
    }

    #[test]
    fn a_method_must_be_visible_from_the_call() {
        let store = "public struct S {\n    public X: int;\n    private Secret(): int => 1;\n    \
                     Folder(): int => 2;\n    public Open(): int => 3;\n    \
                     Peek(other: S): int => other.Secret();\n}\n";
        let program = "F(s: UsersApi.Store.S) {\n    let a: string = s.Secret();\n    \
                       let b: string = s.Folder();\n    let c: string = s.Open();\n}\n";
        let db = TestDb::new();
        let project = app(
            &db,
            &[("src/Program.mr", program), ("src/Store/S.mr", store)],
        );
        let found = diagnostics(&db, project);
        assert_eq!(codes(found), ["MR0129", "MR0129", "MR0203"]);
        assert_eq!(
            found[0].message,
            "`Secret` is private to the type that declares it"
        );
        assert_eq!(&program[found[0].primary.span.range], "Secret");
        // Inside its type, a private method is visible.
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", store)]);
        assert_eq!(codes(diagnostics(&db, project)), Vec::<String>::new());
    }

    #[test]
    fn a_hidden_field_without_an_initializer_says_why_it_is_hidden() {
        for (modifier, why) in [
            ("private ", "is private to `S`"),
            ("", "is visible only in the folder that declares `S`"),
            ("file ", "is visible only in the file that declares `S`"),
        ] {
            let store = format!("public struct S {{\n    {modifier}X: int;\n}}\n");
            let program = "let s = new UsersApi.Store.S {};\n";
            let db = TestDb::new();
            let project = app(
                &db,
                &[("src/Program.mr", program), ("src/Store/S.mr", &store)],
            );
            let found = diagnostics(&db, project);
            assert_eq!(codes(found), ["MR0144"], "{store}");
            assert_eq!(found[0].message, "`S` cannot be created memberwise here");
            assert_eq!(
                found[0].primary.message,
                Some(format!("`X` has no initializer and {why}"))
            );
        }
    }

    #[test]
    fn ref_is_offered_only_for_known_types() {
        let text = "Bump(ref x: int?) {}\nTake(ref x: int) {}\nvar n: int? = 1;\nBump(n);\n\
                    var u = Console.ReadLine();\nTake(u);\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0205", "MR0205"]);
        assert!(found.iter().all(|d| d.fixes.is_empty()), "{found:#?}");
    }

    const POINT: &str = "struct Point {\n    public X: int;\n    public Y: int;\n    \
                         Scaled(factor: int): Point => new Point { X = X * factor, Y = Y * factor };\n}\n";

    #[test]
    fn new_struct_has_the_struct_type() {
        let text = format!(
            "{POINT}let p = new Point {{ X = 3, Y = 4 }};\nlet x = p.X;\nlet q = p.Scaled(2);\n"
        );
        assert_eq!(local_types(&text), ["p: Point", "x: int", "q: Point"]);
        assert_eq!(reported(&text), Vec::<String>::new());
    }

    #[test]
    fn initializer_converts_to_field_type() {
        let text = "struct S {\n    public L: long;\n}\nlet s = new S { L = 1 };\n";
        assert_eq!(local_types(text), ["s: S"]);
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn initializer_of_the_wrong_type_is_a_mismatch() {
        let text = "struct S {\n    public L: int;\n}\nlet s = new S { L = \"a\" };\n";
        assert_eq!(reported(text), ["MR0203"]);
    }

    #[test]
    fn unknown_initializer_name_is_still_inferred() {
        let text = "struct S {\n    public L: int;\n}\nlet s = new S { L = 1, M = 2 };\n";
        assert_eq!(local_types(text), ["s: S"]);
    }

    /// A struct with an instance field of each kind of initializer, a
    /// getter, a static field and a constant.
    const SHAPE: &str = "struct Shape {\n    public Width: int;\n    public Height: int = 1;\n    \
                         public Area: int => Width * Height;\n    public static Count: int = 0;\n    \
                         public const Sides: int = 4;\n}\n";

    /// The diagnostics of `new Shape { inits }` after [`SHAPE`].
    fn shape_new(inits: &str) -> (String, Vec<mora_diag::Diagnostic>) {
        let text = format!("{SHAPE}let s = new Shape {{ {inits} }};\n");
        let found = diagnostics_of(&text);
        (text, found)
    }

    #[test]
    fn a_name_that_is_not_an_instance_field_is_mr0147() {
        for (inits, name, message, label) in [
            (
                "Width = 1, Depth = 2",
                "Depth",
                "`Shape` has no field named `Depth`",
                "not a field of `Shape`",
            ),
            (
                "Width = 1, Area = 2",
                "Area",
                "`Area` is a getter of `Shape` and cannot be assigned",
                "a getter computes its value",
            ),
            (
                "Width = 1, Count = 2",
                "Count",
                "`Count` is a static field of `Shape`, not a field of its values",
                "belongs to the type",
            ),
            (
                "Width = 1, Sides = 2",
                "Sides",
                "`Sides` is a constant of `Shape`, not a field of its values",
                "belongs to the type",
            ),
        ] {
            let (text, found) = shape_new(inits);
            assert_eq!(codes(&found), ["MR0147"], "{inits}");
            let d = &found[0];
            assert_eq!(d.message, message);
            assert_eq!(d.primary.message.as_deref(), Some(label));
            assert_eq!(&text[d.primary.span.range], name);
            assert_eq!(
                d.note.as_deref(),
                Some("memberwise creation of `Shape` assigns the fields that each value holds")
            );
            assert!(d.fixes.is_empty(), "{inits}: {d:#?}");
        }
        // The value is still checked against nothing and inferred.
        let text = format!("{SHAPE}let s = new Shape {{ Width = 1, Depth = \"a\" }};\n");
        assert_eq!(reported(&text), ["MR0147"]);
    }

    #[test]
    fn a_misspelled_field_suggests_an_unassigned_field() {
        let (text, found) = shape_new("Width = 1, Heigth = 2");
        assert_eq!(codes(&found), ["MR0147"]);
        assert_eq!(found[0].fixes.len(), 1);
        assert_eq!(found[0].fixes[0].message, "did you mean `Height`?");
        let fixed = apply_fixes(&text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert!(fixed.text.contains("new Shape { Width = 1, Height = 2 }"));
        // A field already assigned is not suggested.
        let (_, found) = shape_new("Widht = 1, Width = 2");
        assert_eq!(codes(&found), ["MR0147"]);
        assert!(found[0].fixes.is_empty(), "{found:#?}");
    }

    #[test]
    fn a_field_assigned_twice_is_mr0148() {
        let (text, found) = shape_new("Height = 1, Width = 2, Height = 3");
        assert_eq!(codes(&found), ["MR0148"]);
        let d = &found[0];
        assert_eq!(
            d.message,
            "memberwise creation of `Shape` assigns `Height` twice"
        );
        assert_eq!(d.primary.message.as_deref(), Some("assigned again here"));
        let at = text.rfind("Height").expect("the second name");
        assert_eq!(usize::from(d.primary.span.range.start()), at);
        assert_eq!(d.secondary.len(), 1);
        assert_eq!(
            d.secondary[0].message.as_deref(),
            Some("first assigned here")
        );
        assert_eq!(&text[d.secondary[0].span.range], "Height");
        assert!(d.secondary[0].span.range.start() < d.primary.span.range.start());
        assert_eq!(d.fixes.len(), 1);
        assert_eq!(d.fixes[0].message, "remove the repeated assignment");
        let fixed = apply_fixes(&text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert!(fixed.text.contains("new Shape { Height = 1, Width = 2 }"));
        // Each repetition is reported, and its value still checked.
        let (_, found) = shape_new("Width = 1, Width = 2, Width = \"a\"");
        assert_eq!(codes(&found), ["MR0148", "MR0148", "MR0203"]);
    }

    #[test]
    fn names_after_a_constructor_call_are_visible_instance_fields() {
        // The assignments are plain ones: a field may be assigned twice,
        // but a name must be an instance field the body sees.
        let text = "struct S {\n    public mut N: int;\n    public S(n: int) { N = n; }\n}\n\
                    let s = new S(1) { N = 1, N = 2, M = 3 };\nlet t = new S(1) { Nn = 3 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0147", "MR0147"]);
        let d = &found[0];
        assert_eq!(d.message, "`S` has no field named `M`");
        assert_eq!(
            d.note.as_deref(),
            Some(
                "the assignments after a constructor call name the fields that each value of `S` holds"
            )
        );
        assert_eq!(found[1].fixes.len(), 1);
        assert_eq!(found[1].fixes[0].message, "did you mean `N`?");
        let text = "struct S {\n    public mut N: int;\n    private mut H: int;\n    \
                    public S(n: int) { N = n; H = n; }\n}\n\
                    let s = new S(1) { H = 2, N = \"a\" };\n";
        assert_eq!(reported(text), ["MR0129", "MR0203"]);
    }

    #[test]
    fn a_hidden_field_assigned_twice_after_a_constructor_is_fixed_once() {
        // Both assignments are reported, and only the first carries the
        // fix, so applying every fix widens the field once.
        let text = "struct S {\n    private mut H: int;\n    public S(h: int) { H = h; }\n}\n\
                    let s = new S(1) { H = 1, H = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0129", "MR0129"]);
        assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
        assert!(found[1].fixes.is_empty(), "{found:#?}");
        let fixed = apply_fixes(text);
        assert!(
            fixed.text.contains("    internal mut H: int;\n"),
            "{}",
            fixed.text
        );
        assert_eq!(reported(&fixed.text), Vec::<String>::new());
    }

    #[test]
    fn after_a_constructor_an_assigned_field_is_still_suggested() {
        // A field may be assigned again after a constructor, so one already
        // assigned is still the closest field.
        let text = "struct P {\n    public mut X: int;\n    public P(x: int) { X = x; }\n}\n\
                    let p = new P(1) { X = 1, Xx = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0147"]);
        assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
        assert_eq!(found[0].fixes[0].message, "did you mean `X`?");
    }

    #[test]
    fn after_a_constructor_only_a_writable_field_is_suggested() {
        // The assignments after a constructor call may write only a field
        // declared `mut` without a write scope, so a read-only field, or one
        // with a write scope, is no fix for a misspelled name.
        for (field, suggested) in [
            ("public N: int;", false),
            ("public mut(private) N: int;", false),
            ("public mut N: int;", true),
        ] {
            let head = format!("struct S {{\n    {field}\n    public S() {{ N = 1; }}\n}}\n");
            let text = format!("{head}let s = new S() {{ Nn = 2 }};\n");
            let found = diagnostics_of(&text);
            assert_eq!(codes(&found), ["MR0147"], "{text}");
            if suggested {
                assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
                assert_eq!(found[0].fixes[0].message, "did you mean `N`?");
            } else {
                assert!(found[0].fixes.is_empty(), "{found:#?}");
            }
            // The names in braces alone are checked as after the call.
            let text = format!("{head}let s = new S {{ Nn = 2 }};\n");
            let found = diagnostics_of(&text);
            assert_eq!(codes(&found), ["MR0153", "MR0147"], "{text}");
            assert_eq!(found[1].fixes.len(), usize::from(suggested), "{found:#?}");
        }
        // Memberwise creation sets a read-only field, so it suggests one.
        let text = "struct P {\n    public N: int;\n}\nlet p = new P { Nn = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0143", "MR0147"]);
        assert_eq!(found[1].fixes.len(), 1, "{found:#?}");
    }

    #[test]
    fn a_hidden_field_not_writable_after_a_constructor_has_no_fix() {
        // Widening a field the assignment may not write still leaves an
        // invalid program, so there is no fix.
        for field in ["private H: int;", "private mut(private) H: int;"] {
            let text = format!(
                "struct S {{\n    {field}\n    public S(h: int) {{ H = h; }}\n}}\n\
                 let s = new S(1) {{ H = 2 }};\n"
            );
            let found = diagnostics_of(&text);
            assert_eq!(codes(&found), ["MR0129"], "{text}");
            assert!(found[0].fixes.is_empty(), "{found:#?}");
        }
        // Under `MR0153` the braces stay an error whatever the field's
        // visibility, so widening it is no fix either.
        let text = "struct S {\n    private mut H: int;\n    public S() { H = 1; }\n}\n\
                    let s = new S { H = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153", "MR0129"]);
        assert!(found.iter().all(|d| d.fixes.is_empty()), "{found:#?}");
    }

    #[test]
    fn names_after_a_positional_constructor_are_not_checked_yet() {
        // Its positional fields are not instance fields until M2.2.
        let text = "struct U(Id: int);\nlet u = new U(1) { Id = 2 };\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn missing_field_without_initializer_is_mr0143() {
        let text = format!("{POINT}let p = new Point {{ X = 3 }};\n");
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", &text)]);
        let found = diagnostics(&db, project);
        assert_eq!(codes(found), ["MR0143"]);
        assert_eq!(
            found[0].message,
            "memberwise creation of `Point` does not assign `Y`"
        );
        assert_eq!(&text[found[0].primary.span.range], "new Point { X = 3 }");
        // The first missing field in declaration order is named.
        let text = format!("{POINT}let p = new Point {{}};\n");
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", &text)]);
        let found = diagnostics(&db, project);
        assert_eq!(codes(found), ["MR0143"]);
        assert_eq!(
            found[0].message,
            "memberwise creation of `Point` does not assign `X`"
        );
    }

    #[test]
    fn omitted_field_with_initializer_is_accepted() {
        let text = "struct S {\n    public N: int = 5;\n    public static C: int;\n    \
                    public const K: int = 1;\n    public M: int;\n}\n\
                    let a = new S { M = 1 };\nlet b = new S { N = 2, M = 1 };\n";
        assert_eq!(local_types(text), ["a: S", "b: S"]);
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn a_constructor_call_assigns_the_fields_itself() {
        let text = "struct S {\n    public mut N: int;\n    public S(n: int) { N = n; }\n}\n\
                    let s = new S(1);\n";
        assert_eq!(local_types(text), ["s: S"]);
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn arguments_to_a_struct_without_a_constructor_are_mr0205() {
        let text = "struct S {\n    public N: int;\n}\nlet s = new S(1) { N = \"a\" };\n";
        assert_eq!(local_types(text), ["s: S"]);
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0205", "MR0203"]);
        let d = &found[0];
        assert_eq!(
            d.message,
            "`S` declares no constructor, so `new S(...)` takes no arguments"
        );
        assert_eq!(&text[d.primary.span.range], "new S(1) { N = \"a\" }");
    }

    #[test]
    fn new_with_empty_parentheses_on_a_struct_without_a_constructor_is_memberwise() {
        // `new T()` names no fields (§6.9), so a required field is missing.
        let text = "struct S {\n    public N: int;\n}\nlet s = new S();\n";
        assert_eq!(local_types(text), ["s: S"]);
        assert_eq!(reported(text), ["MR0143"]);
        let text = "struct S {\n    public N: int;\n}\nlet s = new S() { N = 1 };\n";
        assert_eq!(reported(text), Vec::<String>::new());
        let text = "struct S {\n    public N: int;\n}\nlet s = new S() { N = 1, N = 2, M = 3 };\n";
        assert_eq!(reported(text), ["MR0148", "MR0147"]);
    }

    #[test]
    fn new_with_empty_parentheses_calls_a_declared_constructor() {
        let text = "struct S {\n    public mut N: int;\n    public S() { N = 1; }\n}\n\
                    let s = new S() { N = 1, N = 2, M = 3 };\nlet t = new S();\n";
        assert_eq!(local_types(text), ["s: S", "t: S"]);
        // No field is missing, and `N` may be assigned again; `M` is no field.
        assert_eq!(reported(text), ["MR0147"]);
    }

    #[test]
    fn braces_alone_on_a_struct_with_a_constructor_are_mr0153() {
        let text = "struct S {\n    public mut N: int;\n    public S() { N = 1; }\n}\n\
                    let s = new S { N = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153"]);
        let d = &found[0];
        assert_eq!(
            d.message,
            "`S` declares a constructor, so it is not created memberwise"
        );
        assert_eq!(&text[d.primary.span.range], "new S { N = 2 }");
        assert_eq!(
            d.primary.message.as_deref(),
            Some("memberwise creation of `S`")
        );
        assert_eq!(d.fixes.len(), 1);
        assert_eq!(
            d.fixes[0].message,
            "call the constructor that takes no arguments"
        );
        let fixed = apply_fixes(text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert!(fixed.text.contains("new S() { N = 2 }"), "{}", fixed.text);
        // No field is reported missing, and the values and names are still
        // checked.
        let text = "struct S {\n    public N: int;\n    public S(n: int) { N = n; }\n}\n\
                    struct T {\n    public N: int;\n    public T() { N = 1; }\n}\n\
                    let s = new S { };\nlet t = new S { N = \"a\", M = 1 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153", "MR0153", "MR0203", "MR0147"]);
        // Without a constructor that takes no arguments there is no fix,
        // whatever the constructors of other types take.
        assert!(found[0].fixes.is_empty(), "{found:#?}");
    }

    #[test]
    fn braces_alone_on_a_struct_with_a_constructor_still_check_the_names() {
        // The names are checked as after the constructor that the fix
        // calls, and a name that is no field leaves no fix to offer.
        let text = "struct S {\n    public mut N: int;\n    private mut H: int;\n    \
                    public S() { N = 1; H = 1; }\n}\n\
                    let s = new S { M = 1 };\nlet t = new S { N = 1, H = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153", "MR0147", "MR0153", "MR0129"]);
        assert!(found[0].fixes.is_empty(), "{found:#?}");
        assert!(found[2].fixes.is_empty(), "{found:#?}");
        // A positional struct's names are not checked until M2.2.
        let text = "struct U(Id: int);\nlet u = new U { Id = 2 };\n";
        assert_eq!(reported(text), ["MR0153"]);
    }

    #[test]
    fn the_mr0153_fix_calls_a_visible_constructor_callable_without_arguments() {
        // Parameters that all have defaults take no arguments.
        let text = "struct S {\n    public mut N: int;\n    public S(n: int = 1, m: int = 2) { N = n + m; }\n}\n\
                    let s = new S { N = 2 };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153"]);
        assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
        let fixed = apply_fixes(text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert!(fixed.text.contains("new S() { N = 2 }"), "{}", fixed.text);
        // One parameter without a default needs an argument.
        let text = "struct S {\n    public mut N: int;\n    public S(n: int, m: int = 2) { N = n + m; }\n}\n\
                    let s = new S { N = 2 };\n";
        assert!(diagnostics_of(text)[0].fixes.is_empty());
        // A private constructor is called only inside its type.
        let head = "struct S {\n    public mut N: int;\n    private S() { N = 1; }\n";
        let text = format!("{head}}}\nlet s = new S {{ N = 2 }};\n");
        let found = diagnostics_of(&text);
        assert_eq!(codes(&found), ["MR0153"]);
        assert!(found[0].fixes.is_empty(), "{found:#?}");
        let text = format!("{head}    static Make(): S => new S {{ N = 2 }};\n}}\n");
        let found = diagnostics_of(&text);
        assert_eq!(codes(&found), ["MR0153"]);
        assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
    }

    #[test]
    fn braces_alone_naming_a_field_not_writable_after_a_constructor_have_no_fix() {
        // A read-only field, or one whose write scope may not allow the
        // write, cannot be assigned after the constructor that `()` calls.
        // A `mut` written twice is `MR0111`, and a write scope on either
        // still keeps the fix away, whatever their order.
        for (field, repeated) in [
            ("public N: int;", false),
            ("public mut(private) N: int;", false),
            ("public mut(private) mut N: int;", true),
            ("public mut mut(private) N: int;", true),
        ] {
            let text = format!(
                "struct S {{\n    {field}\n    public S() {{ N = 1; }}\n}}\n\
                 let s = new S {{ N = 2 }};\n"
            );
            let mut found = diagnostics_of(&text);
            if repeated {
                assert_eq!(codes(&found), ["MR0111", "MR0153"], "{text}");
                found.remove(0);
            }
            assert_eq!(codes(&found), ["MR0153"], "{text}");
            assert!(found[0].fixes.is_empty(), "{found:#?}");
        }
    }

    #[test]
    fn braces_alone_on_a_positional_struct_are_mr0153() {
        let text = "struct U(Id: int);\nlet u = new U { };\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0153"]);
        let d = &found[0];
        assert_eq!(
            d.message,
            "`U` is a positional struct, so it is not created memberwise"
        );
        assert_eq!(
            d.note.as_deref(),
            Some("create it from its positional parameters: `new U(...)`")
        );
        assert!(d.fixes.is_empty(), "{d:#?}");
    }

    #[test]
    fn initializers_are_inferred_when_the_name_is_unknown() {
        let text = "struct S {\n    public L: int;\n}\nlet s = new S { L = 1, M = 1 + \"a\" };\n";
        assert_eq!(reported(text), ["MR0147", "MR0204"]);
    }

    #[test]
    fn initializers_are_inferred_when_the_target_is_not_a_struct() {
        let text = "class C {\n    public N: int;\n}\nlet c = new C { N = 1 + \"a\" };\n";
        assert_eq!(reported(text), ["MR0204"]);
    }

    #[test]
    fn a_static_member_has_no_this() {
        let head = "struct S {\n    public X: int;\n    public Y: int => X;\n    \
                    Get(): int => X;\n";
        for member in [
            "public static Run(): int => X;",
            "public static Run(): int => this.X;",
            "public static Run(): int { return X; }",
            "static Run(): int { G(): int => X; return G(); }",
            "static Run(): int { var f = () => X; return f(); }",
            "static Run(): int => Y;",
            "static Run(): int => Get();",
            "static Run(): int => this.Get();",
            "static Run(): int => X.CompareTo(1);",
            "static T: int = X;",
        ] {
            let text = format!("{head}    {member}\n}}\n");
            assert_eq!(reported(&text), ["MR0149"], "{text}");
        }
        // A static member reads what belongs to the type, and an instance
        // member reads its own fields.
        let text = "struct S {\n    public X: int;\n    static N: int = 1;\n    \
                    static Twice(k: int): int => k * 2;\n    \
                    static Run(): int { G(): int => N + Twice(N); return G(); }\n    \
                    Get(): int => X + N + Twice(X);\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn operators_and_static_implementations_have_no_this() {
        let head = "interface IMake {\n    static abstract Make(): int;\n}\n\
                    interface IUnit {\n    static abstract Unit: int;\n}\n\
                    struct S : IMake, IUnit {\n    public X: int;\n";
        for member in [
            "public static operator -(a: S): S => this;",
            "public static operator +(a: S, b: S): S { return this; }",
            "static IMake.Make(): int => X;",
            "static IUnit.Unit: int => X;",
        ] {
            let text = format!("{head}    {member}\n}}\n");
            assert_eq!(reported(&text), ["MR0149"], "{text}");
        }
    }

    #[test]
    fn a_static_member_cannot_name_an_instance_method_group() {
        let head = "struct S {\n    public X: int;\n    Get(): int => X;\n    \
                    static Take(f: () => int): int => 1;\n";
        for member in [
            "static Run() { var f = Get; }",
            "static Run(): int => Take(Get);",
            "static Run(): (() => int) => Get;",
            "static Run() { var f = () => Get; }",
        ] {
            let text = format!("{head}    {member}\n}}\n");
            assert_eq!(reported(&text), ["MR0149"], "{text}");
        }
        // Every overload has a receiver.
        let text =
            format!("{head}    Get(k: int): int => k;\n    static Run() {{ var f = Get; }}\n}}\n");
        assert_eq!(reported(&text), ["MR0149"], "{text}");
        // A static method group, and overloads of which one is static: no
        // pick is recorded for a method group, so it is reported only when
        // every overload needs a value.
        for member in [
            "static Twice(): int => 2;\n    static Run(): int => Take(Twice);",
            "static Get(k: int): int => k;\n    static Run() { var f = Get; }",
        ] {
            let text = format!("{head}    {member}\n}}\n");
            assert_eq!(reported(&text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn an_initializer_cannot_name_an_instance_method_group() {
        let head = "struct S {\n    public X: int;\n    Get(): int => X;\n    \
                    static Take(f: () => int): int => 1;\n    static Twice(): int => 2;\n";
        for field in ["public Z: int = Take(Get);", "public F: (() => int) = Get;"] {
            let text = format!("{head}    {field}\n}}\n");
            assert_eq!(reported(&text), ["MR0152"], "{text}");
        }
        let text = format!("{head}    public Z: int = Take(Twice);\n}}\n");
        assert_eq!(reported(&text), Vec::<String>::new(), "{text}");
    }

    #[test]
    fn a_nested_type_has_no_value_of_the_type_around_it() {
        let head = "struct S {\n    public mut X: int;\n    public Y: int => X;\n    \
                    Get(): int => X;\n    static N: int = 1;\n    \
                    struct In {\n        public Z: int;\n";
        for member in [
            "R(): int => X;",
            "R(): int => Y;",
            "R(): int => Get();",
            "R() { var f = Get; }",
            "mut R() { X = 1; }",
            "public W: int => X;",
            "public W: int = X;",
            "public F: (() => int) = Get;",
            "R(): int { G(): int => X; return G(); }",
            "R(): int { var f = () => X; return f(); }",
            "static R(): int => X;",
            "static R(): int => Get();",
        ] {
            let text = format!("{head}        {member}\n    }}\n}}\n");
            assert_eq!(reported(&text), ["MR0149"], "{text}");
        }
        // The nested type's own members and `this`, the static members
        // around it, and locals and parameters that take a field's name.
        for member in [
            "R(): int => Z + this.Z + N;",
            "R(X: int): int => X;",
            "R(): int { var X = 1; return X; }",
            "R(): int => [1].Count((X) => X > 0);",
        ] {
            let text = format!("{head}        {member}\n    }}\n}}\n");
            assert_eq!(reported(&text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn a_nested_type_that_uses_the_type_around_it_says_what_it_uses() {
        let head = "struct Outer {\n    public X: int;\n    Get(): int => X;\n    \
                    struct Inner {\n";
        let [field, method, from_static] = [
            ("R(): int => X;", "X"),
            ("R(): int => Get();", "Get"),
            ("static R(): int => X;", "X"),
        ]
        .map(|(member, used)| {
            let d = instance_use(&format!("{head}        {member}\n    }}\n}}\n"), used);
            assert_eq!(
                d.note.as_deref(),
                Some(
                    "a nested type is a type of its own: its members have no value of the \
                     type it is nested in"
                )
            );
            (d.message, d.primary.message.expect("label"))
        });
        assert_eq!(
            field,
            (
                "a member of `Inner` has no value of `Outer`, so it cannot use the instance \
                 field `X`"
                    .to_owned(),
                "needs a value of `Outer`".to_owned()
            )
        );
        assert_eq!(
            method.0,
            "a member of `Inner` has no value of `Outer`, so it cannot use the instance method `Get`"
        );
        assert_eq!(from_static, field);
    }

    #[test]
    fn a_static_member_that_uses_this_says_what_it_uses() {
        let head = "struct S {\n    public X: int;\n    public Y: int => X;\n    \
                    Get(): int => X;\n";
        let [field, getter, method, this] = [
            ("static Run(): int => X;", "X"),
            ("static Run(): int => Y;", "Y"),
            ("static Run(): int => Get();", "Get"),
            ("static Run(): S => this;", "this"),
        ]
        .map(|(member, used)| {
            let d = instance_use(&format!("{head}    {member}\n}}\n"), used);
            assert_eq!(
                d.note.as_deref(),
                Some("a `static` member belongs to the type, not to a value of it")
            );
            (d.message, d.primary.message.expect("label"))
        });
        assert_eq!(
            field,
            (
                "a static member has no `this`, so it cannot use the instance field `X`".to_owned(),
                "needs a value of the type".to_owned()
            )
        );
        assert_eq!(
            getter.0,
            "a static member has no `this`, so it cannot use the instance getter `Y`"
        );
        assert_eq!(
            method.0,
            "a static member has no `this`, so it cannot use the instance method `Get`"
        );
        assert_eq!(this.0, "a static member has no `this`");
        assert_eq!(this.1, "there is no value of the type here");
    }

    #[test]
    fn an_initializer_has_no_this() {
        let head = "struct S {\n    public X: int;\n    public Y: int => X;\n    \
                    Get(): int => X;\n    const K: int = 4;\n    static N: int = 5;\n    \
                    static Twice(k: int): int => k * 2;\n";
        for field in [
            "public Z: int = X;",
            "public Z: int = Y;",
            "public Z: int = this.X;",
            "public Z: int = Get();",
            "public Z: int = this.Get();",
            "public Z: int = X + 1;",
            "public Z: int = (() => X)();",
            "public Z: int = (() => { G(): int => X; return G(); })();",
            "public Z: int = (() => { var f = () => this.X; return f(); })();",
        ] {
            let text = format!("{head}    {field}\n}}\n");
            assert_eq!(reported(&text), ["MR0152"], "{text}");
        }
        // Constants and static members belong to the type, and a field that
        // is not initialized reads nothing.
        let text = format!(
            "{head}    public Z: int = K + N + Twice(K);\n    \
             public W: int = (() => {{ G(): int => N; return G(); }})();\n    public V: int;\n}}\n"
        );
        assert_eq!(reported(&text), Vec::<String>::new(), "{text}");
    }

    #[test]
    fn an_initializer_that_uses_this_says_what_it_uses() {
        let head = "struct S {\n    public X: int;\n    public Y: int => X;\n    \
                    Get(): int => X;\n";
        let [field, getter, method, this] = [
            ("public Z: int = X;", "X"),
            ("public Z: int = Y;", "Y"),
            ("public Z: int = Get();", "Get"),
            ("public Z: S = this;", "this"),
        ]
        .map(|(member, used)| {
            let d = instance_use(&format!("{head}    {member}\n}}\n"), used);
            assert_eq!(
                d.note.as_deref(),
                Some(
                    "an initializer runs for each field that `new T { ... }` leaves out, with \
                     no `this`; it may use constants and `static` members"
                )
            );
            (d.message, d.primary.message.expect("label"))
        });
        assert_eq!(
            field,
            (
                "the initializer of `Z` cannot use the instance member `X`: \
                 initializers run before the value exists"
                    .to_owned(),
                "the value does not exist yet".to_owned()
            )
        );
        assert!(getter.0.contains("the instance member `Y`"), "{getter:?}");
        assert!(method.0.contains("the instance member `Get`"), "{method:?}");
        assert_eq!(
            this,
            (
                "the initializer of `Z` cannot use `this`: initializers run before the value exists"
                    .to_owned(),
                "there is no value of the type yet".to_owned()
            )
        );
    }

    #[test]
    fn target_typed_new_of_a_struct_follows_the_same_rules() {
        // `new() { .. }` does what `new T() { .. }` does where the target
        // type is known (§6.9).
        let s = "struct S {\n    public mut N: int;\n    public S() { N = 1; }\n}\n";
        let p = "struct P {\n    public X: int;\n}\n";
        let text = format!("{s}let s: S = new {{ N = 2 }};\n");
        let found = diagnostics_of(&text);
        assert_eq!(codes(&found), ["MR0153"]);
        assert_eq!(&text[found[0].primary.span.range], "new { N = 2 }");
        assert_eq!(found[0].fixes.len(), 1, "{found:#?}");
        let fixed = apply_fixes(&text);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert!(
            fixed.text.contains("let s: S = new() { N = 2 };"),
            "{}",
            fixed.text
        );
        let text = "struct U(Id: int);\nlet u: U = new { };\n";
        assert_eq!(reported(text), ["MR0153"]);
        let text = format!("{s}let s: S = new() {{ N = 1, M = 2 }};\n");
        assert_eq!(reported(&text), ["MR0147"]);
        let text = format!("{p}let p: P = new {{ }};\nlet q: P = new() {{ X = 1, X = 2 }};\n");
        assert_eq!(reported(&text), ["MR0143", "MR0148"]);
        let text = format!("{p}let p: P = new(1) {{ X = 1 }};\n");
        assert_eq!(reported(&text), ["MR0205"]);
        // An argument takes the type of its parameter when there is one
        // function to call.
        let text =
            format!("{p}let p: P = new {{ X = 1 }};\nTake(p: P) {{}}\nTake(new {{ X = 2 }});\n");
        assert_eq!(reported(&text), Vec::<String>::new());
        let types = expr_types(&text);
        assert!(types.iter().any(|ty| ty == "new { X = 1 }: P"), "{types:?}");
        assert!(types.iter().any(|ty| ty == "new { X = 2 }: P"), "{types:?}");
        let text = format!("{p}Take(p: P) {{}}\nTake(new {{ X = \"a\" }});\nTake(new {{ }});\n");
        assert_eq!(reported(&text), ["MR0203", "MR0143"]);
    }

    #[test]
    fn target_typed_new_takes_the_type_that_every_overload_agrees_on() {
        let p = "struct P {\n    public X: int;\n}\nstruct Q {\n    public Y: int;\n}\n";
        // Each overload the argument count allows has `P` there; the one
        // with three parameters does not count.
        let text = format!(
            "{p}Take(p: P, n: int) {{}}\nTake(p: P, s: string) {{}}\n\
             Take(q: Q, n: int, m: int) {{}}\nTake(new {{ X = 1 }}, 2);\n\
             Take(s: \"a\", p: new {{ X = 3 }});\n"
        );
        assert_eq!(reported(&text), Vec::<String>::new());
        let types = expr_types(&text);
        assert!(types.iter().any(|ty| ty == "new { X = 1 }: P"), "{types:?}");
        assert!(types.iter().any(|ty| ty == "new { X = 3 }: P"), "{types:?}");
        // The fields are checked against the type.
        let text = format!(
            "{p}Take(p: P, n: int) {{}}\nTake(p: P, s: string) {{}}\nTake(new {{ Y = 1 }}, 2);\n"
        );
        assert_eq!(reported(&text), ["MR0143", "MR0147"]);
        // Overloads that differ there give no target type.
        let text = format!("{p}Take(p: P) {{}}\nTake(q: Q) {{}}\nTake(new {{ X = 1 }});\n");
        let found = diagnostics_of(&text);
        assert_eq!(codes(&found), ["MR0154"]);
        assert_eq!(&text[found[0].primary.span.range], "new { X = 1 }");
        assert_eq!(
            found[0].primary.message.as_deref(),
            Some("the overloads of `Take` take different types here")
        );
        // The other arguments, typed without a target, rule out the
        // overloads they cannot go to first.
        let pick = format!("{p}Pick(p: P, n: int) {{}}\nPick(q: Q, s: string) {{}}\n");
        let text = format!("{pick}Pick(new {{ X = 1 }}, 1);\nPick(new {{ Y = 2 }}, \"a\");\n");
        assert_eq!(reported(&text), Vec::<String>::new());
        let types = expr_types(&text);
        assert!(types.iter().any(|ty| ty == "new { X = 1 }: P"), "{types:?}");
        assert!(types.iter().any(|ty| ty == "new { Y = 2 }: Q"), "{types:?}");
        let text = format!("{pick}Pick(new {{ X = 1 }}, true);\n");
        assert_eq!(reported(&text), ["MR0205"]);
        // A call that is an error already says so alone.
        let text = format!("{p}Take(p: P) {{}}\nTake(q: Q) {{}}\nTake(new {{ X = 1 }}, 2);\n");
        assert_eq!(reported(&text), ["MR0205"]);
        // A lambda takes the function type they agree on, too, so its
        // body is checked against the result type.
        let run = "Run(f: () => int, n: int): int => 1;\nRun(f: () => int, s: string): int => 2;\n";
        assert_eq!(
            reported(&format!("{run}let r = Run(() => 3, 2);\n")),
            Vec::<String>::new()
        );
        assert_eq!(
            reported(&format!("{run}let r = Run(() => \"a\", 2);\n")),
            ["MR0203"]
        );
    }

    #[test]
    fn new_of_a_class_or_a_generic_struct_has_its_type() {
        // Classes and generic code come later, so nothing more is checked.
        let text = "class C {\n    public N: int;\n}\nstruct B<T> {\n    public V: T;\n}\n\
                    let c = new C { N = 1 };\nlet d = new C();\nlet e: C = new();\n\
                    let f: C = new { N = 2 };\nlet g = new B<int> { V = 1 };\n\
                    let h: B<int> = new { V = 1 };\n";
        assert_eq!(reported(text), Vec::<String>::new());
        assert_eq!(
            local_types(text),
            ["c: _", "d: _", "e: _", "f: _", "g: _", "h: _"]
        );
    }

    #[test]
    fn target_typed_new_needs_a_type_it_can_create() {
        let text = "F(): int => new { X = 1 };\nlet a: int = new();\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0154", "MR0154"]);
        assert_eq!(&text[found[0].primary.span.range], "new { X = 1 }");
        assert_eq!(
            found[0].message,
            "`new` cannot create the type expected here"
        );
        assert_eq!(&text[found[1].primary.span.range], "new()");
        // Each target that is no struct and no class.
        let types = "union U {\n    A,\n    B;\n}\nenum E { X }\ninterface I {}\n";
        for target in [
            "string",
            "(int, int)",
            "int?",
            "int[]",
            "U",
            "E",
            "I",
            "() => int",
        ] {
            let text = format!("{types}let x: {target} = new();\n");
            assert_eq!(reported(&text), ["MR0154"], "{target}");
        }
        let text = "Id<T>(x: T): T {\n    let t: T = new();\n    return x;\n}\n";
        assert_eq!(reported(text), ["MR0154"]);
        // The values inside are still checked.
        let text = "let a: bool = new { X = Missing };\n";
        assert_eq!(reported(text), ["MR0154", "MR0127"]);
        // A target that is an error was reported already.
        let text = "let a: Missing = new();\n";
        assert_eq!(reported(text), ["MR0127"]);
    }

    #[test]
    fn new_without_a_type_or_a_target_is_mr0154() {
        let text = "var x = new { X = 1 };\nlet y = new();\nnew();\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0154", "MR0154", "MR0154"]);
        assert_eq!(&text[found[0].primary.span.range], "new { X = 1 }");
        assert_eq!(
            found[0].message,
            "`new` without a type needs a target type to create"
        );
        assert_eq!(local_types(text), ["x: ?", "y: ?"]);
    }

    #[test]
    fn new_without_a_target_is_mr0154_wherever_it_is() {
        let p = "struct P {\n    public X: int;\n}\n";
        // A receiver, and an argument of a function of the standard
        // library, written as a path or through a member of one.
        for text in [
            "let a = new { X = 1 }.X;\n",
            "Console.WriteLine(new { X = 1 });\n",
            "Console.Out.WriteLine(new { X = 1 });\n",
        ] {
            let text = format!("{p}{text}");
            let found = diagnostics_of(&text);
            assert_eq!(codes(&found), ["MR0154"], "{text}");
            assert_eq!(&text[found[0].primary.span.range], "new { X = 1 }");
        }
        // An argument of a call checked later is checked with that call,
        // and what is inside it now.
        let later = "class C {\n    public Take(p: P) {}\n}\nMake<T>(p: P, t: T) {}\n";
        for (text, expected) in [
            ("F(c: C) {\n    c.Take(new { X = 1 });\n}\n", vec![]),
            ("F() {\n    Make(new { X = 1 }, 1);\n}\n", vec![]),
            ("F() {\n    Make(new { X = 1 }.X, 1);\n}\n", vec!["MR0154"]),
            // The name that is not found is the one mistake.
            ("F() {\n    Missing(new { X = 1 });\n}\n", vec!["MR0127"]),
        ] {
            assert_eq!(reported(&format!("{p}{later}{text}")), expected, "{text}");
        }
    }

    #[test]
    fn a_new_inside_a_value_with_a_target_has_one() {
        let head = "public struct Q {\n    public Y: int;\n}\npublic struct P {\n    public X: int;\n}\n\
                    public struct R {\n    public Q: Q;\n}\n\
                    public class C {\n    public Take(p: P) {}\n    public TakeT(t: (P, int)) {}\n    \
                    public Run(f: () => P) {}\n    public TakeR(r: R) {}\n}\n\
                    Make<T>(t: T, p: P) {}\nG(p: P) {}\nF(xs: P[]) {}\nT(t: (P, int)) {}\n\
                    public struct T2 {\n    public A: P;\n}\n";
        let quiet = [
            "let q: P = match (x) { _ => new { X = 1 } };",
            "let xs: P[] = [new { X = 1 }];",
            "let ls: List<P> = [new { X = 1 }];",
            "F([new { X = 1 }]);",
            "G(match (x) { _ => new { X = 1 } });",
            "G(b ? new { X = 1 } : p);",
            "c.Take(b ? new { X = 1 } : p);",
            "c.TakeT((new { X = 1 }, 1));",
            "T((new { X = 1 }, 1));",
            "Make(1, b ? new { X = 1 } : p);",
            "c.Run(() => b ? new { X = 1 } : p);",
            "c.Run(() => {\n        return b ? new { X = 1 } : p;\n    });",
            "c.Run(() => {\n        return new { X = 1 };\n    });",
            "c.Take(match (x) { _ => new { X = 1 } });",
            "c.TakeR(new { Q = new { Y = 1 } });",
            "Make(new { Q = new { Y = 1 } }, p);",
        ];
        for line in quiet {
            let text = format!("{head}H(x: int, b: bool, p: P, c: C) {{\n    {line}\n}}\n");
            assert_eq!(reported(&text), Vec::<String>::new(), "{line}");
        }
        // Without a target, one missing type is one error.
        for (line, errors) in [
            ("let a = new { Q = new { Y = 1 } };", 1),
            ("let ys = [new { X = 1 }];", 1),
            ("let f = () => {\n    return new { X = 1 };\n};", 1),
            ("panic(new { X = 1 });", 1),
            ("Todo(new());", 1),
            ("let t = b ? (new { X = 1 }, 1) : (new { X = 2 }, 2);", 2),
            // A target the value does not fit gives its parts none, or one
            // that `new` cannot create.
            ("let t: (P, int, int) = (new { X = 1 }, 1);", 1),
            ("let t: int = (new { X = 1 }, 1);", 1),
            ("let t: P = (new { X = 1 }, 1);", 1),
            ("let t: (P, int) = ((new { X = 1 }, 1), 1);", 1),
            ("G((new { X = 1 }, 1));", 1),
            ("T(((new { X = 1 }, 1), 2));", 1),
            ("let t2 = new T2 { A = (new { X = 1 }, 1) };", 1),
            (
                "var t = (new P { X = 0 }, 1);\nt = (new { X = 1 }, 1, 2);",
                1,
            ),
            ("let n: int = match (1) { _ => new { X = 1 } };", 1),
            ("let n: int[] = [new()];", 1),
            ("let f: int = () => new { X = 1 };", 1),
        ] {
            let text = format!("{head}let b = true;\n{line}\n");
            assert_eq!(reported(&text), vec!["MR0154"; errors], "{line}");
        }
    }

    #[test]
    fn a_type_or_a_namespace_is_no_value() {
        let head = "public struct P {\n    public X: int;\n    public static S(): int => 1;\n    \
                    public struct N {\n        public Y: int;\n        \
                    public static NS(): int => 1;\n        public const NK: int = 1;\n    }\n    \
                    Probe(): int {\n        BARE\n        return 0;\n    }\n}\n\
                    public enum Color { Red }\n";
        for (bare, line, expected) in [
            ("let a = N;", "", vec!["MR0127"]),
            ("", "let a = p.N;", vec!["MR0127"]),
            ("", "let a = P.N;", vec!["MR0127"]),
            // A member of a value is no way to write a type, even as a
            // receiver; through the type it is.
            ("", "let a = p.N.NS();", vec!["MR0127"]),
            ("", "let a = p.N.NK;", vec!["MR0127"]),
            ("", "let a = p.N.NS;", vec!["MR0127"]),
            ("", "let a = nameof(p.N);", vec!["MR0127"]),
            ("", "let a = P.N.NS();", vec![]),
            // A type keyword is a type too.
            ("", "let a = int;", vec!["MR0127"]),
            ("", "let a = int.Parse(\"1\");", vec![]),
            ("", "p.N();", vec!["MR0127"]),
            ("", "P.N();", vec!["MR0127"]),
            ("", "let a = P;", vec!["MR0127"]),
            ("", "P();", vec!["MR0127"]),
            ("", "let a = Color;", vec!["MR0127"]),
            ("", "let a = UsersApi;", vec!["MR0127"]),
            // The receiver of a member, `nameof` and a pattern name types.
            ("", "let a = P.S();", vec![]),
            ("", "let a = nameof(P);", vec![]),
            ("", "let a = p is P;", vec![]),
            ("", "let a = new P.N { Y = 1 };", vec![]),
            ("", "let a = Color.Red;", vec![]),
            ("", "let a = p.X;", vec![]),
            // A path that names a type and then something the type does
            // not have is one error, that name not being found.
            ("", "let a = P.X;", vec!["MR0127"]),
        ] {
            let text = format!("{}F(p: P) {{\n    {line}\n}}\n", head.replace("BARE", bare));
            assert_eq!(reported(&text), expected, "{bare}{line}");
        }
        let found = only_diagnostic(&format!("{}let a = P;\n", head.replace("BARE", "")));
        assert_eq!(found.message, "`P` is not a value");
        let notes = format!("{found:?}");
        assert!(notes.contains("`new P { ... }` creates one"), "{notes}");
        let found = only_diagnostic(&format!("{}let a = Color;\n", head.replace("BARE", "")));
        assert!(format!("{found:?}").contains("a type or a namespace names no value"));
        let found = only_diagnostic(&format!(
            "{}F(p: P) {{\n    let a = p.N;\n}}\n",
            head.replace("BARE", "")
        ));
        assert!(format!("{found:?}").contains("`new UsersApi.P.N { ... }`"));
    }

    #[test]
    fn only_a_place_takes_an_assignment() {
        let head = "public struct P {\n    public X: int;\n    public mut M: int;\n    \
                    public G: int => X;\n    public Inst(): int => X;\n    \
                    public static S(): int => 1;\n    public static mut SF: int = 1;\n    \
                    public static SG: int => 1;\n    public const K: int = 2;\n    \
                    public static SQ: Q => new Q { Y = 1 };\n    \
                    public GQ: Q => new Q { Y = 1 };\n    \
                    public static mut SR: Q = new Q { Y = 1 };\n    \
                    Probe() {\n        BARE\n    }\n}\n\
                    public struct Q {\n    public mut Y: int;\n    public static QS(): int => 1;\n}\n\
                    public struct R {\n    public mut Q: Q;\n}\n\
                    public enum Color { Red }\nOne(): int => 1;\nOne2(): Q => new Q { Y = 1 };\n";
        for (bare, line, expected) in [
            ("Inst = 1;", "", vec!["MR0186"]),
            ("G = 1;", "", vec!["MR0186"]),
            ("K = 1;", "", vec!["MR0186"]),
            ("SF = 1;", "", vec![]),
            ("M = 1;", "", vec![]),
            // The path names the getter and then a field of what it gives.
            ("GQ.Y = 1;", "", vec!["MR0186"]),
            ("L() {}\n        L = L;", "", vec!["MR0186"]),
            ("", "q.Inst = 1;", vec!["MR0186"]),
            ("", "q.Inst += 1;", vec!["MR0186"]),
            ("", "q.Inst++;", vec!["MR0186"]),
            ("", "q.G = 1;", vec!["MR0186"]),
            ("", "q.G--;", vec!["MR0186"]),
            ("", "P.S = 1;", vec!["MR0186"]),
            ("", "P.SG = 1;", vec!["MR0186"]),
            ("", "P.K = 1;", vec!["MR0186"]),
            ("", "Color.Red = 1;", vec!["MR0186"]),
            ("", "One() = 1;", vec!["MR0186"]),
            ("", "(1 + 2) = 1;", vec!["MR0186"]),
            ("", "var a = 1;\n    (a, One()) = (1, 2);", vec!["MR0186"]),
            // Places; a static member through a value waits for statics.
            ("", "q.M = 1;", vec![]),
            ("", "q.M += 1;", vec![]),
            ("", "q.M++;", vec![]),
            ("", "P.SF = 1;", vec![]),
            ("", "xs[0] = 1;", vec![]),
            (
                "",
                "var a = 1;\n    var b = 2;\n    (a, b) = (b, a);",
                vec![],
            ),
            ("", "var a = 1;\n    (a, _) = (1, 2);", vec![]),
            ("", "q.SF = 1;", vec![]),
            ("", "q.K = 1;", vec![]),
            ("", "q.Nope = 1;", vec![]),
            // A field of a struct is a place only when the value that
            // holds it is one: not what a getter or a call gives.
            ("", "P.SQ.Y = 1;", vec!["MR0186"]),
            ("", "One2().Y = 1;", vec!["MR0186"]),
            ("", "One2().Y++;", vec!["MR0186"]),
            ("", "One2().Y += 1;", vec!["MR0186"]),
            ("", "new Q { Y = 1 }.Y++;", vec!["MR0186"]),
            // Only a declared instance field is part of the value: any
            // other name keeps the rule of its own milestone.
            ("", "One2().QS = 1;", vec![]),
            ("", "One2().Nope = 1;", vec![]),
            ("", "One2().Nope++;", vec![]),
            ("", "One2().Item1 = 1;", vec![]),
            ("", "P.SQ.Item1 = 1;", vec![]),
            ("", "var b = true;\n    (b ? q : q).Item1 = 1;", vec![]),
            ("", "var b = true;\n    (b ? q : q).M = 1;", vec!["MR0186"]),
            ("", "var r = new Q { Y = 1 };\n    r.Y = 2;", vec![]),
            ("", "P.SR.Y = 1;", vec![]),
            (
                "",
                "xs[0] = 1;\n    var s = new R { Q = new Q { Y = 1 } };\n    s.Q.Y = 2;",
                vec![],
            ),
        ] {
            let text = format!(
                "{}F(p: P, xs: int[]) {{\n    var q = p;\n    {line}\n}}\n",
                head.replace("BARE", bare)
            );
            assert_eq!(reported(&text), expected, "{bare}{line}");
        }
        let found = only_diagnostic(&format!(
            "{}F(p: P) {{\n    var q = p;\n    q.G = 1;\n}}\n",
            head.replace("BARE", "")
        ));
        assert_eq!(found.message, "cannot assign to `q.G`");
        assert!(format!("{found:?}").contains("a getter, which has no setter"));
        let found = only_diagnostic(&format!(
            "{}F() {{\n    One() = 1;\n}}\n",
            head.replace("BARE", "")
        ));
        assert!(format!("{found:?}").contains("the result of a call"));
    }

    #[test]
    fn a_constant_in_a_namespace_is_no_place() {
        let head = "public const C1: int = 1;\npublic static mut G1: int = 1;\n";
        for (line, expected) in [
            ("C1 = 2;", vec!["MR0186"]),
            ("C1++;", vec!["MR0186"]),
            ("C1 += 1;", vec!["MR0186"]),
            ("UsersApi.C1 = 2;", vec!["MR0186"]),
            ("G1 = 2;", vec![]),
            ("G1++;", vec![]),
            ("UsersApi.G1 = 2;", vec![]),
        ] {
            let text = format!("{head}F() {{\n    {line}\n}}\n");
            assert_eq!(reported(&text), expected, "{line}");
        }
    }

    #[test]
    fn a_new_passed_to_a_static_overload_through_a_value_waits_for_its_target() {
        // The call is not checked before statics (D-152), so a `new` passed
        // to it waits for its target; what is inside it is checked now.
        let head = "public struct Q {\n    public Y: int;\n}\npublic struct P {\n    \
                    public X: int;\n    public static S3(q: Q): int => 1;\n    \
                    public S3(x: int): int => X;\n}\n";
        for (line, expected) in [
            ("p.S3(new { Y = 1 });", vec![]),
            ("p.S3(new { Y = Undefined });", vec!["MR0127"]),
        ] {
            let text = format!("{head}F(p: P) {{\n    {line}\n}}\n");
            assert_eq!(reported(&text), expected, "{line}");
        }
    }

    #[test]
    fn a_body_outside_every_type_has_no_this() {
        // A function in a namespace and the top-level statements (§4.2).
        let p = "struct P {\n    public X: int;\n    G(): int => this.X;\n}\n";
        for (text, expected) in [
            ("F() {\n    let a = this;\n}\n", vec!["MR0149"]),
            ("F(): int => this.X;\n", vec!["MR0149"]),
            ("let a = this;\n", vec!["MR0149"]),
            ("F() {\n    L(): int => this.X;\n}\n", vec!["MR0149"]),
            // A constant or a static declared in a namespace.
            ("const C2: int = this.X;\n", vec!["MR0149"]),
            ("static S2: int = this.X;\n", vec!["MR0149"]),
            ("", vec![]),
        ] {
            assert_eq!(reported(&format!("{p}{text}")), expected, "{text}");
        }
    }

    #[test]
    fn a_returned_tuple_that_does_not_fit_reports_its_new() {
        let p = "struct P {\n    public X: int;\n}\n";
        for text in [
            "H(): int => (new { X = 1 }, 1);\n",
            "H(): int {\n    return (new { X = 1 }, 1);\n}\n",
            "H(): P {\n    return (new { X = 1 }, 1);\n}\n",
        ] {
            assert_eq!(reported(&format!("{p}{text}")), ["MR0154"], "{text}");
        }
    }

    #[test]
    fn overloads_leave_a_new_to_an_unresolved_or_wrong_call() {
        let head = "struct P {\n    public X: int;\n}\nstruct Q {\n    public Y: int;\n}\n\
                    class Box {}\n";
        for (decls, call, expected) in [
            // A parameter of a type not known yet: the call is not
            // resolved (D-137).
            (
                "Put(p: P, b: Box) {}\nPut(q: Q, s: string) {}\n",
                "F(b: Box) {\n    Put(new { X = 1 }, b);\n}\n",
                vec![],
            ),
            // An argument that is an error already.
            (
                "Put(p: P, n: int) {}\nPut(q: Q, s: string) {}\n",
                "Put(new { X = 1 }, Missing);\n",
                vec!["MR0127"],
            ),
            // Every type known and different: the `new` is the mistake.
            (
                "Put(p: P, n: int) {}\nPut(q: Q, n: int) {}\n",
                "Put(new { X = 1 }, 1);\n",
                vec!["MR0154"],
            ),
        ] {
            assert_eq!(
                reported(&format!("{head}{decls}{call}")),
                expected,
                "{call}"
            );
        }
        // A lambda the overloads give different types is checked without
        // a target type: the `new` it returns is the error, not the lambda.
        let text = format!("{head}Run(p: P) {{}}\nRun(q: Q) {{}}\nRun(() => new {{ X = 1 }});\n");
        let found = diagnostics_of(&text);
        assert_eq!(codes(&found), ["MR0154"]);
        assert_eq!(&text[found[0].primary.span.range], "new { X = 1 }");
    }

    #[test]
    fn new_of_a_written_type_it_cannot_create_is_mr0154() {
        let text = "let a = new int();\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0154"]);
        assert_eq!(&text[found[0].primary.span.range], "new int()");
        assert_eq!(found[0].message, "`new` cannot create a value of `int`");
        assert_eq!(local_types(text), ["a: ?"]);
        // A type with no item of its own is named by its kind.
        let found = diagnostics_of("let a = new int?();\n");
        assert_eq!(
            found[0].message,
            "`new` cannot create a value of an option type"
        );
        let types = "union U {\n    A,\n    B;\n}\nenum E { X }\ninterface I {}\n";
        for ty in ["string", "int?", "U", "E", "I"] {
            let text = format!("{types}let x = new {ty}();\nlet y = new {ty} {{ }};\n");
            assert_eq!(reported(&text), ["MR0154", "MR0154"], "{ty}");
        }
        let text = "Id<T>(x: T): T {\n    let t = new T();\n    return x;\n}\n";
        assert_eq!(reported(text), ["MR0154"]);
        // The arguments and values inside are still checked, and a type
        // that is an error was reported already.
        let text = "let a = new int(Missing) { X = Other };\nlet b = new Missing();\n";
        assert_eq!(reported(text), ["MR0154", "MR0127", "MR0127", "MR0127"]);
    }

    #[test]
    fn an_assignment_used_as_a_value_is_mr0185() {
        let head = "Show(n: int) {}\nTake(f: () => ()) {}\nTakeInt(f: () => int) {}\nvar x = 0;\nvar y = 0;\n";
        for use_ in [
            "x = y = 5;",
            "Show(x = 3);",
            "let z = (x += 1);",
            "if ((x = 2) > 1) {}",
            "while ((x -= 1) > 0) {}",
            "Show((x = 2) + 1);",
            "let z = x == (y = 1);",
            "return (x = 1);",
            "let f = () => (x = 1);",
            "let t = $\"{x = 1}\";",
            "TakeInt(() => x = 1);",
        ] {
            let text = format!("{head}{use_}\n");
            assert_eq!(reported(&text), ["MR0185"], "{use_}");
        }
        // Each assignment whose value is used is reported.
        assert_eq!(
            reported(&format!("{head}Show(x = y = 1);\n")),
            ["MR0185", "MR0185"]
        );
    }

    #[test]
    fn a_lambda_that_returns_unit_may_assign() {
        let head = "Take(f: () => ()) {}\nTakeInt(f: () => int) {}\nvar x = 0;\n";
        for ok in [
            "Take(() => x = 1);",
            "Take(() => x += 1);",
            "let f: () => () = () => x = 1;",
            "Take(() => x == 1 ? x = 2 : x = 3);",
            "Take(f: () => x = 1);",
            "x == 1 ? x = 2 : x = 3;",
            "x ??= 1;",
        ] {
            assert_eq!(
                reported(&format!("{head}{ok}\n")),
                Vec::<String>::new(),
                "{ok}"
            );
        }
        for bad in ["TakeInt(() => x = 1);", "let f = () => x = 1;"] {
            assert!(
                reported(&format!("{head}{bad}\n")).contains(&"MR0185".to_owned()),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_conditional_of_conditionals_of_assignments_is_a_statement() {
        let head = "Take(f: () => ()) {}\nlet c = true;\nlet d = false;\nvar x = 0;\n";
        for ok in [
            "c ? (d ? x = 1 : x = 2) : (d ? x = 3 : x = 4);",
            "c ? d ? x = 1 : x = 2 : d ? x = 3 : x = 4;",
            "c ? (d ? (c ? x = 1 : x = 2) : x = 3) : x = 4;",
            "c ? (d ? x = 1 : x = 2) : x = 3;",
            "Take(() => c ? (d ? x = 1 : x = 2) : (d ? x = 3 : x = 4));",
            "c ? (match (d) { true => x = 1, false => x = 2 }) : \
             (match (d) { true => x = 3, false => x = 4 });",
            "c ? match (d) { true => x = 1, false => x = 2 } : \
             match (d) { true => x = 3, false => x = 4 };",
            "c ? (match (d) { true => x = 1, false => x = 2 }) : x = 3;",
            "c ? (match (d) { true => x = 1, false => x++ }) : x++;",
            "c ? (match (d) { true => d ? x = 1 : x = 2, false => x = 3 }) : \
             (d ? (match (c) { true => x = 4, false => x = 5 }) : x = 6);",
        ] {
            assert_eq!(
                reported(&format!("{head}{ok}\n")),
                Vec::<String>::new(),
                "{ok}"
            );
        }
        // Used as a value, each assignment is still reported.
        assert_eq!(
            reported(&format!("{head}let z = c ? (d ? x = 1 : x = 2) : 3;\n")),
            ["MR0185", "MR0185"]
        );
    }

    #[test]
    fn only_a_function_type_makes_a_lambda_body_a_statement() {
        let text = "struct Box<T> {}\nTakeBox(b: Box<()>) {}\nvar x = 0;\nTakeBox(() => x = 1);\n";
        assert_eq!(reported(text), ["MR0185"]);
    }

    #[test]
    fn a_conditional_statement_without_an_assignment_keeps_its_type() {
        let types = expr_types("let c = true;\nc ? 1 : 2;\n");
        assert!(types.contains(&"c ? 1 : 2: int".to_owned()), "{types:?}");
    }

    #[test]
    fn only_a_lambda_argument_takes_its_parameter_type_as_a_target() {
        // A conditional is checked against a target, and its literal does
        // not fit a `byte`; without a target the `int` is the mismatch.
        let text = "F(b: byte) {}\nvar c = true;\nF(c ? 1 : 300);\n";
        let found = reported(text);
        assert_eq!(found, ["MR0203"], "{found:?}");
    }

    #[test]
    fn an_expression_body_of_a_unit_function_may_assign() {
        let text = "F(ref x: int) => x = 1;\n\
                    G(n: int, ref x: int, ref y: int) => match (n) {\n    1 => x = 2,\n    _ => y = 3,\n};\n\
                    H(c: bool, ref x: int, ref y: int) => c ? x = 1 : y = 2;\n\
                    struct S {\n    public V: int;\n    public M() => V = 1;\n    \
                    public S(v: int) => V = v;\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
        // A getter's value is used.
        let getter = "class C {\n    private _a: int = 0;\n    public A: int => _a = 1;\n}\n";
        assert_eq!(reported(getter), ["MR0185"]);
        // So is the arm of a `match` that is a value.
        assert_eq!(
            reported("var x = 0;\nlet r = match (x) {\n    1 => x = 2,\n    _ => x = 3,\n};\n"),
            ["MR0185", "MR0185"]
        );
    }

    #[test]
    fn a_coalescing_assignment_used_as_a_value_is_mr0185() {
        let text = "Show(n: int) {}\nF(a: int?) {\n    Show(a ??= 1);\n}\n";
        let found = diagnostics_of(text);
        let at = usize::from(found[0].primary.span.range.start());
        assert_eq!(&text[at..at + 7], "a ??= 1", "{found:#?}");
    }

    #[test]
    fn an_assignment_as_a_statement_is_not_a_value() {
        let text = "var x = 0;\nvar y = 0;\nx = 1;\nx += 2;\nx = y;\n\
                    for (var i = 0; i < 3; i += 1) {\n    y = i;\n}\n\
                    for (x = 0; x < 3; x = x + 1) {}\n\
                    {\n    x -= 1;\n}\nif (x > 0) x = 1; else y = 2;\n\
                    while (x > 5) x = 0;\nlet w = x++;\nlet v = --y;\nx++;\n\
                    match (x) {\n    1 => y = 2,\n    _ => y = 3,\n}\n";
        assert_eq!(reported(text), Vec::<String>::new());
    }

    #[test]
    fn an_assignment_message_says_what_goes_wrong() {
        let d = only_diagnostic("var x = 0;\nvar y = 0;\nx = y = 5;\n");
        assert_eq!(d.message, "an assignment gives no value to use here");
        assert_eq!(
            d.primary.message.as_deref(),
            Some("this assignment must stand as a statement of its own")
        );
        assert_eq!(
            d.note.as_deref(),
            Some("assign first, then read the variable: an assignment is only a statement")
        );
        // The label is on the inner assignment.
        let text = "var x = 0;\nvar y = 0;\nx = y = 5;\n";
        let at = usize::from(d.primary.span.range.start());
        assert_eq!(&text[at..at + 5], "y = 5");
    }

    #[test]
    fn an_assignment_in_an_assignment_splits_into_two_statements() {
        let head = "var x = 0;\nvar y = 0;\n";
        for (text, expected) in [
            ("x = y = 5;", "y = 5; x = y;"),
            ("x = y += 2;", "y += 2; x = y;"),
        ] {
            let source = format!("{head}var z = 0;\n{text}\n");
            let fixed = apply_fixes(&source);
            assert!(fixed.text.contains(expected), "{}", fixed.text);
            assert_eq!(fixed.remaining, Vec::<String>::new(), "{}", fixed.text);
        }
        // The innermost of three is split in a second round.
        let once = apply_fixes("var x = 0;\nvar y = 0;\nvar z = 0;\nx = y = z = 5;\n");
        assert!(once.text.contains("y = z = 5; x = y;"), "{}", once.text);
        let twice = apply_fixes(&once.text);
        assert!(
            twice.text.contains("z = 5; y = z; x = y;"),
            "{}",
            twice.text
        );
        let nested = apply_fixes("F() {\n    var x = 0;\n    var y = 0;\n    x = y = 5;\n}\n");
        assert!(
            nested.text.contains("    y = 5; x = y;\n"),
            "{}",
            nested.text
        );
    }

    #[test]
    fn an_assignment_to_a_field_splits_into_two_statements() {
        let text = "struct P {\n    public X: int;\n    public Y: int;\n}\n\
                    var p = new P { X = 1, Y = 2 };\np.X = p.Y = 5;\n";
        let fixed = apply_fixes(text);
        assert!(fixed.text.contains("p.Y = 5; p.X = p.Y;"), "{}", fixed.text);
    }

    #[test]
    fn an_assignment_used_as_a_value_has_a_fix_only_when_it_is_mechanical() {
        let head = "Show(n: int) {}\nvar x = 0;\nvar y = 0;\nvar a = [1, 2];\n";
        for use_ in [
            "Show(x = 3);",
            "let z = (x += 1);",
            "a[x++] = y = 5;",
            "if ((x = 2) > 1) {}",
            "for (x = 0; x < 3; x = y = 1) {}",
        ] {
            let text = format!("{head}{use_}\n");
            for d in diagnostics_of(&text)
                .iter()
                .filter(|d| d.code == mora_diag::codes::MR0185)
            {
                assert!(d.fixes.is_empty(), "{use_}: {d:#?}");
            }
        }
        let found = only_diagnostic(&format!("{head}x = y = 5;\n"));
        assert_eq!(found.fixes.len(), 1);
        assert_eq!(
            found.fixes[0].message,
            "assign first, then use the variable"
        );
    }

    #[test]
    fn a_ref_parameter_with_a_default_is_mr0209() {
        for text in [
            "F(ref x: int = 1) {}\n",
            "struct S {\n    public M(ref x: int = 1) {}\n}\n",
            "F() {\n    G(ref x: int = 1) {}\n}\n",
            "F(a: int, ref x: int = 1, b: int = 2) {}\n",
            "F(ref x: int = 1, ref y: int = 2) {}\n",
        ] {
            let found = reported(text);
            assert!(
                found.iter().all(|code| code == "MR0209"),
                "{text}: {found:?}"
            );
            assert!(!found.is_empty(), "{text}");
        }
        assert_eq!(
            reported("F(ref x: int = 1, ref y: int = 2) {}\n"),
            ["MR0209"; 2]
        );
        // Other modes and parameters without a default are fine.
        for text in [
            "F(x: int = 1) {}\n",
            "F(in x: int = 1) {}\n",
            "F(ref x: int) {}\n",
            "F(move x: string) {}\n",
            "F() {\n    G(ref x: int, y: int = 1) {}\n}\n",
        ] {
            assert_eq!(reported(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn a_ref_default_message_says_what_goes_wrong() {
        let text = "F(ref x: int = 1) {}\n";
        let d = only_diagnostic(text);
        assert_eq!(
            d.message,
            "the `ref` parameter `x` cannot have a default value, because every call must pass a variable it can change"
        );
        assert_eq!(
            d.primary.message.as_deref(),
            Some("this default cannot be passed by `ref`")
        );
        let at = usize::from(d.primary.span.range.start());
        assert_eq!(&text[at..at + 1], "1");
        assert_eq!(
            d.note.as_deref(),
            Some(
                "a `ref` argument is a variable the function changes, and a default is a fixed value"
            )
        );
        let messages: Vec<_> = d.fixes.iter().map(|fix| fix.message.as_str()).collect();
        assert_eq!(messages, ["remove the default value", "remove `ref`"]);
    }

    #[test]
    fn the_fixes_of_a_ref_default_remove_the_default_or_ref() {
        let text = "F(a: int, ref x: int = 1, b: int = 2) {}\n\
                    G() {\n    H(ref  y: string  =  \"s\") {}\n}\n";
        let diagnostics = diagnostics_of(text);
        assert_eq!(diagnostics.len(), 2);
        let apply = |index: usize| {
            let mut fixed = text.to_owned();
            let mut edits: Vec<_> = diagnostics
                .iter()
                .flat_map(|d| d.fixes[index].edits.clone())
                .collect();
            edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.range.start()));
            for edit in edits {
                fixed.replace_range(
                    std::ops::Range::<usize>::from(edit.span.range),
                    &edit.replacement,
                );
            }
            fixed
        };
        let no_default = apply(0);
        assert!(
            no_default.contains("F(a: int, ref x: int, b: int = 2) {}"),
            "{no_default}"
        );
        assert!(no_default.contains("H(ref  y: string) {}"), "{no_default}");
        let no_ref = apply(1);
        assert!(
            no_ref.contains("F(a: int, x: int = 1, b: int = 2) {}"),
            "{no_ref}"
        );
        assert!(no_ref.contains("H(y: string  =  \"s\") {}"), "{no_ref}");
        assert_eq!(reported(&no_default), Vec::<String>::new());
        assert_eq!(reported(&no_ref), Vec::<String>::new());
    }
}
