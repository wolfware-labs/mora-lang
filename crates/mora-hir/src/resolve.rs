use mora_span::{Db, Project, SourceFile};

use crate::defs::{ItemId, NamespaceRef, def_map};
use crate::items::{Item, ItemIdx, ItemKind, ItemTree, ValueMember, Visibility, item_tree};

/// The name of the standard library's root namespace (§4.4).
pub(crate) const STD: &str = "Std";

/// Where a name is looked up from: a file of a project, at namespace level
/// or inside a type of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    /// The project the file belongs to.
    pub project: Project,
    /// The file.
    pub file: SourceFile,
    /// The innermost item the use is inside, `None` at namespace level.
    /// Only types have members, so inside a function is the same as inside
    /// the type around it.
    pub within: Option<ItemIdx>,
}

impl Scope {
    /// Namespace level in `file`.
    pub fn file(project: Project, file: SourceFile) -> Self {
        Self {
            project,
            file,
            within: None,
        }
    }

    /// The item `item` of the scope's file.
    pub(crate) fn item(self, item: ItemIdx) -> ItemId {
        ItemId {
            project: self.project,
            file: self.file,
            item,
        }
    }
}

/// What a path names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Resolved {
    /// A namespace of this project or of a dependency.
    Namespace(NamespaceRef),
    /// A type, a macro, or a constant or static.
    Item(ItemId),
    /// One function or a set of overloads.
    Functions(Vec<ItemId>),
    /// A name under `Std` while the standard library is not loaded, as in
    /// tests that compile files on their own. Nothing is known about it.
    External,
}

/// Why a path does not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveError {
    /// The segment that failed, counting from zero.
    pub segment: usize,
    /// What went wrong there.
    pub kind: ResolveErrorKind,
}

/// What went wrong at the segment of a [`ResolveError`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveErrorKind {
    /// No declaration has the name (`MR0127`). `within` is what the segments
    /// before it name, `None` for the first segment.
    Unresolved { within: Option<Resolved> },
    /// More than one declaration has it at the same level of the lookup (`MR0128`).
    Ambiguous(Vec<Resolved>),
    /// Only declarations that are not visible from the scope have it (`MR0129`).
    NotVisible(ItemId),
}

/// Looks `path` up from `scope`.
///
/// The first segment is looked up in order (§14.5): the members of the
/// enclosing types, from the innermost out; the enclosing namespaces from
/// the innermost out; then the global names, which are the
/// first segment of the project's root namespace, the dependencies' names
/// and `Std`; then what the file's `using` directives and the project's
/// `global using` directives open; then the prelude. The first level that
/// has a visible declaration with the name decides: two there are
/// ambiguous, unless all are functions, which are overloads. Each later
/// segment is looked up in what the path names so far. Locals and
/// parameters come before all of these; they belong to bodies, which have
/// their own lookup.
///
/// While the standard library is not loaded, a name that no level has is
/// [`Resolved::External`] when it is a name of the prelude (§15.1) or the
/// file has a `using` directive that opens a namespace or type of `Std`:
/// nothing can tell whether `Std` declares it.
pub fn resolve_path(db: &dyn Db, scope: Scope, path: &[String]) -> Result<Resolved, ResolveError> {
    resolve(db, scope, path, true)
}

/// Looks up the path of a `using` directive: like [`resolve_path`], but
/// without the levels that `using` directives and the prelude add, so the
/// directives of a project never depend on each other.
pub(crate) fn resolve_using(
    db: &dyn Db,
    scope: Scope,
    path: &[String],
) -> Result<Resolved, ResolveError> {
    resolve(db, scope, path, false)
}

fn resolve(
    db: &dyn Db,
    scope: Scope,
    path: &[String],
    with_imports: bool,
) -> Result<Resolved, ResolveError> {
    let Some((first, rest)) = path.split_first() else {
        return Err(ResolveError {
            segment: 0,
            kind: ResolveErrorKind::Unresolved { within: None },
        });
    };
    let mut current = first_segment(db, scope, first, with_imports)
        .map_err(|kind| ResolveError { segment: 0, kind })?;
    for (index, name) in rest.iter().enumerate() {
        current = member(db, scope, &current, name).map_err(|kind| ResolveError {
            segment: index + 1,
            kind,
        })?;
    }
    Ok(current)
}

/// What a path in an expression names: a declaration, or a value member
/// of a type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ExprTarget {
    /// A declaration, as [`resolve_path`] finds it.
    Item(Resolved),
    /// The value member at this index of [`Item::values`] of the type.
    Value(ItemId, usize),
}

/// Looks a path in an expression up from `scope`, after the locals (§14.5).
/// The members of the enclosing types include their value members, and a
/// type's value members are found after the type's name. Once a segment
/// names a value, the segments after it are member accesses on that value,
/// which type checking looks up; the result gives how many segments name
/// the target.
pub(crate) fn resolve_expr_path(
    db: &dyn Db,
    scope: Scope,
    path: &[String],
) -> Result<(ExprTarget, usize), ResolveError> {
    let Some(first) = path.first() else {
        return Err(ResolveError {
            segment: 0,
            kind: ResolveErrorKind::Unresolved { within: None },
        });
    };
    let tree = item_tree(db, scope.file);
    let mut current = None;
    for ty in enclosing_types(tree, scope.within) {
        let id = scope.item(ty);
        if let Some(index) = tree.item(ty).values.iter().position(|v| v.name == *first) {
            return Ok((ExprTarget::Value(id, index), 1));
        }
        let mut hidden = None;
        let found = pick(db, scope, type_members(db, id, first), &mut hidden)
            .map_err(|kind| ResolveError { segment: 0, kind })?;
        if found.is_some() {
            current = found;
            break;
        }
    }
    let mut current = match current {
        Some(found) => found,
        None => match first_segment_in(db, scope, first, true, true)
            .map_err(|kind| ResolveError { segment: 0, kind })?
        {
            ExprTarget::Item(found) => found,
            value => return Ok((value, 1)),
        },
    };
    for (segment, name) in path.iter().enumerate().skip(1) {
        // After a constant or a static, the segments are member accesses on
        // its value.
        if let Resolved::Item(item) = current
            && item_data(db, item).kind == ItemKind::Field
        {
            return Ok((ExprTarget::Item(current), segment));
        }
        match resolve_static_member(db, scope, &current, name)
            .map_err(|kind| ResolveError { segment, kind })?
        {
            ExprTarget::Item(found) => current = found,
            value => return Ok((value, segment + 1)),
        }
    }
    Ok((ExprTarget::Item(current), path.len()))
}

/// Looks `name` up in what `current` names, as `Color.Red` or
/// `Std.Console` in an expression: a static value member of a type, then
/// what [`resolve_path`] finds after a segment. An instance member is
/// reached through a value, which type checking looks up (§4.3).
pub(crate) fn resolve_static_member(
    db: &dyn Db,
    scope: Scope,
    current: &Resolved,
    name: &str,
) -> Result<ExprTarget, ResolveErrorKind> {
    if let Resolved::Item(ty) = *current {
        let values = &item_data(db, ty).values;
        let found = values.iter().position(|value| {
            value.is_static && value.name == name && value_visible(db, ty, value, scope)
        });
        if let Some(index) = found {
            return Ok(ExprTarget::Value(ty, index));
        }
        // Then its nested types and static functions, as `using static`
        // opens them.
        let mut hidden = None;
        return match pick(db, scope, static_members(db, ty, name), &mut hidden)? {
            Some(found) => Ok(ExprTarget::Item(found)),
            None => Err(not_found(hidden, current)),
        };
    }
    member(db, scope, current, name).map(ExprTarget::Item)
}

/// Whether code in `from` can see the value member `value` of `ty`.
pub(crate) fn value_visible(db: &dyn Db, ty: ItemId, value: &ValueMember, from: Scope) -> bool {
    // A `private` value member is visible inside its type.
    is_visible(db, ty, from) && reaches(db, value.visibility, ty, from, || Some(ty.item))
}

/// The first segment, through the levels of the lookup in order.
fn first_segment(
    db: &dyn Db,
    scope: Scope,
    name: &str,
    with_imports: bool,
) -> Result<Resolved, ResolveErrorKind> {
    match first_segment_in(db, scope, name, with_imports, false)? {
        ExprTarget::Item(found) => Ok(found),
        // Value members are looked for only when `with_values` is set.
        ExprTarget::Value(ty, _) => Ok(Resolved::Item(ty)),
    }
}

/// The first segment through the levels of the lookup. With `with_values`,
/// for an expression, the level of the `using` directives also has the
/// static value members of the types that `using static` opens (§4.3).
fn first_segment_in(
    db: &dyn Db,
    scope: Scope,
    name: &str,
    with_imports: bool,
    with_values: bool,
) -> Result<ExprTarget, ResolveErrorKind> {
    let mut hidden = None;
    let tree = item_tree(db, scope.file);
    for ty in enclosing_types(tree, scope.within) {
        let found = type_members(db, scope.item(ty), name);
        if let Some(found) = pick(db, scope, found, &mut hidden)? {
            return Ok(ExprTarget::Item(found));
        }
    }
    let map = def_map(db, scope.project);
    for id in map.enclosing_namespaces(map.file_namespace(scope.file)) {
        let namespace = NamespaceRef {
            project: scope.project,
            id,
        };
        let found = namespace_members(db, namespace, name, true);
        if let Some(found) = pick(db, scope, found, &mut hidden)? {
            return Ok(ExprTarget::Item(found));
        }
    }
    if let Some(found) = pick(
        db,
        scope,
        global_names(db, scope.project, name),
        &mut hidden,
    )? {
        return Ok(ExprTarget::Item(found));
    }
    if with_imports {
        let imports = imports(db, scope.project, scope.file);
        let mut found = Vec::new();
        for (alias, target) in &imports.aliases {
            if alias == name {
                found.push(target.clone());
            }
        }
        for &namespace in &imports.namespaces {
            found.extend(namespace_members(db, namespace, name, false));
        }
        let mut values = Vec::new();
        for &ty in &imports.types {
            found.extend(static_members(db, ty, name));
            if with_values {
                let members = item_data(db, ty).values.iter().enumerate();
                values.extend(
                    members
                        .filter(|(_, value)| {
                            value.is_static
                                && value.name == name
                                && value_visible(db, ty, value, scope)
                        })
                        .map(|(index, _)| (ty, index)),
                );
            }
        }
        match (values.as_slice(), pick(db, scope, found, &mut hidden)?) {
            ([], Some(found)) => return Ok(ExprTarget::Item(found)),
            ([], None) => {}
            (&[(ty, index)], None) => return Ok(ExprTarget::Value(ty, index)),
            // A value member and anything else with the name at this level
            // are ambiguous; each value is named by its type.
            (values, other) => {
                let mut candidates: Vec<Resolved> = other.into_iter().collect();
                candidates.extend(values.iter().map(|&(ty, _)| Resolved::Item(ty)));
                return Err(ResolveErrorKind::Ambiguous(candidates));
            }
        }
        if let Some(std) = dependency(db, scope.project, STD) {
            let found = namespace_members(db, std, name, false);
            if let Some(found) = pick(db, scope, found, &mut hidden)? {
                return Ok(ExprTarget::Item(found));
            }
        } else if hidden.is_none()
            && !is_std(db, scope.project)
            && (PRELUDE.contains(&name) || imports.opens_external)
        {
            return Ok(ExprTarget::Item(Resolved::External));
        }
    }
    Err(hidden.map_or(
        ResolveErrorKind::Unresolved { within: None },
        ResolveErrorKind::NotVisible,
    ))
}

/// A segment after the first, looked up in what the path names so far.
fn member(
    db: &dyn Db,
    scope: Scope,
    current: &Resolved,
    name: &str,
) -> Result<Resolved, ResolveErrorKind> {
    let found = match current {
        Resolved::Namespace(namespace) => namespace_members(db, *namespace, name, true),
        // Only types have members, so this is empty for any other item.
        Resolved::Item(item) => type_members(db, *item, name),
        Resolved::External => return Ok(Resolved::External),
        Resolved::Functions(_) => Vec::new(),
    };
    let mut hidden = None;
    match pick(db, scope, found, &mut hidden)? {
        Some(found) => Ok(found),
        None => Err(not_found(hidden, current)),
    }
}

/// The error for a name that no visible declaration in what `current`
/// names has: [`ResolveErrorKind::NotVisible`] when a hidden one has it.
fn not_found(hidden: Option<ItemId>, current: &Resolved) -> ResolveErrorKind {
    hidden.map_or_else(
        || ResolveErrorKind::Unresolved {
            within: Some(current.clone()),
        },
        ResolveErrorKind::NotVisible,
    )
}

/// The declarations of one level that are visible from `scope`: `None`
/// when there are none, an error when they are ambiguous. The first
/// declaration that is not visible is kept in `hidden`, for the error when
/// no level has a visible one.
fn pick(
    db: &dyn Db,
    scope: Scope,
    found: Vec<Resolved>,
    hidden: &mut Option<ItemId>,
) -> Result<Option<Resolved>, ResolveErrorKind> {
    let mut functions = Vec::new();
    let mut others: Vec<Resolved> = Vec::new();
    for candidate in found {
        match candidate {
            Resolved::Item(item) if !is_visible(db, item, scope) => {
                hidden.get_or_insert(item);
            }
            Resolved::Item(item) if item_data(db, item).kind == ItemKind::Function => {
                if !functions.contains(&item) {
                    functions.push(item);
                }
            }
            Resolved::Functions(items) => {
                for item in items {
                    if !is_visible(db, item, scope) {
                        hidden.get_or_insert(item);
                    } else if !functions.contains(&item) {
                        functions.push(item);
                    }
                }
            }
            other => {
                if !others.contains(&other) {
                    others.push(other);
                }
            }
        }
    }
    match (others.len(), functions.is_empty()) {
        (0, true) => Ok(None),
        (0, false) => Ok(Some(Resolved::Functions(functions))),
        (1, true) => Ok(others.pop()),
        _ => {
            if !functions.is_empty() {
                others.push(Resolved::Functions(functions));
            }
            Err(ResolveErrorKind::Ambiguous(others))
        }
    }
}

/// The declarations named `name` in a namespace: its items and, when
/// `children` is set, the namespace of a subfolder.
fn namespace_members(
    db: &dyn Db,
    namespace: NamespaceRef,
    name: &str,
    children: bool,
) -> Vec<Resolved> {
    let data = def_map(db, namespace.project).namespace(namespace.id);
    let mut found = Vec::new();
    if children && let Some(&child) = data.children.get(name) {
        found.push(Resolved::Namespace(NamespaceRef {
            project: namespace.project,
            id: child,
        }));
    }
    if let Some(items) = data.items.get(name) {
        found.extend(items.iter().copied().map(Resolved::Item));
    }
    found
}

/// The nested types and functions named `name` in a type; none for an
/// item that is not a type.
fn type_members(db: &dyn Db, ty: ItemId, name: &str) -> Vec<Resolved> {
    let tree = item_tree(db, ty.file);
    tree.item(ty.item)
        .members
        .iter()
        .filter(|&&member| tree.item(member).name == name)
        .map(|&member| Resolved::Item(ItemId { item: member, ..ty }))
        .collect()
}

/// What `using static` opens of a type (§4.3): its nested types and its
/// `static` functions, never its instance methods.
fn static_members(db: &dyn Db, ty: ItemId, name: &str) -> Vec<Resolved> {
    let tree = item_tree(db, ty.file);
    tree.item(ty.item)
        .members
        .iter()
        .filter(|&&member| {
            let data = tree.item(member);
            data.name == name && (data.kind != ItemKind::Function || data.is_static)
        })
        .map(|&member| Resolved::Item(ItemId { item: member, ..ty }))
        .collect()
}

/// The global names that match: the first segment of the project's root
/// namespace, a dependency, or `Std` while the standard library is not
/// loaded and the project is not the standard library itself.
fn global_names(db: &dyn Db, project: Project, name: &str) -> Vec<Resolved> {
    let map = def_map(db, project);
    let mut found = Vec::new();
    if map.namespace(map.outermost()).name == name {
        found.push(Resolved::Namespace(NamespaceRef {
            project,
            id: map.outermost(),
        }));
    }
    if let Some(namespace) = dependency(db, project, name) {
        found.push(Resolved::Namespace(namespace));
    } else if name == STD && found.is_empty() {
        // The standard library's own root namespace is `Std` too.
        found.push(Resolved::External);
    }
    found
}

/// The root namespace of the dependency bound to `name`.
fn dependency(db: &dyn Db, project: Project, name: &str) -> Option<NamespaceRef> {
    project
        .dependencies(db)
        .iter()
        .find(|dependency| dependency.name == name)
        .map(|dependency| NamespaceRef {
            project: dependency.project,
            id: def_map(db, dependency.project).root(),
        })
}

/// What the `using` directives that apply to a file open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Imports {
    /// `using Name = Target;`, by name.
    aliases: Vec<(String, Resolved)>,
    /// `using Namespace;`.
    namespaces: Vec<NamespaceRef>,
    /// `using static Type;`.
    types: Vec<ItemId>,
    /// Whether a `using Namespace;` or `using static Type;` opens a part of
    /// `Std` while the standard library is not loaded.
    opens_external: bool,
}

/// The file's own directives and every `global using` of the project. A
/// directive whose path does not resolve opens nothing; [`crate::diagnostics`]
/// reports it.
#[salsa::tracked(returns(ref))]
pub(crate) fn imports(db: &dyn Db, project: Project, file: SourceFile) -> Imports {
    let mut imports = Imports::default();
    for other in def_map(db, project).files() {
        for using in item_tree(db, other).usings() {
            if other != file && !using.global {
                continue;
            }
            let scope = Scope::file(project, other);
            let Ok(target) = resolve_using(db, scope, &using.path) else {
                continue;
            };
            match (&using.alias, using.is_static, target) {
                (Some(alias), _, target) => imports.aliases.push((alias.clone(), target)),
                (None, false, Resolved::Namespace(namespace)) => {
                    imports.namespaces.push(namespace);
                }
                // An item that is not a type has no members, so it opens nothing.
                (None, true, Resolved::Item(ty)) => imports.types.push(ty),
                (None, _, Resolved::External) => imports.opens_external = true,
                _ => {}
            }
        }
    }
    imports
}

/// What the item tree of its file says about `item`.
pub(crate) fn item_data(db: &dyn Db, item: ItemId) -> &Item {
    item_tree(db, item.file).item(item.item)
}

/// The visibility of an item after its enclosing types narrow it (§4.5).
/// `private` at namespace level, where no type encloses the item, means
/// the declaring file.
pub fn effective_visibility(db: &dyn Db, item: ItemId) -> Visibility {
    let tree = item_tree(db, item.file);
    let mut visibility = Visibility::Public;
    let mut at = Some(item.item);
    while let Some(idx) = at {
        let data = tree.item(idx);
        let own = match (data.visibility, data.parent) {
            (Visibility::Private, None) => Visibility::File,
            (own, _) => own,
        };
        visibility = visibility.min(own);
        at = data.parent;
    }
    visibility
}

/// Whether code in `from` can see `item` (§4.5). A `private` member is
/// visible only inside its type; when several types around it are
/// `private`, inside the type of the innermost one.
pub fn is_visible(db: &dyn Db, item: ItemId, from: Scope) -> bool {
    let owner = || {
        let tree = item_tree(db, item.file);
        enclosing_types(tree, Some(item.item))
            .find(|&idx| tree.item(idx).visibility == Visibility::Private)
            .and_then(|idx| tree.item(idx).parent)
    };
    reaches(db, effective_visibility(db, item), item, from, owner)
}

/// Whether code in `from` is inside what `visibility` gives a declaration
/// of `decl`'s project and file. For `private`, that is inside the type
/// `owner` gives, which is looked up only then.
fn reaches(
    db: &dyn Db,
    visibility: Visibility,
    decl: ItemId,
    from: Scope,
    owner: impl FnOnce() -> Option<ItemIdx>,
) -> bool {
    let same_project = decl.project == from.project;
    match visibility {
        Visibility::Public => true,
        Visibility::Internal => same_project,
        // Another project's code never sees it, so its def map is not read.
        Visibility::Folder => {
            same_project && {
                let map = def_map(db, decl.project);
                map.file_namespace(decl.file) == map.file_namespace(from.file)
            }
        }
        Visibility::File => same_project && decl.file == from.file,
        Visibility::Private => {
            same_project
                && decl.file == from.file
                && owner().is_some_and(|owner| {
                    enclosing_types(item_tree(db, from.file), from.within).any(|ty| ty == owner)
                })
        }
    }
}

/// `start` and the types around it, from the innermost out.
pub(crate) fn enclosing_types(
    tree: &ItemTree,
    start: Option<ItemIdx>,
) -> impl Iterator<Item = ItemIdx> + '_ {
    std::iter::successors(start, |&idx| tree.item(idx).parent)
}

/// Whether `project` is the standard library itself.
fn is_std(db: &dyn Db, project: Project) -> bool {
    project.root_namespace(db) == STD
}

/// The names of the prelude (§15.1) that are types, functions or values,
/// which stand in for it while the standard library is not loaded.
const PRELUDE: &[&str] = &[
    "Action",
    "Clock",
    "Console",
    "Debug",
    "Dictionary",
    "Drop",
    "Environment",
    "Err",
    "Func",
    "Future",
    "HashSet",
    "Html",
    "IClone",
    "IComparable",
    "IDefault",
    "IDisplay",
    "IEnumerable",
    "IEnumerator",
    "IEquatable",
    "IError",
    "IFrom",
    "IFuture",
    "IHashable",
    "IHole",
    "IInterpolate",
    "INumber",
    "IParse",
    "Instant",
    "List",
    "Literal",
    "Log",
    "Math",
    "MathF",
    "MonoTime",
    "Ok",
    "Option",
    "ReadOnlySpan",
    "ReferenceEquals",
    "Report",
    "Result",
    "Secret",
    "Span",
    "Task",
    "TimeSpan",
    "Todo",
    "Trace",
    "Unreachable",
    "Url",
    "panic",
];

/// The names a lookup of a first segment from `scope` could find, or, with
/// `within`, of a later segment in what `within` names: every visible
/// declaration's name, for suggesting one in place of a name that does not
/// resolve. With `with_imports` unset, the names of the levels that `using`
/// directives and the prelude add are left out, as [`resolve_using`] does.
pub(crate) fn visible_names(
    db: &dyn Db,
    scope: Scope,
    within: Option<&Resolved>,
    with_imports: bool,
) -> Vec<String> {
    let mut names = Vec::new();
    let add = |found: Vec<Resolved>, name: &str, names: &mut Vec<String>| {
        let visible = found.iter().any(|candidate| match candidate {
            Resolved::Item(item) => is_visible(db, *item, scope),
            _ => true,
        });
        if visible {
            names.push(name.to_owned());
        }
    };
    let namespace_names = |namespace: NamespaceRef, children: bool| {
        let data = def_map(db, namespace.project).namespace(namespace.id);
        let mut found: Vec<&String> = data.items.keys().collect();
        if children {
            found.extend(data.children.keys());
        }
        found.into_iter().cloned().collect::<Vec<_>>()
    };
    let type_names = |ty: ItemId| {
        let tree = item_tree(db, ty.file);
        tree.item(ty.item)
            .members
            .iter()
            .map(|&member| tree.item(member).name.clone())
            .collect::<Vec<_>>()
    };
    match within {
        Some(Resolved::Namespace(namespace)) => {
            for name in namespace_names(*namespace, true) {
                add(
                    namespace_members(db, *namespace, &name, true),
                    &name,
                    &mut names,
                );
            }
        }
        Some(Resolved::Item(item)) => {
            for name in type_names(*item) {
                add(type_members(db, *item, &name), &name, &mut names);
            }
        }
        Some(Resolved::Functions(_) | Resolved::External) => {}
        None => {
            let tree = item_tree(db, scope.file);
            for ty in enclosing_types(tree, scope.within) {
                let ty = scope.item(ty);
                for name in type_names(ty) {
                    add(type_members(db, ty, &name), &name, &mut names);
                }
            }
            let map = def_map(db, scope.project);
            for id in map.enclosing_namespaces(map.file_namespace(scope.file)) {
                let namespace = NamespaceRef {
                    project: scope.project,
                    id,
                };
                for name in namespace_names(namespace, true) {
                    add(
                        namespace_members(db, namespace, &name, true),
                        &name,
                        &mut names,
                    );
                }
            }
            names.push(map.namespace(map.outermost()).name.clone());
            names.push(STD.to_owned());
            names.extend(
                scope
                    .project
                    .dependencies(db)
                    .iter()
                    .map(|dependency| dependency.name.clone()),
            );
            if with_imports {
                let imports = imports(db, scope.project, scope.file);
                names.extend(imports.aliases.iter().map(|(alias, _)| alias.clone()));
                for &namespace in &imports.namespaces {
                    for name in namespace_names(namespace, false) {
                        add(
                            namespace_members(db, namespace, &name, false),
                            &name,
                            &mut names,
                        );
                    }
                }
                for &ty in &imports.types {
                    for name in type_names(ty) {
                        add(static_members(db, ty, &name), &name, &mut names);
                    }
                }
                match dependency(db, scope.project, STD) {
                    Some(std) => {
                        for name in namespace_names(std, false) {
                            add(namespace_members(db, std, &name, false), &name, &mut names);
                        }
                    }
                    None => names.extend(PRELUDE.iter().map(|&name| name.to_owned())),
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use mora_span::{Dependency, ProjectKind};

    use crate::test_db::{
        TestDb, app, codes, file_named, function, item_named, messages, resolve, signature_of,
        unsupported,
    };
    use crate::*;

    #[test]
    fn visibility_follows_the_folder_by_default() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Store/UserStore.mr",
                    "class UserStore {}\ninternal class Shared {}\nfile class Local {}\n",
                ),
                ("src/Store/Other.mr", ""),
                ("src/Program.mr", ""),
            ],
        );
        let scope = |path: &str| Scope::file(project, file_named(&db, project, path));
        let program = scope("src/Program.mr");
        let other = scope("src/Store/Other.mr");
        let store = scope("src/Store/UserStore.mr");
        let item = |name: &str| item_named(&db, project, store.file, name);
        assert!(is_visible(&db, item("UserStore"), other));
        assert!(!is_visible(&db, item("UserStore"), program));
        assert!(is_visible(&db, item("Shared"), program));
        assert!(is_visible(&db, item("Local"), store));
        assert!(!is_visible(&db, item("Local"), other));
    }

    #[test]
    fn a_nested_item_is_no_more_visible_than_its_type() {
        let db = TestDb::new();
        let project = db.project(
            "a",
            ProjectKind::Lib,
            &[(
                "src/A.mr",
                "class Outer {\n    public class Inner {}\n    private class Secret {}\n}\n\
             public class Open {\n    public class Inner {}\n}\nprivate class Top {}\n",
            )],
            Vec::new(),
        );
        let file = project.files(&db)[0];
        let tree = item_tree(&db, file);
        let inner: Vec<_> = tree
            .items()
            .filter(|(_, item)| item.name == "Inner")
            .map(|(idx, _)| ItemId {
                project,
                file,
                item: idx,
            })
            .collect();
        let item = |name: &str| item_named(&db, project, file, name);
        assert_eq!(effective_visibility(&db, inner[0]), Visibility::Folder);
        assert_eq!(effective_visibility(&db, inner[1]), Visibility::Public);
        assert_eq!(
            effective_visibility(&db, item("Secret")),
            Visibility::Private
        );
        // `private` at namespace level, where no type encloses it, means the file.
        assert_eq!(effective_visibility(&db, item("Top")), Visibility::File);
    }

    #[test]
    fn lookup_goes_from_the_innermost_namespace_out() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "internal class Config {}\ninternal class Root {}\n",
                ),
                ("src/Store/Config.mr", "class Config {}\n"),
                ("src/Store/Cache/Entry.mr", "class Entry {}\n"),
            ],
        );
        let program = "src/Program.mr";
        let ok = |text: &str| Ok::<String, String>(text.into());
        // The innermost namespace wins over the root namespace.
        assert_eq!(
            resolve(&db, project, "src/Store/Config.mr", "Config"),
            ok("UsersApi.Store.Config")
        );
        assert_eq!(
            resolve(&db, project, program, "Config"),
            ok("UsersApi.Config")
        );
        // Enclosing namespaces are searched outward.
        assert_eq!(
            resolve(&db, project, "src/Store/Cache/Entry.mr", "Root"),
            ok("UsersApi.Root")
        );
        // A subfolder is reached through its name, and the root through its own.
        assert_eq!(
            resolve(&db, project, program, "Store.Cache"),
            ok("UsersApi.Store.Cache")
        );
        assert_eq!(
            resolve(&db, project, program, "UsersApi.Store"),
            ok("UsersApi.Store")
        );
        // An unmarked item of another folder is found but not visible.
        assert_eq!(
            resolve(&db, project, program, "Store.Config"),
            Err("hidden: UsersApi.Store.Config".into())
        );
        assert_eq!(
            resolve(&db, project, program, "Store.Nothing"),
            Err("unresolved at 1".into())
        );
        assert_eq!(
            resolve(&db, project, program, "Nothing"),
            Err("unresolved at 0".into())
        );
    }

    #[test]
    fn a_hidden_item_does_not_shadow_a_visible_one_further_out() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "internal class Config {}\n"),
                ("src/Store/Config.mr", "file class Config {}\n"),
                ("src/Store/Use.mr", ""),
            ],
        );
        assert_eq!(
            resolve(&db, project, "src/Store/Use.mr", "Config"),
            Ok("UsersApi.Config".into())
        );
    }

    #[test]
    fn usings_come_after_namespaces_and_globals_and_before_the_prelude() {
        let db = TestDb::new();
        let std = db.project(
            "std",
            ProjectKind::Lib,
            &[
                (
                    "src/Console.mr",
                    "public class Console {}\npublic class Store {}\n",
                ),
                (
                    "src/Collections/List.mr",
                    "public class List {}\npublic class Console {}\n",
                ),
            ],
            Vec::new(),
        );
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[
                ("src/Program.mr", "using Std.Collections;\n"),
                ("src/Store/Thing.mr", "internal class Thing {}\n"),
                ("src/Other.mr", ""),
            ],
            vec![Dependency {
                name: "Std".into(),
                project: std,
            }],
        );
        let ok = |text: &str| Ok::<String, String>(text.into());
        // A `using` opens the namespace's items for its own file.
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "List"),
            ok("Std.Collections.List")
        );
        assert_eq!(
            resolve(&db, project, "src/Other.mr", "List"),
            Err("unresolved at 0".into())
        );
        // The `using` level comes before the prelude: two `Console`s, no ambiguity.
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Console"),
            ok("Std.Collections.Console")
        );
        assert_eq!(
            resolve(&db, project, "src/Other.mr", "Console"),
            ok("Std.Console")
        );
        // Enclosing namespaces come before the prelude: the folder `Store` wins.
        assert_eq!(
            resolve(&db, project, "src/Other.mr", "Store"),
            ok("UsersApi.Store")
        );
        // The dependency's name is global.
        assert_eq!(
            resolve(&db, project, "src/Other.mr", "Std.Collections"),
            ok("Std.Collections")
        );
    }

    #[test]
    fn two_usings_that_open_one_name_are_ambiguous() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using UsersApi.A;\nusing UsersApi.B;\n"),
                (
                    "src/A/Item.mr",
                    "internal class Item {}\ninternal Run() {}\n",
                ),
                (
                    "src/B/Item.mr",
                    "internal class Item {}\ninternal Run(x: int) {}\n",
                ),
            ],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Item"),
            Err("ambiguous: 2".into())
        );
        // Functions with one name are overloads.
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Run"),
            Ok("2 functions".into())
        );
    }

    #[test]
    fn global_usings_and_aliases_apply_to_every_file() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "global using UsersApi.Store;\nglobal using Users = UsersApi.Store.UserStore;\n",
                ),
                ("src/Store/UserStore.mr", "internal class UserStore {}\n"),
                ("src/Api/Handler.mr", ""),
            ],
        );
        let handler = "src/Api/Handler.mr";
        assert_eq!(
            resolve(&db, project, handler, "UserStore"),
            Ok("UsersApi.Store.UserStore".into())
        );
        assert_eq!(
            resolve(&db, project, handler, "Users"),
            Ok("UsersApi.Store.UserStore".into())
        );
    }

    #[test]
    fn std_is_unchecked_without_a_standard_library() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "using Std.Collections;\nusing static Std.Math;\n",
            )],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Std.Anything.At.All"),
            Ok("external".into())
        );
        assert_eq!(diagnostics(&db, project), &[]);
    }

    #[test]
    fn only_public_items_of_a_dependency_are_visible() {
        let db = TestDb::new();
        let core = db.project(
            "core",
            ProjectKind::Lib,
            &[(
                "src/Text/Parser.mr",
                "public class Parser {}\ninternal class Lexer {}\n",
            )],
            Vec::new(),
        );
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[("src/Program.mr", "")],
            vec![Dependency {
                name: "Core".into(),
                project: core,
            }],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Core.Text.Parser"),
            Ok("Core.Text.Parser".into())
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Core.Text.Lexer"),
            Err("hidden: Core.Text.Lexer".into())
        );
    }

    #[test]
    fn using_static_opens_the_nested_types() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using static UsersApi.Store.UserStore;\n"),
                (
                    "src/Store/UserStore.mr",
                    "internal class UserStore {\n    internal class Entry {}\n}\n",
                ),
            ],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Entry"),
            Ok("UsersApi.Store.Entry".into())
        );
    }

    #[test]
    fn using_static_opens_static_functions_but_not_instance_methods() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using static UsersApi.Store.UserStore;\n"),
                (
                    "src/Store/UserStore.mr",
                    "internal class UserStore {\n    internal static Create() {}\n    internal Add() {}\n}\n",
                ),
            ],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Create"),
            Ok("1 functions".into())
        );
        assert!(resolve(&db, project, "src/Program.mr", "Add").is_err());
    }

    #[test]
    fn an_alias_to_overloads_keeps_only_the_visible_ones() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", ""),
                ("src/A/Run.mr", "internal Run() {}\n"),
                (
                    "src/A/Other.mr",
                    "global using R = UsersApi.A.Run;\nglobal using H = UsersApi.A.Hide;\n\
                 file Run(x: int) {}\nfile Hide() {}\n",
                ),
            ],
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "R"),
            Ok("1 functions".into())
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "H"),
            Err("hidden: UsersApi.A.Hide".into())
        );
    }

    #[test]
    fn the_standard_library_names_itself() {
        let db = TestDb::new();
        let std = db.project(
            "std",
            ProjectKind::Lib,
            &[("src/Text/Builder.mr", "public class Builder {}\n")],
            Vec::new(),
        );
        assert_eq!(
            resolve(&db, std, "src/Text/Builder.mr", "Std.Text.Builder"),
            Ok("Std.Text.Builder".into())
        );
    }

    #[test]
    fn an_empty_path_does_not_resolve() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "")]);
        let scope = Scope::file(project, file_named(&db, project, "src/Program.mr"));
        let error = resolve_path(&db, scope, &[]).unwrap_err();
        assert_eq!(error.segment, 0);
        assert_eq!(error.kind, ResolveErrorKind::Unresolved { within: None });
    }

    #[test]
    fn duplicates_in_the_own_namespace_are_ambiguous() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "class A {}\nstruct A {}\n")]);
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "A"),
            Err("ambiguous: 2".into())
        );
    }

    #[test]
    fn usings_that_fail_or_name_the_wrong_kind_open_nothing() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "using UsersApi.Missing;\nusing UsersApi.Store.Box;\nusing static UsersApi.Store;\n",
                ),
                ("src/Store/Box.mr", "internal class Box {}\n"),
            ],
        );
        assert_eq!(codes(diagnostics(&db, project)), ["MR0127"]);
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Box"),
            Err("unresolved at 0".into())
        );
    }

    #[test]
    fn a_private_member_is_visible_inside_the_type_of_its_innermost_private_parent() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "class A {\n    private class B {\n        class C {}\n    }\n    class D {}\n}\nclass E {}\n",
            )],
        );
        let file = file_named(&db, project, "src/Program.mr");
        let item = |name| item_named(&db, project, file, name);
        let inside = |name| Scope {
            project,
            file,
            within: Some(item(name).item),
        };
        let c = item("C");
        assert!(is_visible(&db, c, inside("A")));
        assert!(is_visible(&db, c, inside("B")));
        assert!(is_visible(&db, c, inside("D")));
        assert!(!is_visible(&db, c, inside("E")));
        assert!(!is_visible(&db, c, Scope::file(project, file)));
        let other = app(&db, &[("src/Program.mr", "")]);
        let elsewhere = Scope {
            project: other,
            file,
            within: Some(item("A").item),
        };
        assert!(!is_visible(&db, c, elsewhere));
    }

    #[test]
    fn file_items_are_not_visible_from_another_project() {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", "file class A {}\n")]);
        let file = file_named(&db, project, "src/Program.mr");
        let a = item_named(&db, project, file, "A");
        assert!(is_visible(&db, a, Scope::file(project, file)));
        let other = app(&db, &[]);
        assert!(!is_visible(&db, a, Scope::file(other, file)));
    }

    #[test]
    fn without_the_standard_library_prelude_names_and_usings_of_std_are_external() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "using Std.Json;\nF(a: JsonValue, b: Console, c: Missing) {}\n",
                ),
                (
                    "src/Other.mr",
                    "G(a: JsonValue, b: Console, c: Option<int>, d: Hidden) {}\n",
                ),
                (
                    "src/Store/Hidden.mr",
                    "file class Hidden {}\nclass Console {}\n",
                ),
            ],
        );
        let f = function(signature_of(&db, project, "src/Program.mr", "F"));
        assert_eq!(unsupported(&f.params[0].ty), (Construct::External, 0));
        assert_eq!(unsupported(&f.params[1].ty), (Construct::External, 0));
        assert_eq!(unsupported(&f.params[2].ty), (Construct::External, 0));
        let g = function(signature_of(&db, project, "src/Other.mr", "G"));
        assert_eq!(g.params[0].ty, Ty::Error);
        assert_eq!(unsupported(&g.params[1].ty), (Construct::External, 0));
        assert_eq!(unsupported(&g.params[2].ty), (Construct::External, 1));
        assert_eq!(g.params[3].ty, Ty::Error);
        assert_eq!(
            messages(diagnostics(&db, project)),
            ["cannot find `JsonValue`", "cannot find `Hidden`"]
        );
    }

    #[test]
    fn with_the_standard_library_the_prelude_is_its_root_namespace() {
        let db = TestDb::new();
        let std = db.project(
            "std",
            ProjectKind::Lib,
            &[
                ("src/Console.mr", "public class Console {}\n"),
                ("src/Json/JsonValue.mr", "public struct JsonValue {}\n"),
                (
                    "src/Statics.mr",
                    "public class Math {\n    public static struct Pi {}\n    Hidden() {}\n}\n",
                ),
            ],
            Vec::new(),
        );
        // The standard library itself has no `Std` to stand in for.
        let own = std.files(&db)[0];
        assert!(resolve_path(&db, Scope::file(std, own), &["Option".to_owned()]).is_err());
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[(
                "src/Program.mr",
                "using Std.Json;\nusing J = Std.Json;\nusing static Std.Math;\n\
             F(a: Consol, b: Option, c: JsonValu, d: J.JsonValue, e: Pie) {}\n",
            )],
            vec![Dependency {
                name: "Std".into(),
                project: std,
            }],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(
            messages(reported),
            [
                "cannot find `Consol`",
                "cannot find `Option`",
                "cannot find `JsonValu`",
                "cannot find `Pie`",
            ]
        );
        let fixes: Vec<_> = reported
            .iter()
            .map(|d| d.fixes.first().map(|fix| fix.message.clone()))
            .collect();
        assert_eq!(
            fixes,
            [
                Some("did you mean `Console`?".to_owned()),
                None,
                Some("did you mean `JsonValue`?".to_owned()),
                Some("did you mean `Pi`?".to_owned()),
            ]
        );
    }
}
