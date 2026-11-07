use crate::defs::ItemId;
use crate::items::Visibility;

/// A type (§5.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// A primitive type of §5.2 other than `()`.
    Primitive(Primitive),
    /// `string` (§5.3).
    String,
    /// A tuple (§5.5). The empty tuple is the unit type `()`.
    Tuple(Vec<TupleElem>),
    /// A struct, by its declaration (§6.2).
    Struct(ItemId),
    /// A type that a later part of the compiler gives meaning to.
    Unsupported(Unsupported),
    /// A type that could not be lowered; a diagnostic says why.
    Error,
}

impl Ty {
    /// The unit type `()`, the empty tuple.
    pub const UNIT: Ty = Ty::Tuple(Vec::new());
}

/// The primitive types of §5.2, by keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Primitive {
    Bool,
    Byte,
    SByte,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    NInt,
    NUInt,
    Float,
    Double,
    Char,
}

impl Primitive {
    /// The keyword that names the type.
    pub fn keyword(self) -> &'static str {
        match self {
            Primitive::Bool => "bool",
            Primitive::Byte => "byte",
            Primitive::SByte => "sbyte",
            Primitive::Short => "short",
            Primitive::UShort => "ushort",
            Primitive::Int => "int",
            Primitive::UInt => "uint",
            Primitive::Long => "long",
            Primitive::ULong => "ulong",
            Primitive::NInt => "nint",
            Primitive::NUInt => "nuint",
            Primitive::Float => "float",
            Primitive::Double => "double",
            Primitive::Char => "char",
        }
    }
}

/// An element of a tuple type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TupleElem {
    /// The name of a named element, as in `(Id: int, Name: string)`.
    pub name: Option<String>,
    /// Its type.
    pub ty: Ty,
}

/// A type whose meaning a later part of the compiler defines: generic,
/// class, union, enum and interface types, arrays, options, function types
/// and the rest. Its parts are lowered, so the names in them are checked.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported {
    /// What the type is.
    pub construct: Construct,
    /// The declaration it names, when it names one.
    pub item: Option<ItemId>,
    /// The types inside it: generic arguments, the element type, or the
    /// parameter types and then the result type of a function type.
    pub parts: Vec<Ty>,
}

/// What an [`Unsupported`] type is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Construct {
    /// A type parameter of the item or of a type around it (§8).
    TypeParam,
    /// A declared type with generic arguments, or a generic struct (§8).
    Generic,
    /// A class (§6.3).
    Class,
    /// A union (§6.6).
    Union,
    /// An enum (§6.7).
    Enum,
    /// An interface (§6.8).
    Interface,
    /// `T[]` (§5.4).
    Array,
    /// `T?` (§5.6).
    Optional,
    /// `(T) => R` (§5.8).
    Function,
    /// `weak T` (§10.7).
    Weak,
    /// `any I` (§8.4).
    Any,
    /// `$t` inside a macro (§9.3).
    Splice,
    /// A type of the standard library while it is not loaded, as in tests
    /// that compile files on their own. Nothing is known about it.
    External,
}

/// What an item declares about its types (§14.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// A function or method.
    Function(FnSignature),
    /// A struct and its fields.
    Struct(Vec<Field>),
    /// A `const` or `static` at namespace level, and its type. A field
    /// declared in a type is not an item: a struct lists it in
    /// [`Signature::Struct`], and a class's fields are not lowered yet.
    Value(Ty),
    /// An item whose signature a later part of the compiler lowers.
    None,
}

/// The signature of a function (§6.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSignature {
    /// The parameters, in order.
    pub params: Vec<Param>,
    /// The return type, `()` when none is written.
    pub ret: Ty,
    /// Whether the function is `async` (§12).
    pub is_async: bool,
}

/// A parameter of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Its name, empty when the parser recovered from a missing one.
    pub name: String,
    /// How it is passed (§10.3).
    pub mode: ParamMode,
    /// Whether it is a `params` parameter (§6.9).
    pub is_params: bool,
    /// Whether it has a default value.
    pub has_default: bool,
    /// Its type.
    pub ty: Ty,
}

/// A parameter mode (§10.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamMode {
    /// `in`, the default.
    In,
    /// `ref`.
    Ref,
    /// `move`.
    Move,
}

/// A field of a struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Its name.
    pub name: String,
    /// Its visibility as written; [`Visibility::Folder`] when none is.
    pub visibility: Visibility,
    /// Its type.
    pub ty: Ty,
}
