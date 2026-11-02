use la_arena::{Arena, Idx};
use mora_span::{Db, SourceFile, TextRange, TextSize};
use mora_syntax::ast::{self, AstNode};
use mora_syntax::lexer::ident_name;
use mora_syntax::{Parse, SyntaxKind, SyntaxNode};

use crate::type_ref::{self, SignatureRef, SignatureSource};

/// The syntax tree of a file.
#[salsa::tracked(returns(ref))]
pub fn parse(db: &dyn Db, file: SourceFile) -> Parse {
    mora_syntax::parse(file.text(db))
}

/// Everything one walk over a file's declarations gives. The queries
/// below each keep one part, so a change to one part leaves the others
/// equal.
#[salsa::tracked(returns(ref))]
fn lowered(db: &dyn Db, file: SourceFile) -> Lowered {
    lower(&parse(db, file).tree())
}

/// What a file declares, without positions: an edit that changes no
/// declaration, such as one inside a function body, gives an equal tree.
#[salsa::tracked(returns(ref))]
pub fn item_tree(db: &dyn Db, file: SourceFile) -> ItemTree {
    lowered(db, file).tree.clone()
}

/// Where the parts of [`item_tree`] are in the text, for diagnostics.
#[salsa::tracked(returns(ref))]
pub fn item_source_map(db: &dyn Db, file: SourceFile) -> ItemSourceMap {
    lowered(db, file).map.clone()
}

/// The signatures of a file's items as written, indexed like
/// [`ItemTree::items`], without positions.
#[salsa::tracked(returns(ref))]
pub(crate) fn signature_refs(db: &dyn Db, file: SourceFile) -> Vec<SignatureRef> {
    lowered(db, file).signatures.clone()
}

/// Where the paths of [`signature_refs`] are, for diagnostics.
#[salsa::tracked(returns(ref))]
pub(crate) fn signature_sources(db: &dyn Db, file: SourceFile) -> Vec<SignatureSource> {
    lowered(db, file).signature_sources.clone()
}

/// The parts of a file that [`lower`] builds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Lowered {
    tree: ItemTree,
    map: ItemSourceMap,
    signatures: Vec<SignatureRef>,
    signature_sources: Vec<SignatureSource>,
}

/// An index into [`ItemTree::items`].
pub type ItemIdx = Idx<Item>;

/// The declarations of one file (§4.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemTree {
    /// Every item, nested ones included.
    items: Arena<Item>,
    /// The items declared at namespace level, in the order of the text.
    top_level: Vec<ItemIdx>,
    /// The `using` directives, in the order of the text.
    usings: Vec<Using>,
    /// Whether the file has statements at top level (§4.6).
    has_statements: bool,
}

impl ItemTree {
    pub fn item(&self, idx: ItemIdx) -> &Item {
        &self.items[idx]
    }

    /// Every item with its index, nested ones included.
    pub fn items(&self) -> impl Iterator<Item = (ItemIdx, &Item)> {
        self.items.iter()
    }

    /// The items declared at namespace level, in the order of the text.
    pub fn top_level(&self) -> &[ItemIdx] {
        &self.top_level
    }

    /// The `using` directives, in the order of the text.
    pub fn usings(&self) -> &[Using] {
        &self.usings
    }

    pub fn has_statements(&self) -> bool {
        self.has_statements
    }
}

/// A declaration that has a name: a type, a function, a macro, or a
/// constant or static at namespace level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The declared name, in NFC.
    pub name: String,
    /// What kind of declaration it is.
    pub kind: ItemKind,
    /// The visibility as written; [`Visibility::Folder`] when none is.
    pub visibility: Visibility,
    /// Whether the declaration has the `static` modifier.
    pub is_static: bool,
    /// The type it is declared in, `None` at namespace level.
    pub parent: Option<ItemIdx>,
    /// The nested types and the functions it declares, in the order of the text.
    pub members: Vec<ItemIdx>,
    /// The members that hold or give a value: fields, getters, constants,
    /// enum members and union cases, in the order of the text.
    pub values: Vec<ValueMember>,
}

/// A member of a type that an expression reads as a value (§14.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueMember {
    /// The declared name, in NFC.
    pub name: String,
    /// What kind of member it is.
    pub kind: ValueKind,
    /// The visibility as written; an enum member and a union case are
    /// [`Visibility::Public`], so their type alone decides.
    pub visibility: Visibility,
    /// Whether it belongs to the type rather than to a value of it: a
    /// `static` field or getter, a constant, an enum member or a union case.
    pub is_static: bool,
}

/// What a [`ValueMember`] declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Field,
    Getter,
    Const,
    EnumMember,
    UnionCase,
}

/// What an item declares. `Field` is a `const` or `static` at namespace
/// level (§4.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Struct,
    Class,
    Union,
    Enum,
    Interface,
    Function,
    Macro,
    Field,
}

/// Who can see a declaration (§4.5), narrowest first, so the effective
/// visibility of a nested item is the minimum along its parents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Visibility {
    /// `private`: the enclosing type.
    #[default]
    Private,
    /// `file`: the declaring file.
    File,
    /// No modifier: the declaring folder, that is its namespace (D-97).
    Folder,
    /// `internal`: the project.
    Internal,
    /// `public`: every project that depends on this one.
    Public,
}

/// A `using` directive (§4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Using {
    /// `global using`: it applies to every file of the project.
    pub global: bool,
    /// `using static`: it opens the members of a type.
    pub is_static: bool,
    /// The name of `using Name = ...;`.
    pub alias: Option<String>,
    /// The segments of the path, in NFC, without generic arguments.
    pub path: Vec<String>,
}

/// The text ranges of an [`ItemTree`], in the same order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemSourceMap {
    /// One entry per item, indexed like [`ItemTree::items`].
    items: Vec<ItemSource>,
    /// One entry per `using` directive.
    usings: Vec<UsingSource>,
    /// The first top-level statement.
    first_statement: Option<TextRange>,
}

impl ItemSourceMap {
    pub fn item(&self, idx: ItemIdx) -> &ItemSource {
        &self.items[u32::from(idx.into_raw()) as usize]
    }

    pub fn using(&self, index: usize) -> &UsingSource {
        &self.usings[index]
    }

    pub fn first_statement(&self) -> Option<TextRange> {
        self.first_statement
    }
}

/// Where an item is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSource {
    /// The declared name.
    pub name: TextRange,
    /// Where a modifier goes: the first modifier, or the declaration itself
    /// when it has none. Attributes come before it.
    pub modifiers_start: TextSize,
    /// The visibility modifier, when there is one.
    pub visibility: Option<TextRange>,
    /// The name of each of [`Item::values`], in the same order.
    pub values: Vec<TextRange>,
}

/// Where a `using` directive is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsingSource {
    /// Each segment of the path.
    pub segments: Vec<TextRange>,
}

/// Builds the item tree, the signatures and their source maps in one walk,
/// so their indexes agree.
fn lower(file: &ast::SourceFile) -> Lowered {
    let mut lower = Lowered::default();
    for using in file.using_directives() {
        lower.using(&using);
    }
    for top in file.top_level_items() {
        match top {
            ast::TopLevelItem::Item(item) => {
                if let Some(idx) = lower.item(item.syntax(), None) {
                    lower.tree.top_level.push(idx);
                }
            }
            ast::TopLevelItem::Stmt(ast::Stmt::EmptyStmt(_)) => {}
            ast::TopLevelItem::Stmt(stmt) => {
                lower.tree.has_statements = true;
                if lower.map.first_statement.is_none() {
                    lower.map.first_statement = Some(stmt.syntax().text_range());
                }
            }
        }
    }
    lower
}

/// The parts of a declaration the item tree keeps.
struct Decl {
    kind: ItemKind,
    name: Option<ast::Name>,
    modifiers: Vec<ast::Modifier>,
    members: Vec<ast::Member>,
    values: Vec<(ValueMember, TextRange)>,
}

/// The syntax node of every item of `file`, indexed like
/// [`ItemTree::items`]: the walk is the one [`lower`] makes.
pub(crate) fn item_nodes(file: &ast::SourceFile) -> Vec<SyntaxNode> {
    fn walk(node: &SyntaxNode, member: bool, out: &mut Vec<SyntaxNode>) {
        let Some(decl) = decl(node, member) else {
            return;
        };
        if decl.name.and_then(|name| name.ident_token()).is_none() {
            return;
        }
        out.push(node.clone());
        for member in &decl.members {
            walk(member.syntax(), true, out);
        }
    }
    let mut out = Vec::new();
    for top in file.top_level_items() {
        if let ast::TopLevelItem::Item(item) = top {
            walk(item.syntax(), false, &mut out);
        }
    }
    out
}

impl Lowered {
    fn using(&mut self, using: &ast::UsingDirective) {
        // A directive without a path leaves nothing to resolve; the parser
        // has reported it.
        let tokens: Vec<_> = using
            .path()
            .into_iter()
            .flat_map(|path| path.path_segments())
            .filter_map(|segment| segment.name_ref()?.ident_token())
            .collect();
        if tokens.is_empty() {
            return;
        }
        let segments = tokens
            .iter()
            .map(|token| ident_name(token.text()).into_owned())
            .collect();
        let ranges = tokens.iter().map(|token| token.text_range()).collect();
        let alias = using
            .using_alias()
            .and_then(|alias| alias.name())
            .and_then(|name| name.ident_token())
            .map(|token| ident_name(token.text()).into_owned());
        self.tree.usings.push(Using {
            global: using.global_token().is_some(),
            is_static: using.static_token().is_some(),
            alias,
            path: segments,
        });
        self.map.usings.push(UsingSource { segments: ranges });
    }

    /// Adds the declaration `node` and its nested types and functions.
    /// Declarations without a name, and kinds the tree does not keep, give `None`.
    fn item(&mut self, node: &SyntaxNode, parent: Option<ItemIdx>) -> Option<ItemIdx> {
        let mut decl = decl(node, parent.is_some())?;
        let name = decl.name?.ident_token()?;
        let mut visibility = Visibility::Folder;
        let mut visibility_range = None;
        let mut is_static = false;
        for token in decl.modifiers.iter().filter_map(|modifier| modifier.kind()) {
            if token.kind() == SyntaxKind::StaticKw {
                is_static = true;
                continue;
            }
            let Some(written) = self::visibility(token.kind()) else {
                continue;
            };
            if visibility_range.is_none() {
                visibility = written;
                visibility_range = Some(token.text_range());
            }
        }
        let (values, value_ranges) = std::mem::take(&mut decl.values).into_iter().unzip();
        let idx = self.tree.items.alloc(Item {
            name: ident_name(name.text()).into_owned(),
            kind: decl.kind,
            visibility,
            is_static,
            parent,
            members: Vec::new(),
            values,
        });
        self.map.items.push(ItemSource {
            name: name.text_range(),
            modifiers_start: modifiers_start(node),
            visibility: visibility_range,
            values: value_ranges,
        });
        let (signature, source) = type_ref::signature(node);
        self.signatures.push(signature);
        self.signature_sources.push(source);
        let members: Vec<_> = decl
            .members
            .iter()
            .filter_map(|member| self.item(member.syntax(), Some(idx)))
            .collect();
        self.tree.items[idx].members = members;
        Some(idx)
    }
}

/// The parts of a declaration. Members keep nested types and functions
/// only; namespace level keeps every named item.
fn decl(node: &SyntaxNode, member: bool) -> Option<Decl> {
    let node = node.clone();
    let decl = |kind, name, modifiers: ast::AstChildren<ast::Modifier>, members: Vec<_>| Decl {
        kind,
        name,
        modifiers: modifiers.collect(),
        values: member_values(&members),
        members,
    };
    let members = |list: Option<ast::MemberList>| {
        list.map(|list| list.members().collect())
            .unwrap_or_default()
    };
    if let Some(it) = ast::StructDecl::cast(node.clone()) {
        let list = members(it.member_list());
        return Some(decl(ItemKind::Struct, it.name(), it.modifiers(), list));
    }
    if let Some(it) = ast::ClassDecl::cast(node.clone()) {
        let list = members(it.member_list());
        return Some(decl(ItemKind::Class, it.name(), it.modifiers(), list));
    }
    if let Some(it) = ast::UnionDecl::cast(node.clone()) {
        let body = it.union_body();
        let list = body.iter().flat_map(|body| body.members()).collect();
        let mut union = decl(ItemKind::Union, it.name(), it.modifiers(), list);
        let cases = body.iter().flat_map(|body| body.union_cases());
        let cases = cases.filter_map(|case| case_value(case.name(), ValueKind::UnionCase));
        union.values.splice(0..0, cases);
        return Some(union);
    }
    if let Some(it) = ast::EnumDecl::cast(node.clone()) {
        let mut decl = decl(ItemKind::Enum, it.name(), it.modifiers(), Vec::new());
        let members = it
            .enum_body()
            .into_iter()
            .flat_map(|body| body.enum_members());
        decl.values = members
            .filter_map(|member| case_value(member.name(), ValueKind::EnumMember))
            .collect();
        return Some(decl);
    }
    if let Some(it) = ast::InterfaceDecl::cast(node.clone()) {
        let list = members(it.member_list());
        return Some(decl(ItemKind::Interface, it.name(), it.modifiers(), list));
    }
    if let Some(it) = ast::FunctionDecl::cast(node.clone()) {
        // `IShape.Area(): float => ...` implements an interface member; it
        // declares no name of its own.
        if it.explicit_impl().is_some() {
            return None;
        }
        return Some(decl(
            ItemKind::Function,
            it.name(),
            it.modifiers(),
            Vec::new(),
        ));
    }
    if member {
        return None;
    }
    if let Some(it) = ast::MacroDecl::cast(node.clone()) {
        return Some(decl(ItemKind::Macro, it.name(), it.modifiers(), Vec::new()));
    }
    if let Some(it) = ast::FieldDecl::cast(node) {
        let name = it.variable_declarator().and_then(|v| v.name());
        return Some(decl(ItemKind::Field, name, it.modifiers(), Vec::new()));
    }
    None
}

/// The fields, getters and constants among `members`.
fn member_values(members: &[ast::Member]) -> Vec<(ValueMember, TextRange)> {
    members
        .iter()
        .filter_map(|member| {
            let (name, modifiers, kind) = match member {
                ast::Member::FieldDecl(field) => {
                    let name = field.variable_declarator()?.name();
                    (name, field.modifiers(), ValueKind::Field)
                }
                // `IShape.Name: string => ...` implements an interface
                // member and declares no name of its own.
                ast::Member::GetterDecl(getter) if getter.explicit_impl().is_none() => {
                    (getter.name(), getter.modifiers(), ValueKind::Getter)
                }
                _ => return None,
            };
            let token = name?.ident_token()?;
            let mut value = ValueMember {
                name: ident_name(token.text()).into_owned(),
                kind,
                visibility: Visibility::Folder,
                is_static: false,
            };
            let mut has_visibility = false;
            for token in modifiers.filter_map(|modifier| modifier.kind()) {
                match token.kind() {
                    SyntaxKind::StaticKw => value.is_static = true,
                    SyntaxKind::ConstKw => {
                        value.kind = ValueKind::Const;
                        value.is_static = true;
                    }
                    kind => {
                        if let Some(written) = visibility(kind)
                            && !has_visibility
                        {
                            value.visibility = written;
                            has_visibility = true;
                        }
                    }
                }
            }
            Some((value, token.text_range()))
        })
        .collect()
}

/// An enum member or a union case, which is as visible as its type.
fn case_value(name: Option<ast::Name>, kind: ValueKind) -> Option<(ValueMember, TextRange)> {
    let token = name?.ident_token()?;
    let value = ValueMember {
        name: ident_name(token.text()).into_owned(),
        kind,
        visibility: Visibility::Public,
        is_static: true,
    };
    Some((value, token.text_range()))
}

/// The visibility a modifier keyword writes, if it writes one.
pub(crate) fn visibility(kind: SyntaxKind) -> Option<Visibility> {
    match kind {
        SyntaxKind::PrivateKw => Some(Visibility::Private),
        SyntaxKind::FileKw => Some(Visibility::File),
        SyntaxKind::InternalKw => Some(Visibility::Internal),
        SyntaxKind::PublicKw => Some(Visibility::Public),
        _ => None,
    }
}

/// The start of the first child of a declaration that is not an attribute
/// or trivia: where its modifiers start, or its keyword when it has none.
fn modifiers_start(node: &SyntaxNode) -> TextSize {
    node.children_with_tokens()
        .find(|element| !element.kind().is_trivia() && element.kind() != SyntaxKind::Attribute)
        .map_or(node.text_range().start(), |element| {
            element.text_range().start()
        })
}

#[cfg(test)]
mod tests {

    use crate::test_db::{TestDb, path};
    use crate::*;

    #[test]
    fn item_trees_have_no_positions() {
        let db = TestDb::new();
        let a = db.file("src/A.mr", "class A {\n    F() {}\n}\n");
        let b = db.file("src/B.mr", "\n\n  class A {   F() { let x = 1; }\n}");
        assert_eq!(item_tree(&db, a), item_tree(&db, b));
        assert_ne!(item_source_map(&db, a), item_source_map(&db, b));
    }

    #[test]
    fn item_trees_keep_names_kinds_visibility_and_nesting() {
        let db = TestDb::new();
        let file = db.file(
            "src/A.mr",
            "global using static Std.Math;\nusing Json = Std.Json.JsonValue;\n\
         public struct Point {\n    private class Cache {}\n    static Main() {}\n    X: int;\n}\n\
         file enum Color { Red }\nconst Max: int = 5;\nArea(): int => 1;\nlet y = 2;\n",
        );
        let tree = item_tree(&db, file);
        let summary: Vec<_> = tree
            .items()
            .map(|(_, item)| {
                let parent = item.parent.map(|p| tree.item(p).name.as_str());
                (
                    item.name.as_str(),
                    item.kind,
                    item.visibility,
                    item.is_static,
                    parent,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Point", ItemKind::Struct, Visibility::Public, false, None),
                (
                    "Cache",
                    ItemKind::Class,
                    Visibility::Private,
                    false,
                    Some("Point")
                ),
                (
                    "Main",
                    ItemKind::Function,
                    Visibility::Folder,
                    true,
                    Some("Point")
                ),
                ("Color", ItemKind::Enum, Visibility::File, false, None),
                ("Max", ItemKind::Field, Visibility::Folder, false, None),
                ("Area", ItemKind::Function, Visibility::Folder, false, None),
            ]
        );
        assert_eq!(tree.top_level().len(), 4);
        assert!(tree.has_statements());
        let usings = tree.usings();
        assert_eq!(
            usings[0],
            Using {
                global: true,
                is_static: true,
                alias: None,
                path: path("Std.Math"),
            }
        );
        assert_eq!(usings[1].alias.as_deref(), Some("Json"));
        assert_eq!(usings[1].path, path("Std.Json.JsonValue"));
    }

    #[test]
    fn item_trees_keep_every_kind_of_declaration() {
        let db = TestDb::new();
        let text = "union Shape { Circle(r: float), Square(side: float); \
                    static Unit(): Shape => Circle(1.0); }\n\
                    interface IShape {\n    Area(): float;\n}\n\
                    macro Log(x: Expr) {\n}\n\
                    extension (s: Shape) {\n}\n\
                    class Square {\n    IShape.Area(): float => 1.0;\n    X: int;\n}\n";
        let file = db.file("src/A.mr", text);
        let tree = item_tree(&db, file);
        let summary: Vec<_> = tree
            .items()
            .map(|(_, item)| (item.name.as_str(), item.kind))
            .collect();
        assert_eq!(
            summary,
            [
                ("Shape", ItemKind::Union),
                ("Unit", ItemKind::Function),
                ("IShape", ItemKind::Interface),
                ("Area", ItemKind::Function),
                ("Log", ItemKind::Macro),
                ("Square", ItemKind::Class),
            ]
        );
    }

    #[test]
    fn broken_usings_keep_what_they_name() {
        let db = TestDb::new();
        let file = db.file("src/A.mr", "using;\nusing A.;\nusing B.1;\nusing C;\n");
        let paths: Vec<_> = item_tree(&db, file)
            .usings()
            .iter()
            .map(|using| using.path.join("."))
            .collect();
        assert_eq!(paths, ["A", "B", "C"]);
    }
}
