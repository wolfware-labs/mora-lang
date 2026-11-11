//! The owners of the bodies of a file, and what each one lowers.

use la_arena::RawIdx;
use mora_span::{TextRange, TextSize};
use mora_syntax::SyntaxNode;
use mora_syntax::ast::{self, AstNode};

use super::lower::Lower;
use super::{BodyKind, BodyOwner, Expr, FileBodies, LocalKind, Lowered};
use crate::items::{ItemIdx, Visibility, Written, item_nodes};
use crate::name::name_of;

/// Lowers every body of `file`: the top-level statements first, then the
/// others in the order of the text.
pub(super) fn file(file: &ast::SourceFile) -> FileBodies {
    let mut found = Vec::new();
    found.push((TextSize::new(0), BodyOwner::TopLevel, top_level(file)));
    // The nodes are indexed like the item tree, so a node's position is its
    // item's index.
    for (raw, node) in (0u32..).zip(item_nodes(file)) {
        item_bodies(&node, ItemIdx::from_raw(RawIdx::from_u32(raw)), &mut found);
    }
    let extensions = file.top_level_items().filter_map(|top| match top {
        ast::TopLevelItem::Item(ast::Item::ExtensionDecl(it)) => Some(it),
        _ => None,
    });
    for (index, extension) in (0u32..).zip(extensions) {
        extension_bodies(&extension, index, &mut found);
    }
    // The top-level statements stay first; the sort is stable.
    found[1..].sort_by_key(|&(start, _, _)| start);
    let mut bodies = FileBodies::default();
    for (_, owner, lowered) in found {
        bodies.owners.push(owner);
        bodies.bodies.insert(owner, lowered);
    }
    bodies
}

type Found = Vec<(TextSize, BodyOwner, Lowered)>;

fn top_level(file: &ast::SourceFile) -> Lowered {
    let mut lower = Lower::new(BodyKind::TopLevel, String::new());
    let mut stmts = Vec::new();
    let mut range: Option<TextRange> = None;
    for top in file.top_level_items() {
        if let ast::TopLevelItem::Stmt(stmt) = top {
            let at = stmt.syntax().text_range();
            range = Some(range.map_or(at, |range| range.cover(at)));
            stmts.push(lower.stmt(&stmt));
        }
    }
    let root = lower.alloc_expr(Expr::Block(stmts), range.unwrap_or_default());
    lower.body.root = Some(root);
    lower.finish()
}

/// What a function-like declaration gives its body.
struct Callable<'a> {
    /// The receiver of an extension member, a parameter before the others.
    receiver: Option<ast::Param<'a>>,
    /// The parameters as written.
    params: Vec<ast::Param<'a>>,
    /// The type parameters it declares, and an extension's.
    type_params: Vec<ast::TypeParamList<'a>>,
    /// Whether its parameter and return types are checked here, because
    /// no signature of the item tree holds them.
    check_types: bool,
    /// Whether the parameters' defaults are lowered here: false for every
    /// accessor of an indexer but the first.
    with_defaults: bool,
    /// The visibility written on the declaration, for `MR0123`.
    visibility: Visibility,
    /// The return type, or the type of a getter or an indexer.
    ret: Option<ast::Type<'a>>,
    /// Whether it is `async`.
    is_async: bool,
    /// Whether it is `static`, for a member the item tree does not hold.
    is_static: bool,
    /// A `set` accessor's implicit `value`, and where it is declared.
    value: Option<TextRange>,
    /// The arguments of a constructor's `: this(...)`.
    this_call: Option<ast::ArgList<'a>>,
    /// The body as a block.
    block: Option<ast::Block<'a>>,
    /// The body as `=> e`.
    expr: Option<ast::ExprBody<'a>>,
}

impl Default for Callable<'_> {
    fn default() -> Self {
        Self {
            receiver: None,
            params: Vec::new(),
            type_params: Vec::new(),
            check_types: false,
            with_defaults: true,
            visibility: Visibility::Private,
            ret: None,
            is_async: false,
            is_static: false,
            value: None,
            this_call: None,
            block: None,
            expr: None,
        }
    }
}

fn callable(kind: BodyKind, name: String, it: Callable) -> Lowered {
    let mut lower = Lower::new(kind, name);
    lower.body.visibility = it.visibility;
    for &list in &it.type_params {
        lower.push_type_params(Some(list));
    }
    // An extension member's receiver is declared without its type and
    // default: the extension's own body, whose parameter it is, checks and
    // lowers those.
    lower.params(
        it.receiver.as_ref(),
        &it.params,
        it.check_types,
        it.with_defaults,
    );
    if let Some(at) = it.value {
        let local = lower.declare_named("value".to_owned(), at, LocalKind::Param, None);
        lower.body.params.push(local);
    }
    if it.check_types
        && let Some(ret) = it.ret
    {
        let ty = lower.ty(Some(ret));
        lower.body.signature_types.push(ty);
        lower.body.ret = Some(ty);
    }
    lower.body.is_async = it.is_async;
    lower.body.is_static = it.is_static;
    if let Some(args) = it.this_call {
        let range = args.syntax().text_range();
        let this = lower.alloc_expr(Expr::This, range);
        let args = lower.args(args.args());
        let call = lower.alloc_expr(Expr::Call { callee: this, args }, range);
        lower.body.this_call = Some(call);
    }
    lower.body.root = lower.function_body(it.block, it.expr);
    lower.finish()
}

/// An initializer, and the type it is checked against when no signature
/// holds it.
fn initializer(
    visibility: Visibility,
    kind: BodyKind,
    name: String,
    ty: Option<ast::Type>,
    init: Option<ast::Expr>,
) -> Lowered {
    let mut lower = Lower::new(kind, name);
    lower.body.visibility = visibility;
    if ty.is_some() {
        let ty = lower.ty(ty);
        lower.body.signature_types.push(ty);
    }
    if let Some(init) = init {
        let range = init.syntax().text_range();
        lower.body.root = Some(lower.expr(Some(init), range));
    }
    lower.finish()
}

/// The interface an explicit implementation names, as written.
fn explicit(name: &str, explicit: Option<ast::ExplicitImpl>) -> String {
    let path = explicit
        .and_then(|it| it.path())
        .map(|path| path.syntax().text());
    match path {
        Some(path) => format!("{path}.{name}"),
        None => name.to_owned(),
    }
}

fn start(node: &SyntaxNode) -> TextSize {
    node.text_range().start()
}

/// The bodies an item of the item tree owns.
fn item_bodies(node: &SyntaxNode, item: ItemIdx, found: &mut Found) {
    if let Some(it) = ast::FunctionDecl::cast(*node) {
        // Its parameter and return types are in its signature.
        let body = callable(
            BodyKind::Function,
            String::new(),
            Callable {
                params: params(it.param_list()),
                block: it.block(),
                expr: it.expr_body(),
                ..Callable::default()
            },
        );
        found.push((start(node), BodyOwner::Item(item), body));
        return;
    }
    if let Some(it) = ast::FieldDecl::cast(*node) {
        // A constant or static at namespace level: its type is in its
        // signature.
        let init = it.variable_declarator().and_then(|v| v.init());
        if init.is_some() {
            let body = initializer(
                Visibility::Private,
                BodyKind::Initializer,
                String::new(),
                None,
                init,
            );
            found.push((start(node), BodyOwner::Item(item), body));
        }
        return;
    }
    if let Some(it) = ast::EnumDecl::cast(*node) {
        let members = it
            .enum_body()
            .into_iter()
            .flat_map(|body| body.enum_members());
        let named = members.filter(|member| member.name().and_then(|n| n.ident_token()).is_some());
        for (index, member) in (0u32..).zip(named) {
            if let Some(value) = member.value() {
                let name = name_of(member.name()).unwrap_or_default();
                let body = initializer(
                    Visibility::Private,
                    BodyKind::Initializer,
                    name,
                    None,
                    Some(value),
                );
                let owner = BodyOwner::Value(item, index);
                found.push((start(member.syntax()), owner, body));
            }
        }
        return;
    }
    let (members, values_before, is_struct) = if let Some(it) = ast::StructDecl::cast(*node) {
        (members(it.member_list()), 0, true)
    } else if let Some(it) = ast::ClassDecl::cast(*node) {
        (members(it.member_list()), 0, false)
    } else if let Some(it) = ast::InterfaceDecl::cast(*node) {
        (members(it.member_list()), 0, false)
    } else if let Some(it) = ast::UnionDecl::cast(*node) {
        let body = it.union_body();
        let named = body
            .iter()
            .flat_map(|body| body.union_cases())
            .filter(|case| case.name().and_then(|n| n.ident_token()).is_some());
        // The cases are the first value members.
        let mut cases = 0;
        for case in named {
            let index = cases;
            cases += 1;
            // A case's fields are typed like parameters, which no signature
            // holds yet.
            let Some(list) = case.param_list() else {
                continue;
            };
            let body = callable(
                BodyKind::Case,
                name_of(case.name()).unwrap_or_default(),
                Callable {
                    visibility: Visibility::Public,
                    params: params(Some(list)),
                    check_types: true,
                    ..Callable::default()
                },
            );
            let owner = BodyOwner::Value(item, index);
            found.push((start(case.syntax()), owner, body));
        }
        let members = body.iter().flat_map(|body| body.members()).collect();
        (members, cases, false)
    } else {
        return;
    };
    member_bodies(&members, item, values_before, is_struct, found);
}

fn members(list: Option<ast::MemberList>) -> Vec<ast::Member> {
    list.map(|list| list.members().collect())
        .unwrap_or_default()
}

fn params(list: Option<ast::ParamList>) -> Vec<ast::Param> {
    list.map(|list| list.params().collect()).unwrap_or_default()
}

/// The bodies of the members of the type `owner`. Value members are
/// numbered like [`crate::Item::values`], so `values_before` counts a
/// union's cases. A struct's field types are in its signature; any other
/// type's are checked here. Methods are items, which [`item_bodies`] lowers;
/// the other members with a body are numbered in the order of the text.
fn member_bodies(
    members: &[ast::Member],
    owner: ItemIdx,
    values_before: u32,
    is_struct: bool,
    found: &mut Found,
) {
    let mut value = values_before;
    let mut member = 0u32;
    for decl in members {
        let at = start(decl.syntax());
        let mut add_member = |found: &mut Found, body: Lowered| {
            found.push((at, BodyOwner::Member(owner, member), body));
            member += 1;
        };
        match decl {
            ast::Member::FieldDecl(it) => {
                let Some(declarator) = it.variable_declarator() else {
                    continue;
                };
                let Some(name) = name_of(declarator.name()) else {
                    continue;
                };
                let ty = if is_struct { None } else { declarator.ty() };
                let init = declarator.init();
                if ty.is_some() || init.is_some() {
                    let body = initializer(
                        Written::of(it.modifiers()).visibility_or_folder(),
                        BodyKind::Initializer,
                        name,
                        ty,
                        init,
                    );
                    found.push((at, BodyOwner::Value(owner, value), body));
                }
                value += 1;
            }
            ast::Member::GetterDecl(it) => {
                let Some(name) = name_of(it.name()) else {
                    continue;
                };
                let explicit_impl = it.explicit_impl();
                let is_value = explicit_impl.is_none();
                let body = callable(
                    BodyKind::Getter,
                    explicit(&name, explicit_impl),
                    Callable {
                        visibility: if is_value {
                            Written::of(it.modifiers()).visibility_or_folder()
                        } else {
                            Visibility::Private
                        },
                        check_types: true,
                        ret: it.ty(),
                        is_static: Written::of(it.modifiers()).is_static,
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                );
                if is_value {
                    found.push((at, BodyOwner::Value(owner, value), body));
                    value += 1;
                } else {
                    add_member(found, body);
                }
            }
            // A method is an item, unless it implements an interface member.
            ast::Member::FunctionDecl(it) if it.explicit_impl().is_some() => {
                let name = name_of(it.name()).unwrap_or_default();
                let body = callable(
                    BodyKind::Function,
                    explicit(&name, it.explicit_impl()),
                    Callable {
                        params: params(it.param_list()),
                        type_params: it.type_param_list().into_iter().collect(),
                        check_types: true,
                        ret: it.ret(),
                        is_async: Written::of(it.modifiers()).is_async,
                        is_static: Written::of(it.modifiers()).is_static,
                        block: it.block(),
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                );
                add_member(found, body);
            }
            ast::Member::ConstructorDecl(it) => {
                let body = callable(
                    BodyKind::Constructor,
                    name_of(it.name()).unwrap_or_default(),
                    Callable {
                        visibility: Written::of(it.modifiers()).visibility_or_folder(),
                        params: params(it.param_list()),
                        check_types: true,
                        this_call: it
                            .constructor_initializer()
                            .and_then(|init| init.arg_list()),
                        block: it.block(),
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                );
                add_member(found, body);
            }
            ast::Member::DestructorDecl(it) => {
                let body = callable(
                    BodyKind::Destructor,
                    name_of(it.name()).unwrap_or_default(),
                    Callable {
                        block: it.block(),
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                );
                add_member(found, body);
            }
            ast::Member::OperatorDecl(it) => {
                let op = it.op().map(|op| op.text().to_owned()).unwrap_or_default();
                let body = callable(
                    BodyKind::Operator,
                    op,
                    Callable {
                        visibility: Written::of(it.modifiers()).visibility_or_folder(),
                        params: params(it.param_list()),
                        check_types: true,
                        ret: it.ret(),
                        // An operator is static whether or not `static` is
                        // written (§6.9).
                        is_static: true,
                        block: it.block(),
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                );
                add_member(found, body);
            }
            ast::Member::IndexerDecl(it) => {
                let name = explicit("this[]", it.explicit_impl());
                let visibility = Written::of(it.modifiers()).visibility_or_folder();
                let indexer_params: Vec<_> = it.params().collect();
                let expr = it.expr_body();
                let accessors: Vec<_> = it
                    .accessor_list()
                    .into_iter()
                    .flat_map(|list| list.accessors())
                    .collect();
                // An expression body is the getter; an indexer with neither
                // it nor accessors still has its types checked.
                if expr.is_some() || accessors.is_empty() {
                    let body = callable(
                        BodyKind::Get,
                        name.clone(),
                        Callable {
                            visibility,
                            params: indexer_params.clone(),
                            check_types: true,
                            ret: it.ty(),
                            expr,
                            ..Callable::default()
                        },
                    );
                    add_member(found, body);
                }
                for (index, accessor) in accessors.into_iter().enumerate() {
                    let keyword = accessor.kind();
                    let is_set = keyword.as_ref().is_some_and(|kw| kw.text() == "set");
                    let at = keyword.map_or(accessor.syntax().text_range(), |kw| kw.text_range());
                    // The types are checked once, with the first accessor.
                    let check_types = index == 0;
                    let body = callable(
                        if is_set { BodyKind::Set } else { BodyKind::Get },
                        name.clone(),
                        Callable {
                            visibility,
                            params: indexer_params.clone(),
                            check_types,
                            with_defaults: check_types,
                            ret: if check_types { it.ty() } else { None },
                            value: is_set.then_some(at),
                            block: accessor.block(),
                            expr: accessor.expr_body(),
                            ..Callable::default()
                        },
                    );
                    found.push((
                        start(accessor.syntax()),
                        BodyOwner::Member(owner, member),
                        body,
                    ));
                    member += 1;
                }
            }
            _ => {}
        }
    }
}

/// The bodies of the `index`th extension block of the file (§6.9): the
/// block itself, which checks the receiver's type, as member 0, then each
/// member, which sees the receiver as its first parameter.
fn extension_bodies(extension: &ast::ExtensionDecl, index: u32, found: &mut Found) {
    let receiver = extension.param();
    let own: Vec<_> = extension.type_param_list().into_iter().collect();
    let header = callable(
        BodyKind::Extension,
        "extension".to_owned(),
        Callable {
            visibility: Written::of(extension.modifiers()).visibility_or_folder(),
            params: receiver.into_iter().collect(),
            type_params: own.clone(),
            check_types: true,
            ..Callable::default()
        },
    );
    found.push((
        start(extension.syntax()),
        BodyOwner::Extension(index, 0),
        header,
    ));
    let mut member = 1u32;
    for decl in members(extension.member_list()) {
        let at = start(decl.syntax());
        let mut type_params = own.clone();
        let body = match &decl {
            ast::Member::FunctionDecl(it) => {
                type_params.extend(it.type_param_list());
                let written = Written::of(it.modifiers());
                callable(
                    BodyKind::Function,
                    name_of(it.name()).unwrap_or_default(),
                    Callable {
                        visibility: written.visibility_or_folder(),
                        receiver,
                        params: params(it.param_list()),
                        type_params,
                        check_types: true,
                        ret: it.ret(),
                        is_async: written.is_async,
                        block: it.block(),
                        expr: it.expr_body(),
                        ..Callable::default()
                    },
                )
            }
            ast::Member::GetterDecl(it) => callable(
                BodyKind::Getter,
                name_of(it.name()).unwrap_or_default(),
                Callable {
                    visibility: Written::of(it.modifiers()).visibility_or_folder(),
                    receiver,
                    type_params,
                    check_types: true,
                    ret: it.ty(),
                    expr: it.expr_body(),
                    ..Callable::default()
                },
            ),
            _ => continue,
        };
        found.push((at, BodyOwner::Extension(index, member), body));
        member += 1;
    }
}
