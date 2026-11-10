use mora_span::{Db, Project, SourceFile};

use crate::arena::arena_index;
use crate::defs::ItemId;
use crate::items::{
    ItemIdx, ItemKind, ItemTree, ValueKind, ValueMember, Visibility, item_tree, signature_refs,
};
use crate::resolve::{
    ResolveErrorKind, Resolved, Scope, effective_visibility, enclosing_types, item_data,
    resolve_path,
};
use crate::ty::{Construct, Field, FnSignature, Param, Signature, TupleElem, Ty, Unsupported};
use crate::type_ref::{PathRef, SignatureRef, SignatureRefKind, TypeRef};

/// The signature of an item, with the names in its types looked up (§14.5).
/// It reads no text range, so an edit inside a body does not run it again.
pub fn signature(db: &dyn Db, item: ItemId) -> &Signature {
    &item_signature(db, item.project, item.file, item.item).signature
}

/// The field of the struct `item` that its value member at `index`
/// declares, with its position among the fields of [`Signature::Struct`]:
/// a field or a constant, `static` or not. `None` for another member, or
/// when `item` is not a struct.
pub fn struct_field(db: &dyn Db, item: ItemId, index: u32) -> Option<(usize, &Field)> {
    let Signature::Struct(fields) = signature(db, item) else {
        return None;
    };
    let values = &item_tree(db, item.file).item(item.item).values;
    if !declares_field(values.get(index as usize)?) {
        return None;
    }
    let n = values[..index as usize]
        .iter()
        .filter(|member| declares_field(member))
        .count();
    Some((n, fields.get(n)?))
}

/// The value member that declares the field at position `n` of
/// [`Signature::Struct`] of `item`, with its position among the value
/// members: the inverse of [`struct_field`].
pub fn field_member(db: &dyn Db, item: ItemId, n: usize) -> Option<(u32, &ValueMember)> {
    let (index, member) = item_tree(db, item.file)
        .item(item.item)
        .values
        .iter()
        .enumerate()
        .filter(|(_, member)| declares_field(member))
        .nth(n)?;
    Some((u32::try_from(index).ok()?, member))
}

/// Whether `member` declares a field of [`Signature::Struct`]: a field or a
/// constant.
fn declares_field(member: &ValueMember) -> bool {
    matches!(member.kind, ValueKind::Field | ValueKind::Const)
}

/// The type of `this` in the method `item`: the struct it is declared in
/// (§10.3). `None` for a function that takes no receiver, and for a method
/// of any other type, or of a generic struct, whose type is not a
/// [`Ty::Struct`].
pub fn this_type(db: &dyn Db, item: ItemId) -> Option<Ty> {
    let Signature::Function(function) = signature(db, item) else {
        return None;
    };
    function.receiver?;
    let parent = item_tree(db, item.file).item(item.item).parent?;
    let generic = !signature_refs(db, item.file)[arena_index(parent)]
        .type_params
        .is_empty();
    let parent = ItemId {
        item: parent,
        ..item
    };
    (item_data(db, parent).kind == ItemKind::Struct && !generic).then_some(Ty::Struct(parent))
}

/// What is wrong with the types of an item's signature.
pub(crate) fn signature_errors(db: &dyn Db, item: ItemId) -> &[SignatureError] {
    &item_signature(db, item.project, item.file, item.item).errors
}

/// A signature and the errors found while lowering it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoweredSignature {
    signature: Signature,
    errors: Vec<SignatureError>,
}

/// A path in a signature that is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignatureError {
    /// The path, by its position among the paths of the item's signature.
    pub(crate) path: usize,
    /// The segment the error is about, counting from zero.
    pub(crate) segment: usize,
    /// The text of that segment, in NFC.
    pub(crate) name: String,
    /// What is wrong.
    pub(crate) kind: SignatureErrorKind,
}

/// What is wrong with a path in a signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignatureErrorKind {
    /// The path does not resolve from the scope (`MR0127`, `MR0128`,
    /// `MR0129`).
    Resolve(Scope, ResolveErrorKind),
    /// The path names something that is not a type (`MR0127`).
    NotAType(Resolved),
    /// The path names a type less visible than the member (`MR0123`).
    LessVisible {
        /// The type.
        ty: ItemId,
        /// The field it is the type of, by its position, or `None` for the
        /// item itself.
        field: Option<usize>,
        /// The effective visibility of the member.
        member: Visibility,
    },
}

#[salsa::tracked(returns(ref))]
fn item_signature(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    item: ItemIdx,
) -> LoweredSignature {
    let tree = item_tree(db, file);
    let refs = signature_refs(db, file);
    let scope = Scope {
        project,
        file,
        // A function or a value has no members, so looking up from inside it
        // is looking up from the type around it.
        within: Some(item),
    };
    let type_params = type_params(tree, refs, Some(item));
    let mut lower = ResolveTypes {
        db,
        scope,
        type_params,
        field: None,
        member: effective_visibility(db, scope.item(item)),
        errors: Vec::new(),
    };
    let signature = match &refs[arena_index(item)].kind {
        SignatureRefKind::Function {
            params,
            ret,
            is_async,
            receiver,
        } => Signature::Function(FnSignature {
            params: params
                .iter()
                .map(|param| Param {
                    name: param.name.clone(),
                    mode: param.mode,
                    is_params: param.is_params,
                    has_default: param.has_default,
                    ty: lower.ty(&param.ty),
                })
                .collect(),
            ret: ret.as_ref().map_or(Ty::UNIT, |ret| lower.ty(ret)),
            is_async: *is_async,
            is_generic: !refs[arena_index(item)].type_params.is_empty(),
            receiver: {
                let data = tree.item(item);
                (data.parent.is_some() && !data.is_static).then_some(*receiver)
            },
        }),
        SignatureRefKind::Struct(fields) => {
            let owner = lower.member;
            let fields = fields
                .iter()
                .enumerate()
                .map(|(position, field)| {
                    lower.field = Some(position);
                    lower.member = owner.min(field.visibility);
                    Field {
                        name: field.name.clone(),
                        visibility: field.visibility,
                        ty: lower.ty(&field.ty),
                        needs_init: field.needs_init,
                    }
                })
                .collect();
            Signature::Struct(fields)
        }
        SignatureRefKind::Value(ty) => Signature::Value(lower.ty(ty)),
        SignatureRefKind::None => Signature::None,
    };
    LoweredSignature {
        signature,
        errors: lower.errors,
    }
}

/// Looks up the names in types as written.
pub(crate) struct ResolveTypes<'db> {
    /// The database the names are looked up in.
    pub(crate) db: &'db dyn Db,
    /// Where the names are looked up from.
    pub(crate) scope: Scope,
    /// The type parameters of the item and of the types around it.
    pub(crate) type_params: Vec<String>,
    /// The field whose type is being lowered, `None` for the item itself.
    pub(crate) field: Option<usize>,
    /// The effective visibility of the member whose types are being lowered.
    pub(crate) member: Visibility,
    /// The errors so far.
    pub(crate) errors: Vec<SignatureError>,
}

/// The type parameters in scope inside `within` and the types around it.
pub(crate) fn type_params(
    tree: &ItemTree,
    refs: &[SignatureRef],
    within: Option<ItemIdx>,
) -> Vec<String> {
    enclosing_types(tree, within)
        .flat_map(|ty| refs[arena_index(ty)].type_params.iter().cloned())
        .collect()
}

impl ResolveTypes<'_> {
    pub(crate) fn ty(&mut self, ty: &TypeRef) -> Ty {
        match ty {
            TypeRef::Primitive(primitive) => Ty::Primitive(*primitive),
            TypeRef::String => Ty::String,
            TypeRef::Tuple(elems) => Ty::Tuple(
                elems
                    .iter()
                    .map(|(name, ty)| TupleElem {
                        name: name.clone(),
                        ty: self.ty(ty),
                    })
                    .collect(),
            ),
            TypeRef::Path(path) => self.path(path),
            TypeRef::Array(elem) => self.unsupported(Construct::Array, None, [&**elem]),
            TypeRef::Optional(elem) => self.unsupported(Construct::Optional, None, [&**elem]),
            TypeRef::Weak(elem) => self.unsupported(Construct::Weak, None, [&**elem]),
            TypeRef::Any(elem) => self.unsupported(Construct::Any, None, [&**elem]),
            TypeRef::Function(params, ret) => {
                self.unsupported(Construct::Function, None, params.iter().chain([&**ret]))
            }
            TypeRef::Splice => self.unsupported(Construct::Splice, None, []),
            TypeRef::Missing => Ty::Error,
        }
    }

    fn unsupported<'a>(
        &mut self,
        construct: Construct,
        item: Option<ItemId>,
        parts: impl IntoIterator<Item = &'a TypeRef>,
    ) -> Ty {
        Ty::Unsupported(Unsupported {
            construct,
            item,
            parts: parts.into_iter().map(|part| self.ty(part)).collect(),
        })
    }

    /// Reports `error` and gives the error type. The generic arguments are
    /// still lowered, so the names in them are checked too.
    fn fail(&mut self, error: SignatureError, args: &[TypeRef]) -> Ty {
        self.errors.push(error);
        for arg in args {
            self.ty(arg);
        }
        Ty::Error
    }

    fn path(&mut self, path: &PathRef) -> Ty {
        if self.type_params.contains(&path.segments[0]) {
            return self.unsupported(Construct::TypeParam, None, &path.args);
        }
        let error = |segment: usize, kind| SignatureError {
            path: path.index,
            segment,
            name: path.segments[segment].clone(),
            kind,
        };
        let resolved = match resolve_path(self.db, self.scope, &path.segments) {
            Ok(resolved) => resolved,
            Err(failed) => {
                let kind = SignatureErrorKind::Resolve(self.scope, failed.kind);
                return self.fail(error(failed.segment, kind), &path.args);
            }
        };
        let last = path.segments.len() - 1;
        let item = match resolved {
            Resolved::Item(item) => item,
            Resolved::External => return self.unsupported(Construct::External, None, &path.args),
            other => {
                return self.fail(error(last, SignatureErrorKind::NotAType(other)), &path.args);
            }
        };
        let construct = match item_data(self.db, item).kind {
            ItemKind::Struct => Construct::Generic,
            ItemKind::Class => Construct::Class,
            ItemKind::Union => Construct::Union,
            ItemKind::Enum => Construct::Enum,
            ItemKind::Interface => Construct::Interface,
            ItemKind::Function | ItemKind::Macro | ItemKind::Field => {
                let kind = SignatureErrorKind::NotAType(Resolved::Item(item));
                return self.fail(error(last, kind), &path.args);
            }
        };
        if effective_visibility(self.db, item) < self.member {
            self.errors.push(error(
                last,
                SignatureErrorKind::LessVisible {
                    ty: item,
                    field: self.field,
                    member: self.member,
                },
            ));
        }
        let generic = path.generic
            || !signature_refs(self.db, item.file)[arena_index(item.item)]
                .type_params
                .is_empty();
        if construct == Construct::Generic && !generic {
            return Ty::Struct(item);
        }
        self.unsupported(construct, Some(item), &path.args)
    }
}

#[cfg(test)]
mod tests {
    use salsa::Setter;

    use crate::test_db::{
        TestDb, app, codes, file_named, function, item_named, signature_of, unsupported,
    };
    use crate::*;

    /// The types of the parameters of the function `F` in a file of its own.
    fn param_types(text: &str) -> Vec<Ty> {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        function(signature_of(&db, project, "src/Program.mr", "F"))
            .params
            .into_iter()
            .map(|param| param.ty)
            .collect()
    }

    #[test]
    fn every_primitive_keyword_names_its_type() {
        let types = param_types(
            "F(a: bool, b: byte, c: sbyte, d: short, e: ushort, f: int, g: uint, h: long, \
         i: ulong, j: nint, k: nuint, l: float, m: double, n: char, o: string) {}\n",
        );
        let primitives = [
            Primitive::Bool,
            Primitive::Byte,
            Primitive::SByte,
            Primitive::Short,
            Primitive::UShort,
            Primitive::Int,
            Primitive::UInt,
            Primitive::Long,
            Primitive::ULong,
            Primitive::NInt,
            Primitive::NUInt,
            Primitive::Float,
            Primitive::Double,
            Primitive::Char,
        ];
        let mut expected: Vec<Ty> = primitives.into_iter().map(Ty::Primitive).collect();
        expected.push(Ty::String);
        assert_eq!(types, expected);
    }

    #[test]
    fn tuple_forms_lower_to_tuples() {
        let types = param_types(
            "F(a: (), b: (int, string), c: (Id: int, Name: string), d: (int), e: (int,), \
         f: (Id: int), g: (int, (bool, char))) {}\n",
        );
        let elem = |name: Option<&str>, ty: Ty| TupleElem {
            name: name.map(str::to_owned),
            ty,
        };
        let int = Ty::Primitive(Primitive::Int);
        assert_eq!(types[0], Ty::UNIT);
        assert_eq!(
            types[1],
            Ty::Tuple(vec![elem(None, int.clone()), elem(None, Ty::String)])
        );
        assert_eq!(
            types[2],
            Ty::Tuple(vec![
                elem(Some("Id"), int.clone()),
                elem(Some("Name"), Ty::String)
            ])
        );
        // Parentheses around one type group it; a comma or a name makes a tuple.
        assert_eq!(types[3], int);
        assert_eq!(types[4], Ty::Tuple(vec![elem(None, int.clone())]));
        assert_eq!(types[5], Ty::Tuple(vec![elem(Some("Id"), int.clone())]));
        assert_eq!(
            types[6],
            Ty::Tuple(vec![
                elem(None, int),
                elem(
                    None,
                    Ty::Tuple(vec![
                        elem(None, Ty::Primitive(Primitive::Bool)),
                        elem(None, Ty::Primitive(Primitive::Char)),
                    ])
                ),
            ])
        );
    }

    #[test]
    fn function_signatures_keep_names_modes_defaults_async_and_generics() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "async F(a: int, in b: int, ref c: int, move d: int, params e: int, f: int = 1) {}\n\
             G(): string => \"\";\nId<T>(x: T): T => x;\n",
            )],
        );
        let f = function(signature_of(&db, project, "src/Program.mr", "F"));
        assert!(f.is_async);
        assert!(!f.is_generic);
        assert_eq!(f.ret, Ty::UNIT);
        let shape: Vec<_> = f
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.mode, p.is_params, p.has_default))
            .collect();
        assert_eq!(
            shape,
            [
                ("a", ParamMode::In, false, false),
                ("b", ParamMode::In, false, false),
                ("c", ParamMode::Ref, false, false),
                ("d", ParamMode::Move, false, false),
                ("e", ParamMode::In, true, false),
                ("f", ParamMode::In, false, true),
            ]
        );
        let g = function(signature_of(&db, project, "src/Program.mr", "G"));
        assert!(!g.is_async);
        assert_eq!(g.ret, Ty::String);
        assert!(function(signature_of(&db, project, "src/Program.mr", "Id")).is_generic);
    }

    #[test]
    fn a_value_member_finds_the_struct_field_it_declares() {
        let db = TestDb::new();
        let text = "struct P {\n    X: int;\n    Get: int => X;\n    const K: int = 1;\n    \
                    static S: long = 2;\n    Y: string;\n}\nclass C {\n    Z: int;\n}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let p = item_named(&db, project, file, "P");
        let found =
            |index: u32| struct_field(&db, p, index).map(|(n, field)| (n, field.name.clone()));
        assert_eq!(found(0), Some((0, "X".to_owned())));
        assert_eq!(found(1), None);
        assert_eq!(found(2), Some((1, "K".to_owned())));
        assert_eq!(found(3), Some((2, "S".to_owned())));
        assert_eq!(found(4), Some((3, "Y".to_owned())));
        assert_eq!(found(5), None);
        assert_eq!(
            struct_field(&db, item_named(&db, project, file, "C"), 0),
            None
        );
    }

    #[test]
    fn a_field_finds_the_value_member_that_declares_it() {
        let db = TestDb::new();
        let text = "struct P {\n    X: int;\n    Get: int => X;\n    const K: int = 1;\n    \
                    static S: long = 2;\n    Y: string;\n}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let p = item_named(&db, project, file, "P");
        let found = |n: usize| {
            field_member(&db, p, n)
                .map(|(index, member)| (index, member.name.clone(), member.is_instance_field()))
        };
        assert_eq!(found(0), Some((0, "X".to_owned(), true)));
        assert_eq!(found(1), Some((2, "K".to_owned(), false)));
        assert_eq!(found(2), Some((3, "S".to_owned(), false)));
        assert_eq!(found(3), Some((4, "Y".to_owned(), true)));
        assert_eq!(found(4), None);
    }

    #[test]
    fn this_has_the_type_of_the_struct_of_its_method() {
        let db = TestDb::new();
        let text = "struct P {\n    M() {}\n    static S() {}\n}\nclass C {\n    M() {}\n}\n\
                    struct G<T> {\n    M() {}\n}\nF() {}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let tree = item_tree(&db, file);
        let item = |name: &str, parent: &str| {
            let (item, _) = tree
                .items()
                .find(|(_, data)| {
                    data.name == name
                        && data.parent.map(|p| tree.item(p).name.as_str()) == Some(parent)
                })
                .expect("the item");
            ItemId {
                project,
                file,
                item,
            }
        };
        let p = item_named(&db, project, file, "P");
        assert_eq!(this_type(&db, item("M", "P")), Some(Ty::Struct(p)));
        assert_eq!(this_type(&db, item("S", "P")), None);
        assert_eq!(this_type(&db, item("M", "C")), None);
        assert_eq!(this_type(&db, item("M", "G")), None);
        assert_eq!(this_type(&db, item_named(&db, project, file, "F")), None);
    }

    #[test]
    fn a_method_records_how_it_takes_its_receiver() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "struct P {\n    X: int;\n    In(): int => X;\n    mut Ref() {}\n    \
                 move Move() {}\n    static S() {}\n    static mut T() {}\n}\nF() {}\nmut G() {}\n",
            )],
        );
        let receiver =
            |name: &str| function(signature_of(&db, project, "src/Program.mr", name)).receiver;
        assert_eq!(receiver("In"), Some(ParamMode::In));
        assert_eq!(receiver("Ref"), Some(ParamMode::Ref));
        assert_eq!(receiver("Move"), Some(ParamMode::Move));
        assert_eq!(receiver("S"), None);
        assert_eq!(receiver("T"), None);
        assert_eq!(receiver("F"), None);
        assert_eq!(receiver("G"), None);
    }

    #[test]
    fn structs_are_nominal_and_list_their_fields() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "struct Point {\n    public X: int;\n    private mut Y: Point;\n    file Z: float;\n    \
             internal W: char;\n    mut static V: bool;\n    Length(): int => X;\n}\n\
             static Origin: Point = default;\nF(p: Point) {}\n",
            )],
        );
        let file = file_named(&db, project, "src/Program.mr");
        let point = item_named(&db, project, file, "Point");
        let Signature::Struct(fields) = signature(&db, point) else {
            panic!("not a struct");
        };
        let shape: Vec<_> = fields
            .iter()
            .map(|f| (f.name.as_str(), f.visibility, f.ty.clone(), f.needs_init))
            .collect();
        assert_eq!(
            shape,
            [
                ("X", Visibility::Public, Ty::Primitive(Primitive::Int), true),
                ("Y", Visibility::Private, Ty::Struct(point), true),
                ("Z", Visibility::File, Ty::Primitive(Primitive::Float), true),
                (
                    "W",
                    Visibility::Internal,
                    Ty::Primitive(Primitive::Char),
                    true
                ),
                (
                    "V",
                    Visibility::Folder,
                    Ty::Primitive(Primitive::Bool),
                    false
                ),
            ]
        );
        assert_eq!(
            signature_of(&db, project, "src/Program.mr", "Origin"),
            Signature::Value(Ty::Struct(point))
        );
        assert_eq!(
            function(signature_of(&db, project, "src/Program.mr", "F")).params[0].ty,
            Ty::Struct(point)
        );
        assert_eq!(diagnostics(&db, project), &[]);
    }

    #[test]
    fn later_constructs_are_unsupported_with_their_parts() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "class C {}\nunion U { A }\nenum E { A }\ninterface I {}\nstruct G<T> {}\n\
             F(a: int[], b: int?, c: (int, bool) => string, d: weak C, e: any I, \
             f: C, g: U, h: E, i: I, j: G, k: G<int>, l: S<int>, m: Std.Json.JsonValue, n: $t) {}\n\
             struct S {}\n",
            )],
        );
        let types: Vec<_> = function(signature_of(&db, project, "src/Program.mr", "F"))
            .params
            .iter()
            .map(|param| unsupported(&param.ty))
            .collect();
        assert_eq!(
            types,
            [
                (Construct::Array, 1),
                (Construct::Optional, 1),
                (Construct::Function, 3),
                (Construct::Weak, 1),
                (Construct::Any, 1),
                (Construct::Class, 0),
                (Construct::Union, 0),
                (Construct::Enum, 0),
                (Construct::Interface, 0),
                (Construct::Generic, 0),
                (Construct::Generic, 1),
                (Construct::Generic, 1),
                (Construct::External, 0),
                (Construct::Splice, 0),
            ]
        );
        let class = function(signature_of(&db, project, "src/Program.mr", "F")).params[5]
            .ty
            .clone();
        let file = file_named(&db, project, "src/Program.mr");
        assert_eq!(
            class,
            Ty::Unsupported(Unsupported {
                construct: Construct::Class,
                item: Some(item_named(&db, project, file, "C")),
                parts: Vec::new(),
            })
        );
        for name in ["C", "U", "E", "I"] {
            assert_eq!(
                signature_of(&db, project, "src/Program.mr", name),
                Signature::None
            );
        }
        assert_eq!(diagnostics(&db, project), &[]);
    }

    #[test]
    fn type_parameters_of_the_item_and_its_types_are_in_scope() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "class Box<T> {\n    Get<U>(t: T, u: U, e: T.Enumerator, l: List<T>): V {}\n}\n\
             union Tree<K> {\n    Leaf;\n    Find(k: K) {}\n}\ninterface IMap<Q> {\n    Get(q: Q);\n}\n",
            )],
        );
        let get = function(signature_of(&db, project, "src/Program.mr", "Get"));
        let types: Vec<_> = get.params.iter().map(|p| unsupported(&p.ty)).collect();
        assert_eq!(
            types,
            [
                (Construct::TypeParam, 0),
                (Construct::TypeParam, 0),
                (Construct::TypeParam, 0),
                (Construct::External, 1),
            ]
        );
        // `V` is no type parameter.
        assert_eq!(get.ret, Ty::Error);
        let find = function(signature_of(&db, project, "src/Program.mr", "Find"));
        assert_eq!(unsupported(&find.params[0].ty), (Construct::TypeParam, 0));
        let get = function(signature_of(&db, project, "src/Program.mr", "Get"));
        assert_eq!(get.params.len(), 4);
        assert_eq!(codes(diagnostics(&db, project)), ["MR0127"]);
    }

    #[test]
    fn missing_types_lower_to_the_error_type() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "F(a: , b: int[, c) {}\nstatic X = 1;\nG(a: Std.): () {}\n",
            )],
        );
        let f = function(signature_of(&db, project, "src/Program.mr", "F"));
        assert!(f.params.iter().any(|p| p.ty == Ty::Error), "{f:?}");
        assert_eq!(
            signature_of(&db, project, "src/Program.mr", "X"),
            Signature::Value(Ty::Error)
        );
        // The parser reports these; the signatures add nothing. The parser
        // recovers `[, c` as a top-level statement, whose `c` does not
        // resolve.
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0127"]);
        assert_eq!(reported[0].message, "cannot find `c`");
    }

    #[test]
    fn an_edit_inside_a_body_does_not_recompute_signatures() {
        let mut db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "let x = 1;\n"),
                (
                    "src/Store/UserStore.mr",
                    "internal struct User {\n    public Id: int;\n}\n\
                 internal Find(id: int): User {\n    return new User { Id = id };\n}\n",
                ),
            ],
        );
        let store = file_named(&db, project, "src/Store/UserStore.mr");
        let find = item_named(&db, project, store, "Find");
        let before = signature(&db, find).clone();
        assert_eq!(diagnostics(&db, project), &[]);
        db.take_executed();

        // The edit makes the body longer, so every range after it moves.
        let text = "internal struct User {\n    public Id: int;\n}\n\
                    internal Find(id: int): User {\n    let user = new User { Id = id + 1 };\n    \
                    return user;\n}\n";
        store.set_text(&mut db).to(text.into());
        assert_eq!(diagnostics(&db, project), &[]);
        assert_eq!(signature(&db, find), &before);
        let executed = db.take_executed();
        let ran = |query: &str| {
            executed
                .iter()
                .any(|key| key.starts_with(&format!("{query}(")))
        };
        assert!(ran("lowered"), "{executed:?}");
        assert!(!ran("item_signature"), "{executed:?}");

        // A change to the signature computes it again.
        store
            .set_text(&mut db)
            .to("internal struct User {\n    public Id: int;\n}\n\
         internal Find(id: long): User {\n    return new User { Id = 0 };\n}\n"
                .into());
        let after = function(signature(&db, find).clone());
        assert_eq!(after.params[0].ty, Ty::Primitive(Primitive::Long));
        let executed = db.take_executed();
        assert!(
            executed
                .iter()
                .any(|key| key.starts_with("item_signature(")),
            "{executed:?}"
        );
    }

    #[test]
    fn only_namespace_level_fields_have_a_value_signature() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "class Account {\n    public Balance: long;\n    static Count: int;\n}\n\
                 static Total: long = 0;\n",
            )],
        );
        let file = file_named(&db, project, "src/Program.mr");
        // A class's fields are not items, so they have no signature of their own.
        let names: Vec<_> = item_tree(&db, file)
            .items()
            .map(|(_, item)| item.name.as_str())
            .collect();
        assert_eq!(names, ["Account", "Total"]);
        assert_eq!(
            signature_of(&db, project, "src/Program.mr", "Account"),
            Signature::None
        );
        assert_eq!(
            signature_of(&db, project, "src/Program.mr", "Total"),
            Signature::Value(Ty::Primitive(Primitive::Long))
        );
    }
}
