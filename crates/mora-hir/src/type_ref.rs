//! Types and signatures as written, before their names are looked up.

use mora_span::TextRange;
use mora_syntax::ast::{self, AstNode};
use mora_syntax::{PREDEFINED_TYPES, SyntaxKind, SyntaxNode};

use crate::items::{Visibility, Written};
use crate::name::{name_of, token_name};
use crate::ty::{ParamMode, Primitive};

/// A type as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeRef {
    Primitive(Primitive),
    String,
    Path(PathRef),
    Tuple(Vec<(Option<String>, TypeRef)>),
    Array(Box<TypeRef>),
    Optional(Box<TypeRef>),
    Function(Vec<TypeRef>, Box<TypeRef>),
    Weak(Box<TypeRef>),
    Any(Box<TypeRef>),
    Splice,
    Missing,
}

/// A path in a type, such as `Store.User` or `List<int>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRef {
    /// The segments, in NFC, without generic arguments.
    pub segments: Vec<String>,
    /// Whether a segment has a generic argument list.
    pub generic: bool,
    /// The generic arguments of every segment, in order.
    pub args: Vec<TypeRef>,
    /// Its position among the paths of its item's signature or body, which
    /// indexes the ranges of their source map.
    pub index: usize,
}

/// The signature of an item as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SignatureRef {
    /// The names of the item's own type parameters.
    pub(crate) type_params: Vec<String>,
    /// What the item declares.
    pub(crate) kind: SignatureRefKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum SignatureRefKind {
    Function {
        params: Vec<ParamRef>,
        ret: Option<TypeRef>,
        is_async: bool,
        /// How `this` is taken, as the modifiers write it, should the
        /// function be a method.
        receiver: ParamMode,
    },
    Struct(Vec<FieldRef>),
    Value(TypeRef),
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParamRef {
    pub(crate) name: String,
    pub(crate) mode: ParamMode,
    pub(crate) is_params: bool,
    pub(crate) has_default: bool,
    pub(crate) ty: TypeRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldRef {
    pub(crate) name: String,
    pub(crate) visibility: Visibility,
    pub(crate) ty: TypeRef,
    pub(crate) needs_init: bool,
}

/// Where the paths of one item's signature are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SignatureSource {
    /// The range of each segment of each path, indexed by [`PathRef::index`].
    pub(crate) paths: Vec<Vec<TextRange>>,
    /// The name of each field of a struct, in order.
    pub(crate) fields: Vec<TextRange>,
}

/// The signature of the declaration `node`, which the item tree keeps.
pub(crate) fn signature(node: &SyntaxNode) -> (SignatureRef, SignatureSource) {
    let mut lower = LowerTypes::default();
    let node = *node;
    let mut signature = SignatureRef::default();
    if let Some(it) = ast::FunctionDecl::cast(node) {
        signature.type_params = type_param_names(it.type_param_list()).collect();
        let params = it
            .param_list()
            .into_iter()
            .flat_map(|list| list.params())
            .map(|param| lower.param(&param))
            .collect();
        signature.kind = SignatureRefKind::Function {
            params,
            ret: it.ret().map(|ty| lower.ty(&ty)),
            is_async: Written::of(it.modifiers()).is_async,
            receiver: receiver_mode(it.modifiers()),
        };
    } else if let Some(it) = ast::StructDecl::cast(node) {
        signature.type_params = type_param_names(it.type_param_list()).collect();
        let fields = it
            .member_list()
            .into_iter()
            .flat_map(|list| list.members())
            .filter_map(|member| match member {
                ast::Member::FieldDecl(field) => lower.field(&field),
                _ => None,
            })
            .collect();
        signature.kind = SignatureRefKind::Struct(fields);
    } else if let Some(it) = ast::FieldDecl::cast(node) {
        let ty = it.variable_declarator().and_then(|v| v.ty());
        signature.kind = SignatureRefKind::Value(lower.optional(ty.as_ref()));
    } else if let Some(it) = ast::ClassDecl::cast(node) {
        signature.type_params = type_param_names(it.type_param_list()).collect();
    } else if let Some(it) = ast::UnionDecl::cast(node) {
        signature.type_params = type_param_names(it.type_param_list()).collect();
    } else if let Some(it) = ast::InterfaceDecl::cast(node) {
        signature.type_params = type_param_names(it.type_param_list()).collect();
    }
    (signature, lower.source)
}

/// The mode of the parameter `param` as its modifiers write it (§10.3):
/// `ref`, `move`, or `in` when neither is written.
pub(crate) fn param_mode(param: &ast::Param) -> ParamMode {
    let mut mode = ParamMode::In;
    for token in param.modifiers().filter_map(|modifier| modifier.kind()) {
        match token.kind() {
            SyntaxKind::RefKw => mode = ParamMode::Ref,
            SyntaxKind::MoveKw => mode = ParamMode::Move,
            _ => {}
        }
    }
    mode
}

/// How a method with the modifiers `modifiers` takes `this` (§10.3):
/// `ref` when it is `mut`, `move` when it is `move`, `in` otherwise.
fn receiver_mode<'a>(modifiers: impl IntoIterator<Item = ast::Modifier<'a>>) -> ParamMode {
    let mut mode = ParamMode::In;
    for token in modifiers.into_iter().filter_map(|modifier| modifier.kind()) {
        match token.kind() {
            SyntaxKind::MutKw => mode = ParamMode::Ref,
            SyntaxKind::MoveKw => mode = ParamMode::Move,
            _ => {}
        }
    }
    mode
}

/// The names of the type parameters `list` declares, in order; a parameter
/// whose name the parser recovered from has none.
pub(crate) fn type_param_names<'a>(
    list: Option<ast::TypeParamList<'a>>,
) -> impl Iterator<Item = String> + 'a {
    list.into_iter()
        .flat_map(|list| list.type_params())
        .filter_map(|param| name_of(param.name()))
}

/// Lowers types as written and keeps where their paths are.
#[derive(Default)]
pub(crate) struct LowerTypes {
    source: SignatureSource,
}

impl LowerTypes {
    /// The ranges of the segments of each path lowered so far.
    pub(crate) fn into_paths(self) -> Vec<Vec<TextRange>> {
        self.source.paths
    }

    fn param(&mut self, param: &ast::Param) -> ParamRef {
        let is_params = param
            .modifiers()
            .filter_map(|modifier| modifier.kind())
            .any(|token| token.kind() == SyntaxKind::ParamsKw);
        ParamRef {
            name: name_of(param.name()).unwrap_or_default(),
            mode: param_mode(param),
            is_params,
            has_default: param.default().is_some(),
            ty: self.optional(param.ty().as_ref()),
        }
    }

    fn field(&mut self, field: &ast::FieldDecl) -> Option<FieldRef> {
        let declarator = field.variable_declarator()?;
        let name = declarator.name()?.ident_token()?;
        self.source.fields.push(name.text_range());
        let written = Written::of(field.modifiers());
        Some(FieldRef {
            name: token_name(name),
            visibility: written.visibility_or_folder(),
            ty: self.optional(declarator.ty().as_ref()),
            needs_init: declarator.eq_token().is_none() && !written.is_static,
        })
    }

    pub(crate) fn optional(&mut self, ty: Option<&ast::Type>) -> TypeRef {
        ty.map_or(TypeRef::Missing, |ty| self.ty(ty))
    }

    fn boxed(&mut self, ty: Option<ast::Type>) -> Box<TypeRef> {
        Box::new(self.optional(ty.as_ref()))
    }

    pub(crate) fn ty(&mut self, ty: &ast::Type) -> TypeRef {
        match ty {
            ast::Type::PredefinedType(it) => it
                .kind()
                .map_or(TypeRef::Missing, |token| predefined(token.kind())),
            ast::Type::PathType(it) => self.path(it).unwrap_or(TypeRef::Missing),
            ast::Type::TupleType(it) => self.tuple(it),
            ast::Type::ArrayType(it) => TypeRef::Array(self.boxed(it.ty())),
            ast::Type::OptionalType(it) => TypeRef::Optional(self.boxed(it.ty())),
            ast::Type::WeakType(it) => TypeRef::Weak(self.boxed(it.ty())),
            ast::Type::AnyType(it) => {
                let path = it.path_type().and_then(|it| self.path(&it));
                let path = path.unwrap_or(TypeRef::Missing);
                TypeRef::Any(Box::new(path))
            }
            ast::Type::FnType(it) => {
                let params = it
                    .params()
                    .into_iter()
                    .flat_map(|params| params.tuple_type_elems())
                    .map(|elem| self.optional(elem.ty().as_ref()))
                    .collect();
                TypeRef::Function(params, self.boxed(it.ret()))
            }
            ast::Type::SpliceType(_) => TypeRef::Splice,
        }
    }

    /// A tuple type, or a type in parentheses: `(int)` is `int`, while
    /// `(int,)` and `(Id: int)` are tuples of one element (§5.5).
    fn tuple(&mut self, tuple: &ast::TupleType) -> TypeRef {
        let elems: Vec<_> = tuple.tuple_type_elems().collect();
        if let [elem] = elems.as_slice()
            && elem.name().is_none()
            && tuple.comma_token().is_none()
        {
            return self.optional(elem.ty().as_ref());
        }
        let elems = elems
            .iter()
            .map(|elem| (name_of(elem.name()), self.optional(elem.ty().as_ref())))
            .collect();
        TypeRef::Tuple(elems)
    }

    /// A path, `None` when the parser recovered from a missing name.
    fn path(&mut self, path: &ast::PathType) -> Option<TypeRef> {
        let mut segments = Vec::new();
        let mut ranges = Vec::new();
        let mut generic = false;
        let mut arg_lists = Vec::new();
        // The parser gives every path at least one segment.
        for segment in path.path()?.path_segments() {
            let token = segment.name_ref()?.ident_token()?;
            segments.push(token_name(token));
            ranges.push(token.text_range());
            if let Some(list) = segment.generic_arg_list() {
                generic = true;
                arg_lists.push(list);
            }
        }
        // The path takes its index before its arguments, so paths are
        // numbered in the order of the text.
        let index = self.source.paths.len();
        self.source.paths.push(ranges);
        let args = arg_lists
            .iter()
            .flat_map(|list| list.types())
            .map(|ty| self.ty(&ty))
            .collect();
        Some(TypeRef::Path(PathRef {
            segments,
            generic,
            args,
            index,
        }))
    }
}

/// The type a predefined keyword names, [`TypeRef::Missing`] for any other
/// kind. The keywords are [`PREDEFINED_TYPES`]: the parser decides what is
/// a type keyword, and this says which type each one names.
pub(crate) fn predefined(kind: SyntaxKind) -> TypeRef {
    use SyntaxKind as K;
    match kind {
        K::BoolKw => TypeRef::Primitive(Primitive::Bool),
        K::ByteKw => TypeRef::Primitive(Primitive::Byte),
        K::SbyteKw => TypeRef::Primitive(Primitive::SByte),
        K::ShortKw => TypeRef::Primitive(Primitive::Short),
        K::UshortKw => TypeRef::Primitive(Primitive::UShort),
        K::IntKw => TypeRef::Primitive(Primitive::Int),
        K::UintKw => TypeRef::Primitive(Primitive::UInt),
        K::LongKw => TypeRef::Primitive(Primitive::Long),
        K::UlongKw => TypeRef::Primitive(Primitive::ULong),
        K::NintKw => TypeRef::Primitive(Primitive::NInt),
        K::NuintKw => TypeRef::Primitive(Primitive::NUInt),
        K::FloatKw => TypeRef::Primitive(Primitive::Float),
        K::DoubleKw => TypeRef::Primitive(Primitive::Double),
        K::CharKw => TypeRef::Primitive(Primitive::Char),
        K::StringKw => TypeRef::String,
        _ => TypeRef::Missing,
    }
}

/// The keywords that name types, for suggestions.
pub(crate) fn type_keywords() -> impl Iterator<Item = &'static str> {
    PREDEFINED_TYPES.iter().filter_map(|keyword| keyword.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_type_keywords_name_predefined_types() {
        assert_eq!(predefined(SyntaxKind::StringKw), TypeRef::String);
        assert_eq!(
            predefined(SyntaxKind::IntKw),
            TypeRef::Primitive(Primitive::Int)
        );
        assert_eq!(predefined(SyntaxKind::Ident), TypeRef::Missing);
        assert_eq!(predefined(SyntaxKind::ClassKw), TypeRef::Missing);
    }

    #[test]
    fn every_type_keyword_names_a_type() {
        for &keyword in PREDEFINED_TYPES {
            let ty = predefined(keyword);
            assert_ne!(ty, TypeRef::Missing, "{keyword:?} names no type");
            // `Primitive::keyword` gives the text back.
            if let TypeRef::Primitive(primitive) = ty {
                assert_eq!(Some(primitive.keyword()), keyword.text(), "{keyword:?}");
            }
        }
    }

    #[test]
    fn every_type_keyword_is_suggested() {
        let keywords: Vec<_> = type_keywords().collect();
        assert_eq!(
            keywords,
            [
                "bool", "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "nint",
                "nuint", "float", "double", "char", "string",
            ]
        );
    }
}
