//! Types and signatures as written, before their names are looked up.

use mora_span::TextRange;
use mora_syntax::SyntaxKind;
use mora_syntax::SyntaxNode;
use mora_syntax::ast::{self, AstNode};
use mora_syntax::lexer::ident_name;

use crate::items::{Visibility, visibility};
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
    let node = node.clone();
    let mut signature = SignatureRef::default();
    if let Some(it) = ast::FunctionDecl::cast(node.clone()) {
        signature.type_params = type_params(it.type_param_list());
        let params = it
            .param_list()
            .into_iter()
            .flat_map(|list| list.params())
            .map(|param| lower.param(&param))
            .collect();
        let is_async = it
            .modifiers()
            .filter_map(|modifier| modifier.kind())
            .any(|token| token.kind() == SyntaxKind::AsyncKw);
        signature.kind = SignatureRefKind::Function {
            params,
            ret: it.ret().map(|ty| lower.ty(&ty)),
            is_async,
        };
    } else if let Some(it) = ast::StructDecl::cast(node.clone()) {
        signature.type_params = type_params(it.type_param_list());
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
    } else if let Some(it) = ast::FieldDecl::cast(node.clone()) {
        let ty = it.variable_declarator().and_then(|v| v.ty());
        signature.kind = SignatureRefKind::Value(lower.optional(ty.as_ref()));
    } else if let Some(it) = ast::ClassDecl::cast(node.clone()) {
        signature.type_params = type_params(it.type_param_list());
    } else if let Some(it) = ast::UnionDecl::cast(node.clone()) {
        signature.type_params = type_params(it.type_param_list());
    } else if let Some(it) = ast::InterfaceDecl::cast(node) {
        signature.type_params = type_params(it.type_param_list());
    }
    (signature, lower.source)
}

fn type_params(list: Option<ast::TypeParamList>) -> Vec<String> {
    list.into_iter()
        .flat_map(|list| list.type_params())
        .filter_map(|param| param.name()?.ident_token())
        .map(|token| ident_name(token.text()).into_owned())
        .collect()
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
        let mut mode = ParamMode::In;
        let mut is_params = false;
        for token in param.modifiers().filter_map(|modifier| modifier.kind()) {
            match token.kind() {
                SyntaxKind::RefKw => mode = ParamMode::Ref,
                SyntaxKind::MoveKw => mode = ParamMode::Move,
                SyntaxKind::ParamsKw => is_params = true,
                _ => {}
            }
        }
        ParamRef {
            name: name_text(param.name()),
            mode,
            is_params,
            has_default: param.default().is_some(),
            ty: self.optional(param.ty().as_ref()),
        }
    }

    fn field(&mut self, field: &ast::FieldDecl) -> Option<FieldRef> {
        let declarator = field.variable_declarator()?;
        let name = declarator.name()?.ident_token()?;
        let visibility = field
            .modifiers()
            .filter_map(|modifier| modifier.kind())
            .find_map(|token| visibility(token.kind()))
            .unwrap_or(Visibility::Folder);
        self.source.fields.push(name.text_range());
        Some(FieldRef {
            name: ident_name(name.text()).into_owned(),
            visibility,
            ty: self.optional(declarator.ty().as_ref()),
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
            .map(|elem| {
                let name = elem
                    .name()
                    .and_then(|name| name.ident_token())
                    .map(|token| ident_name(token.text()).into_owned());
                (name, self.optional(elem.ty().as_ref()))
            })
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
            segments.push(ident_name(token.text()).into_owned());
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

fn name_text(name: Option<ast::Name>) -> String {
    name.and_then(|name| name.ident_token())
        .map(|token| ident_name(token.text()).into_owned())
        .unwrap_or_default()
}

/// The type a predefined keyword names, [`TypeRef::Missing`] for any other
/// kind.
pub(crate) fn predefined(kind: SyntaxKind) -> TypeRef {
    let primitive = match kind {
        SyntaxKind::BoolKw => Primitive::Bool,
        SyntaxKind::ByteKw => Primitive::Byte,
        SyntaxKind::SbyteKw => Primitive::SByte,
        SyntaxKind::ShortKw => Primitive::Short,
        SyntaxKind::UshortKw => Primitive::UShort,
        SyntaxKind::IntKw => Primitive::Int,
        SyntaxKind::UintKw => Primitive::UInt,
        SyntaxKind::LongKw => Primitive::Long,
        SyntaxKind::UlongKw => Primitive::ULong,
        SyntaxKind::NintKw => Primitive::NInt,
        SyntaxKind::NuintKw => Primitive::NUInt,
        SyntaxKind::FloatKw => Primitive::Float,
        SyntaxKind::DoubleKw => Primitive::Double,
        SyntaxKind::CharKw => Primitive::Char,
        SyntaxKind::StringKw => return TypeRef::String,
        // A keyword this function does not know yet names no type.
        _ => return TypeRef::Missing,
    };
    TypeRef::Primitive(primitive)
}

/// The keywords that name types, for suggestions.
pub(crate) const TYPE_KEYWORDS: &[&str] = &[
    "bool", "byte", "char", "double", "float", "int", "long", "nint", "nuint", "sbyte", "short",
    "string", "uint", "ulong", "ushort",
];

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
}
