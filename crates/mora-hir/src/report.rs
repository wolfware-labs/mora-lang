use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Db, Project, ProjectKind, SourceFile, Span, TextRange, TextSize};

use crate::bodies::{
    BodyError, LocalKind, body, body_owners, body_resolution, body_scope, body_source_map,
};
use crate::defs::{DefMap, FolderProblem, ItemId, def_map};
use crate::items::{
    ItemKind, Visibility, item_index, item_source_map, item_tree, signature_sources,
};
use crate::resolve::{
    ResolveErrorKind, Resolved, STD, Scope, effective_visibility, enclosing_types, item_data,
    resolve_using, value_visible, visible_names,
};
use crate::signature::{SignatureError, SignatureErrorKind, signature, signature_errors};
use crate::suggest::closest;
use crate::ty::Signature;
use crate::type_ref::type_keywords;

/// The diagnostics of a project's folders (`MR0121`), the paths in its
/// `using` directives, signatures and bodies (`MR0127`, `MR0128`, `MR0129`,
/// `MR0123`), the member names of its types (`MR0146`), the locals of its
/// bodies (`MR0180`) and its entry point (`MR0124`, `MR0125`). Lexical and
/// syntax errors come from [`crate::parse`].
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, project: Project) -> Vec<Diagnostic> {
    let map = def_map(db, project);
    let mut out = Vec::new();
    for problem in map.folder_problems() {
        out.push(folder(db, problem));
    }
    for file in map.files() {
        usings(db, project, file, &mut out);
        signatures(db, project, file, &mut out);
        member_names(db, file, &mut out);
        out.extend(body_diagnostics(db, project, file).iter().cloned());
    }
    entry_point(db, project, map, &mut out);
    out
}

/// The start of a file: where a diagnostic about the file as a whole points.
fn file_start(db: &dyn Db, file: SourceFile) -> Span {
    Span::new(file.id(db), TextRange::empty(TextSize::new(0)))
}

fn folder(db: &dyn Db, problem: &FolderProblem) -> Diagnostic {
    match problem {
        FolderProblem::Invalid { folder, file } => {
            let name = folder.rsplit('/').next().unwrap_or(folder);
            Diagnostic::error(
                codes::MR0121,
                format!("the folder `{folder}` does not give a namespace name"),
                Label::new(file_start(db, *file), format!("this file is in `{folder}`")),
            )
            .with_note(format!(
                "a folder's name in PascalCase names its namespace, and `{name}` does not give an identifier"
            ))
        }
        FolderProblem::Clash {
            folder,
            other,
            namespace,
            file,
            other_file,
        } => Diagnostic::error(
            codes::MR0121,
            format!("the folders `{other}` and `{folder}` both give the namespace `{namespace}`"),
            Label::new(file_start(db, *file), format!("this file is in `{folder}`")),
        )
        .with_secondary(Label::new(
            file_start(db, *other_file),
            format!("this file is in `{other}`"),
        )),
    }
}

/// The errors in the paths of a file's `using` directives.
fn usings(db: &dyn Db, project: Project, file: SourceFile, out: &mut Vec<Diagnostic>) {
    let tree = item_tree(db, file);
    let map = item_source_map(db, file);
    let scope = Scope::file(project, file);
    for (index, using) in tree.usings().iter().enumerate() {
        let Err(error) = resolve_using(db, scope, &using.path) else {
            continue;
        };
        let name = &using.path[error.segment];
        let span = Span::new(file.id(db), map.using(index).segments[error.segment]);
        out.push(resolve_error(
            db,
            scope,
            name,
            span,
            error.kind,
            Lookup::Using,
        ));
    }
}

/// The errors in the types of the signatures of a file's items.
fn signatures(db: &dyn Db, project: Project, file: SourceFile, out: &mut Vec<Diagnostic>) {
    let sources = signature_sources(db, file);
    for (idx, _) in item_tree(db, file).items() {
        let item = Scope::file(project, file).item(idx);
        let source = &sources[item_index(idx)];
        for error in signature_errors(db, item) {
            let range = source.paths[error.path][error.segment];
            let span = Span::new(file.id(db), range);
            let member = |field: Option<usize>| match (field, signature(db, item)) {
                (Some(field), Signature::Struct(fields)) => fields[field].name.clone(),
                _ => item_data(db, item).name.clone(),
            };
            out.push(signature_error(db, error, span, member));
        }
    }
}

/// The diagnostic for a path in a type that is wrong. `member` names what
/// has the type, from the field it is on, for `MR0123`.
fn signature_error(
    db: &dyn Db,
    error: &SignatureError,
    span: Span,
    member: impl FnOnce(Option<usize>) -> String,
) -> Diagnostic {
    let name = &error.name;
    match &error.kind {
        SignatureErrorKind::Resolve(scope, kind) => {
            resolve_error(db, *scope, name, span, kind.clone(), Lookup::Type)
        }
        SignatureErrorKind::NotAType(resolved) => not_a_type(db, name, span, resolved),
        SignatureErrorKind::LessVisible {
            ty,
            field,
            member: visibility,
        } => less_visible(db, name, span, *ty, &member(*field), *visibility),
    }
}

/// `MR0127` for a path in a type position that names something else.
fn not_a_type(db: &dyn Db, name: &str, span: Span, resolved: &Resolved) -> Diagnostic {
    // A namespace, an item or overloads, which all have a name.
    let full = full_name(db, resolved).unwrap_or_default();
    let what = format!("{} `{full}`", kind_name(db, resolved));
    Diagnostic::error(
        codes::MR0127,
        format!("`{name}` is not a type"),
        Label::new(span, format!("this is {what}")),
    )
}

/// The errors in the bodies of a file: `MR0180`, and names and types that
/// do not resolve. A file whose bodies did not change keeps its result.
#[salsa::tracked(returns(ref))]
fn body_diagnostics(db: &dyn Db, project: Project, file: SourceFile) -> Vec<Diagnostic> {
    let id = file.id(db);
    let tree = item_tree(db, file);
    let mut out = Vec::new();
    for owner in body_owners(db, file) {
        let body = body(db, file, owner);
        let map = body_source_map(db, file, owner);
        let scope = body_scope(project, file, owner);
        for &(local, earlier) in &body.shadows {
            let name = &body.locals[local].name;
            let what = match body.locals[earlier].kind {
                LocalKind::Param => "parameter",
                _ => "local",
            };
            out.push(
                Diagnostic::error(
                    codes::MR0180,
                    format!("`{name}` is already the name of a {what} in scope"),
                    Label::new(Span::new(id, map.local(local)), "declared again here"),
                )
                .with_secondary(Label::new(
                    Span::new(id, map.local(earlier)),
                    format!("the {what} `{name}` is declared here"),
                ))
                .with_note(
                    "a local may not shadow another; a name can be used again once the first is out of scope",
                ),
            );
        }
        for error in &body_resolution(db, project, file, owner).errors {
            out.push(match error {
                BodyError::Path {
                    expr,
                    scope: at,
                    segment,
                    name,
                    error,
                } => {
                    let span = Span::new(id, map.path_segments[expr][*segment]);
                    let mut extra: Vec<String> = Vec::new();
                    if *segment == 0 {
                        extra.extend(
                            body.locals_in_scope(*at)
                                .map(|local| body.locals[local].name.clone()),
                        );
                        for ty in enclosing_types(tree, scope.within) {
                            extra.extend(tree.item(ty).values.iter().map(|v| v.name.clone()));
                        }
                    }
                    resolve_error(db, scope, name, span, error.clone(), Lookup::Expr(extra))
                }
                BodyError::Member {
                    expr,
                    name,
                    within,
                    error,
                } => {
                    let span = Span::new(id, map.member_names[expr]);
                    let mut extra = Vec::new();
                    if let Resolved::Item(ty) = within {
                        let values = &item_data(db, *ty).values;
                        extra.extend(
                            values
                                .iter()
                                .filter(|value| value_visible(db, *ty, value, scope))
                                .map(|value| value.name.clone()),
                        );
                    }
                    let error = match error {
                        // The lookup started at `within`, which the message names.
                        ResolveErrorKind::Unresolved { .. } => ResolveErrorKind::Unresolved {
                            within: Some(within.clone()),
                        },
                        other => other.clone(),
                    };
                    resolve_error(db, scope, name, span, error, Lookup::Expr(extra))
                }
                BodyError::Type(error) => {
                    let span = Span::new(id, map.type_paths[error.path][error.segment]);
                    signature_error(db, error, span, |_| body.name.clone())
                }
                BodyError::Capture { expr, index, name } => {
                    let span = Span::new(id, map.captures[&(*expr, *index)]);
                    Diagnostic::error(
                        codes::MR0127,
                        format!("cannot find the local `{name}`"),
                        Label::new(span, "no local of this name is in scope"),
                    )
                    .with_note("a capture list names locals or parameters of the enclosing code, or `this`")
                }
            });
        }
    }
    // Each body reports its errors by kind; the file reads them in order.
    out.sort_by_key(|diagnostic| diagnostic.primary.span.range.start());
    out
}

/// Where a name that does not resolve was looked up, which decides what is
/// suggested in its place.
enum Lookup {
    /// A `using` directive, without what directives open.
    Using,
    /// A type, which also suggests the type keywords.
    Type,
    /// An expression, which also suggests these names: the locals in scope
    /// and the value members of the enclosing types.
    Expr(Vec<String>),
}

/// `MR0146`: two members of one type with one name, unless both are
/// methods. The members are the nested types, the methods and the value
/// members (§14.5); constructors, operators and indexers have no name.
fn member_names(db: &dyn Db, file: SourceFile, out: &mut Vec<Diagnostic>) {
    let tree = item_tree(db, file);
    let map = item_source_map(db, file);
    let id = file.id(db);
    for (idx, item) in tree.items() {
        // Each member's name, where it is, and whether it is a method.
        let mut members: Vec<(&str, TextRange, bool)> = item
            .members
            .iter()
            .map(|&member| {
                let data = tree.item(member);
                let range = map.item(member).name;
                (data.name.as_str(), range, data.kind == ItemKind::Function)
            })
            .collect();
        members.extend(
            item.values
                .iter()
                .zip(&map.item(idx).values)
                .map(|(value, &range)| (value.name.as_str(), range, false)),
        );
        members.sort_by_key(|&(_, range, _)| range.start());
        for (index, &(name, range, method)) in members.iter().enumerate() {
            let first = members[..index]
                .iter()
                .find(|&&(other, _, other_method)| other == name && !(method && other_method));
            let Some(&(_, first, _)) = first else {
                continue;
            };
            out.push(
                Diagnostic::error(
                    codes::MR0146,
                    format!("`{name}` is already declared in `{}`", item.name),
                    Label::new(Span::new(id, range), "declared again here"),
                )
                .with_secondary(Label::new(Span::new(id, first), "first declared here"))
                .with_note(
                    "the members of a type share one set of names; only methods may share a name, as overloads",
                ),
            );
        }
    }
}

/// The diagnostic for a path that does not resolve from `scope`. A type
/// position also suggests the keywords that name types.
fn resolve_error(
    db: &dyn Db,
    scope: Scope,
    name: &str,
    span: Span,
    kind: ResolveErrorKind,
    lookup: Lookup,
) -> Diagnostic {
    match kind {
        ResolveErrorKind::Unresolved { within } => {
            let with_imports = !matches!(lookup, Lookup::Using);
            let mut candidates = visible_names(db, scope, within.as_ref(), with_imports);
            match lookup {
                Lookup::Type if within.is_none() => {
                    candidates.extend(type_keywords().map(str::to_owned));
                }
                Lookup::Expr(extra) => candidates.extend(extra),
                _ => {}
            }
            candidates.sort();
            candidates.dedup();
            let suggestion = closest(name, &candidates);
            unresolved(db, name, span, within, suggestion)
        }
        ResolveErrorKind::Ambiguous(candidates) => ambiguous(db, name, span, &candidates),
        ResolveErrorKind::NotVisible(item) => not_visible(db, scope, name, span, item),
    }
}

fn unresolved(
    db: &dyn Db,
    name: &str,
    span: Span,
    within: Option<Resolved>,
    suggestion: Option<&str>,
) -> Diagnostic {
    let message = match within.as_ref().and_then(|within| full_name(db, within)) {
        Some(within) => format!("cannot find `{name}` in `{within}`"),
        None => format!("cannot find `{name}`"),
    };
    let diagnostic = Diagnostic::error(codes::MR0127, message, Label::new(span, "not found"));
    match suggestion {
        Some(suggestion) => diagnostic.with_fix(Fix::new(
            format!("did you mean `{suggestion}`?"),
            vec![TextEdit::replace(span, suggestion)],
        )),
        None => diagnostic,
    }
}

/// `MR0123`: the signature of `member` mentions the type `ty`, which is
/// visible in less of the program than the member.
fn less_visible(
    db: &dyn Db,
    name: &str,
    span: Span,
    ty: ItemId,
    member_name: &str,
    member: Visibility,
) -> Diagnostic {
    let region = |visibility| match visibility {
        Visibility::Public => "everywhere",
        Visibility::Internal => "in its project",
        Visibility::Folder => "in its folder",
        Visibility::File => "in its file",
        Visibility::Private => "inside its type",
    };
    let narrower = effective_visibility(db, ty);
    let mut diagnostic = Diagnostic::error(
        codes::MR0123,
        format!(
            "`{member_name}` is visible {}, but its signature mentions `{name}`, which is visible only {}",
            region(member),
            region(narrower),
        ),
        Label::new(span, format!("less visible than `{member_name}`")),
    )
    .with_secondary(Label::new(item_name_span(db, ty), "declared here"))
    .with_note(
        "code that can use a member must be able to name the types in its signature; \
         make the type more visible or the member less visible",
    );
    // Marking the type helps only when the types around it are visible enough.
    let tree = item_tree(db, ty.file);
    let parents_allow = enclosing_types(tree, tree.item(ty.item).parent)
        .all(|parent| tree.item(parent).visibility >= member);
    let keyword = match member {
        Visibility::Public => Some("public"),
        Visibility::Internal => Some("internal"),
        _ => None,
    };
    if let Some(keyword) = keyword.filter(|_| parents_allow) {
        diagnostic = diagnostic.with_fix(Fix::new(
            format!("mark `{name}` `{keyword}`"),
            vec![widen(db, ty, keyword)],
        ));
    }
    diagnostic
}

fn ambiguous(db: &dyn Db, name: &str, span: Span, candidates: &[Resolved]) -> Diagnostic {
    let names: Vec<Option<String>> = candidates
        .iter()
        .map(|candidate| full_name(db, candidate))
        .collect();
    let described: Vec<String> = candidates
        .iter()
        .zip(&names)
        .filter_map(|(candidate, name)| {
            Some(format!("{} `{}`", kind_name(db, candidate), name.as_ref()?))
        })
        .collect();
    let remedy = if names.windows(2).all(|pair| pair[0] == pair[1]) {
        "rename one of them"
    } else {
        "write more of the path to choose one"
    };
    let note = format!("it could be {}; {remedy}", described.join(" or "));
    // An overload set is one candidate but several declarations.
    let declarations: usize = candidates
        .iter()
        .map(|candidate| match candidate {
            Resolved::Functions(items) => items.len(),
            _ => 1,
        })
        .sum();
    let mut diagnostic = Diagnostic::error(
        codes::MR0128,
        format!("`{name}` is ambiguous"),
        Label::new(span, format!("`{name}` names {declarations} declarations")),
    )
    .with_note(note);
    for candidate in candidates {
        let items = match candidate {
            Resolved::Item(item) => std::slice::from_ref(item),
            Resolved::Functions(items) => items.as_slice(),
            Resolved::Namespace(_) | Resolved::External => &[],
        };
        for &item in items {
            diagnostic =
                diagnostic.with_secondary(Label::new(item_name_span(db, item), "declared here"));
        }
    }
    diagnostic
}

fn not_visible(db: &dyn Db, scope: Scope, name: &str, span: Span, item: ItemId) -> Diagnostic {
    let declared_in = file_namespace(db, item.project, item.file).unwrap_or_default();
    let used_in = file_namespace(db, scope.project, scope.file).unwrap_or_default();
    let same_project = item.project == scope.project;
    let (message, note) = match effective_visibility(db, item) {
        _ if !same_project => (
            format!("`{name}` is not public, so `{used_in}` cannot see it"),
            format!(
                "only `public` declarations of `{declared_in}` are visible to the projects that depend on it"
            ),
        ),
        Visibility::Folder => (
            format!("`{name}` is visible only in `{declared_in}`, not in `{used_in}`"),
            format!(
                "a declaration without a visibility modifier is visible only in its folder; mark it `internal` to use it from `{used_in}`"
            ),
        ),
        Visibility::File => (
            format!("`{name}` is visible only in `{}`", item.file.path(db)),
            "a `file` declaration is visible only in the file that declares it".to_owned(),
        ),
        Visibility::Private | Visibility::Internal | Visibility::Public => (
            format!("`{name}` is private to the type that declares it"),
            "a `private` member is visible only inside its type".to_owned(),
        ),
    };
    let keyword = if same_project { "internal" } else { "public" };
    Diagnostic::error(codes::MR0129, message, Label::new(span, "not visible here"))
        .with_secondary(Label::new(item_name_span(db, item), "declared here"))
        .with_note(note)
        .with_fix(Fix::new(
            format!("mark `{name}` `{keyword}`"),
            vec![widen(db, item, keyword)],
        ))
}

/// The edit that gives `item` the visibility modifier `keyword`. Only the
/// item needs it: a type around it that the use cannot see fails the lookup
/// first.
fn widen(db: &dyn Db, item: ItemId, keyword: &str) -> TextEdit {
    let source = item_source_map(db, item.file).item(item.item);
    let file = item.file.id(db);
    match source.visibility {
        Some(range) => TextEdit::replace(Span::new(file, range), keyword),
        None => TextEdit::insert(file, source.modifiers_start, format!("{keyword} ")),
    }
}

fn item_name_span(db: &dyn Db, item: ItemId) -> Span {
    Span::new(
        item.file.id(db),
        item_source_map(db, item.file).item(item.item).name,
    )
}

/// What kind of declaration a path resolved to, for messages.
pub(crate) fn kind_name(db: &dyn Db, resolved: &Resolved) -> &'static str {
    let kind = match resolved {
        Resolved::Namespace(_) | Resolved::External => return "the namespace",
        Resolved::Functions(_) => ItemKind::Function,
        Resolved::Item(item) => item_data(db, *item).kind,
    };
    match kind {
        ItemKind::Struct => "the struct",
        ItemKind::Class => "the class",
        ItemKind::Union => "the union",
        ItemKind::Enum => "the enum",
        ItemKind::Interface => "the interface",
        ItemKind::Function => "the function",
        ItemKind::Macro => "the macro",
        ItemKind::Field => "the value",
    }
}

/// The full name of what a path resolved to, such as `UsersApi.Store.UserStore`.
pub(crate) fn full_name(db: &dyn Db, resolved: &Resolved) -> Option<String> {
    match resolved {
        Resolved::Namespace(namespace) => {
            Some(def_map(db, namespace.project).namespace_path(namespace.id))
        }
        Resolved::Item(item) => Some(item_full_name(db, *item)),
        Resolved::Functions(items) => items.first().map(|&item| item_full_name(db, item)),
        Resolved::External => Some(STD.to_owned()),
    }
}

pub(crate) fn item_full_name(db: &dyn Db, item: ItemId) -> String {
    let tree = item_tree(db, item.file);
    let mut segments: Vec<String> = enclosing_types(tree, Some(item.item))
        .map(|idx| tree.item(idx).name.clone())
        .collect();
    segments.extend(file_namespace(db, item.project, item.file));
    segments.reverse();
    segments.join(".")
}

/// The full name of the namespace of `file`, `None` when it is not a file
/// of `project`.
fn file_namespace(db: &dyn Db, project: Project, file: SourceFile) -> Option<String> {
    let map = def_map(db, project);
    map.file_namespace(file).map(|id| map.namespace_path(id))
}

/// `MR0124` and `MR0125` (§4.6).
fn entry_point(db: &dyn Db, project: Project, map: &DefMap, out: &mut Vec<Diagnostic>) {
    let entry = project.entry(db);
    let first_statement = |file: SourceFile| {
        let range = item_source_map(db, file)
            .first_statement()
            .unwrap_or_default();
        Span::new(file.id(db), range)
    };
    for &file in map.statement_files() {
        let message = match project.kind(db) {
            ProjectKind::Lib => "a library cannot have top-level statements".to_owned(),
            ProjectKind::App if Some(file) == map.entry_file() => continue,
            ProjectKind::App => {
                format!("top-level statements are allowed only in the entry file `{entry}`")
            }
        };
        out.push(Diagnostic::error(
            codes::MR0125,
            message,
            Label::new(first_statement(file), "a top-level statement"),
        ));
    }
    if project.kind(db) == ProjectKind::Lib {
        return;
    }
    let statements = map
        .entry_file()
        .filter(|file| map.statement_files().contains(file));
    let mains = map.mains();
    let (rest, first) = match statements {
        Some(file) => (
            mains,
            Label::new(first_statement(file), "the top-level statements start here"),
        ),
        None => match mains.split_first() {
            Some((&first, rest)) => (
                rest,
                Label::new(item_name_span(db, first), "the first `Main` is here"),
            ),
            None => return,
        },
    };
    for &main in rest {
        let message = if statements.is_some() {
            "`Main` and top-level statements are both entry points"
        } else {
            "more than one `Main` function"
        };
        out.push(
            Diagnostic::error(
                codes::MR0124,
                message,
                Label::new(item_name_span(db, main), "a second entry point"),
            )
            .with_secondary(first.clone())
            .with_note("an app has exactly one entry point: top-level statements in the entry file, or one `Main`"),
        );
    }
}

#[cfg(test)]
mod tests {
    use mora_diag::Diagnostic;
    use mora_span::{Dependency, Project, ProjectKind};

    use crate::test_db::{TestDb, app, codes, function, messages, resolve, signature_of};
    use crate::*;

    /// The text a diagnostic's primary span covers.
    fn primary_text(db: &TestDb, project: Project, diagnostic: &Diagnostic) -> String {
        let file = project
            .files(db)
            .iter()
            .find(|file| file.id(db) == diagnostic.primary.span.file)
            .unwrap();
        file.text(db)[diagnostic.primary.span.range].to_owned()
    }

    #[test]
    fn the_fix_for_a_hidden_item_marks_it_internal() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using UsersApi.Store.UserStore;\n"),
                ("src/Store/UserStore.mr", "@Json\nclass UserStore {}\n"),
            ],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0129"]);
        let fix = &reported[0].fixes[0];
        assert_eq!(fix.message, "mark `UserStore` `internal`");
        let edit = &fix.edits[0];
        assert_eq!(edit.replacement, "internal ");
        assert_eq!(u32::from(edit.span.range.start()), 6);
    }

    #[test]
    fn a_misplaced_statement_is_reported_where_it_starts() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "let x = 1;\n"),
                (
                    "src/Util.mr",
                    "class A {}\nSetup() {}\n\nSetup();\nlet y = 2;\n",
                ),
            ],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0125"]);
        assert_eq!(primary_text(&db, project, &reported[0]), "Setup();");
    }

    #[test]
    fn duplicate_declarations_are_named_with_their_kind() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using UsersApi.Store.Item;\n"),
                ("src/Store/A.mr", "internal struct Item {}\n"),
                ("src/Store/B.mr", "internal class Item {}\n"),
            ],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0128"]);
        assert_eq!(reported[0].message, "`Item` is ambiguous");
        assert_eq!(
            reported[0].note.as_deref(),
            Some(
                "it could be the struct `UsersApi.Store.Item` or the class `UsersApi.Store.Item`; \
             rename one of them"
            )
        );
        assert_eq!(reported[0].secondary.len(), 2);
    }

    #[test]
    fn an_unknown_member_names_what_it_was_looked_up_in() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "using UsersApi.Store.UserStore.Missing;\nusing UsersApi.Run.X;\n",
                ),
                (
                    "src/Store/UserStore.mr",
                    "internal class UserStore {\n    internal class Entry {}\n}\n",
                ),
                ("src/Run.mr", "Run() {}\n"),
            ],
        );
        let messages: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "cannot find `Missing` in `UsersApi.Store.UserStore`",
                "cannot find `X` in `UsersApi.Run`",
            ]
        );
        assert_eq!(
            resolve(&db, project, "src/Program.mr", "Store.UserStore.Entry"),
            Ok("UsersApi.Store.Entry".into())
        );
    }

    #[test]
    fn the_fix_replaces_a_narrower_modifier() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "using UsersApi.Store.Open.Inner;\nusing UsersApi.Store.Open.Local;\n",
                ),
                (
                    "src/Store/Types.mr",
                    "internal class Open {\n    class Inner {}\n    file class Local {}\n}\n",
                ),
            ],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0129", "MR0129"]);
        let edit = |index: usize| {
            let edit = &reported[index].fixes[0].edits[0];
            (edit.replacement.as_str(), edit.span.range.len().into())
        };
        assert_eq!(edit(0), ("internal ", 0u32));
        assert_eq!(edit(1), ("internal", 4u32));
        assert_eq!(
            reported[0].message,
            "`Inner` is visible only in `UsersApi.Store`, not in `UsersApi`"
        );
        assert_eq!(
            reported[1].message,
            "`Local` is visible only in `src/Store/Types.mr`"
        );
    }

    #[test]
    fn a_dependency_item_that_is_not_public_gets_a_public_fix() {
        let db = TestDb::new();
        let core = db.project(
            "core",
            ProjectKind::Lib,
            &[("src/Text/Lexer.mr", "internal class Lexer {}\n")],
            Vec::new(),
        );
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[("src/Program.mr", "using Core.Text.Lexer;\n")],
            vec![Dependency {
                name: "Core".into(),
                project: core,
            }],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0129"]);
        assert_eq!(
            reported[0].message,
            "`Lexer` is not public, so `UsersApi` cannot see it"
        );
        assert_eq!(reported[0].fixes[0].message, "mark `Lexer` `public`");
        assert_eq!(reported[0].fixes[0].edits[0].replacement, "public");
    }

    #[test]
    fn every_kind_is_named_in_an_ambiguity() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "using UsersApi.K.U;\nusing UsersApi.K.I;\nusing UsersApi.K.C;\nusing UsersApi.K.F;\n",
                ),
                (
                    "src/K/A.mr",
                    "internal union U { A }\ninternal interface I {}\ninternal const C: int = 1;\n\
                 internal class F {}\n",
                ),
                (
                    "src/K/B.mr",
                    "internal enum U { A }\ninternal macro I(x: Expr) {\n}\ninternal class C {}\n\
                 internal F() {}\n",
                ),
            ],
        );
        let notes: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| d.note.clone().unwrap())
            .collect();
        let prefix = |note: &str| note.split(';').next().unwrap().to_owned();
        assert_eq!(
            notes.iter().map(|note| prefix(note)).collect::<Vec<_>>(),
            [
                "it could be the union `UsersApi.K.U` or the enum `UsersApi.K.U`",
                "it could be the interface `UsersApi.K.I` or the macro `UsersApi.K.I`",
                "it could be the value `UsersApi.K.C` or the class `UsersApi.K.C`",
                "it could be the class `UsersApi.K.F` or the function `UsersApi.K.F`",
            ]
        );
    }

    #[test]
    fn an_ambiguity_counts_each_overload() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using UsersApi.K.F;\n"),
                ("src/K/A.mr", "internal class F {}\n"),
                ("src/K/B.mr", "internal F() {}\ninternal F(x: int) {}\n"),
            ],
        );
        let diagnostics = diagnostics(&db, project);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].primary.message.as_deref(),
            Some("`F` names 3 declarations")
        );
        assert_eq!(diagnostics[0].secondary.len(), 3);
    }

    #[test]
    fn different_declarations_ask_for_more_of_the_path() {
        let db = TestDb::new();
        let core = db.project(
            "core",
            ProjectKind::Lib,
            &[("src/Text/Parser.mr", "")],
            Vec::new(),
        );
        // A dependency bound to the project's own root name; the manifest
        // rejects this (MR0501), but resolution must still say something.
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[("src/Program.mr", "using UsersApi.Text;\n")],
            vec![Dependency {
                name: "UsersApi".into(),
                project: core,
            }],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0128"]);
        assert_eq!(
            reported[0].note.as_deref(),
            Some(
                "it could be the namespace `UsersApi` or the namespace `Core`; \
             write more of the path to choose one"
            )
        );
    }

    #[test]
    fn names_that_are_not_types_are_reported() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "const Max: int = 1;\nmacro M(x: Expr) {\n}\nF() {}\nF(x: int) {}\n\
             G(a: Max, b: M, c: F) {}\n",
            )],
        );
        let g = function(signature_of(&db, project, "src/Program.mr", "G"));
        assert!(g.params[..3].iter().all(|p| p.ty == Ty::Error), "{g:?}");
        assert_eq!(
            messages(diagnostics(&db, project)),
            [
                "`Max` is not a type",
                "`M` is not a type",
                "`F` is not a type",
            ]
        );
    }

    #[test]
    fn ambiguous_and_hidden_types_in_signatures_are_reported() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "class A {}\nstruct A {}\nF(a: A, b: UsersApi.Store.Hidden, c: List<Missing>, d: Mising<Usr>) {}\n",
                ),
                ("src/Store/Hidden.mr", "file struct Hidden {}\n"),
            ],
        );
        assert_eq!(
            codes(diagnostics(&db, project)),
            ["MR0128", "MR0129", "MR0127", "MR0127", "MR0127"]
        );
    }

    #[test]
    fn nested_types_resolve_inside_their_type_only() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "struct Outer {\n    private struct Inner {}\n    private Take(i: Inner): Outer.Inner {}\n    \
             Field: Inner;\n}\nF(i: Inner) {}\nG(i: Outer.Inner) {}\n",
            )],
        );
        let reported = diagnostics(&db, project);
        // `Field` is visible in the folder but `Inner` only inside `Outer`;
        // outside `Outer`, `Inner` is not found or not visible.
        assert_eq!(codes(reported), ["MR0123", "MR0127", "MR0129"]);
    }

    #[test]
    fn suggestions_come_from_what_the_path_names_so_far() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "struct Outer {\n    struct Inner {}\n    private struct Secret {}\n}\n\
                 F(a: Outer.Inne, b: UsersApi.Stor, c: Outer.Secre, d: Std.Lst.X, e: F.G) {}\n",
                ),
                ("src/Store/A.mr", ""),
            ],
        );
        let fixes: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| {
                (
                    d.message.clone(),
                    d.fixes.first().map(|fix| fix.message.clone()),
                )
            })
            .collect();
        assert_eq!(
            fixes,
            [
                (
                    "cannot find `Inne` in `UsersApi.Outer`".to_owned(),
                    Some("did you mean `Inner`?".to_owned())
                ),
                (
                    "cannot find `Stor` in `UsersApi`".to_owned(),
                    Some("did you mean `Store`?".to_owned())
                ),
                // `Secret` is private to `Outer`, so it is not suggested.
                ("cannot find `Secre` in `UsersApi.Outer`".to_owned(), None),
                ("cannot find `G` in `UsersApi.F`".to_owned(), None),
            ]
        );
    }

    #[test]
    fn less_visible_types_name_the_member_and_offer_a_fix_when_it_helps() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Store/A.mr",
                "struct Secret {}\ninternal struct Key {}\nstruct Box {\n    public struct Item {}\n}\n\
             public struct Vault {\n    public Current: Secret;\n    internal Kept: Secret;\n    \
             Last: Secret;\n}\npublic Open(key: Key) {}\ninternal Unbox(item: Box.Item) {}\n\
             file struct Token {}\nIssue(t: Token) {}\n\
             struct W {\n    private struct In {}\n    public P(i: In) {}\n}\n",
            )],
        );
        let reported: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| {
                (
                    d.code.to_string(),
                    d.message.clone(),
                    d.fixes.first().map(|fix| fix.message.clone()),
                )
            })
            .collect();
        let expected = [
            (
                "`Current` is visible everywhere, but its signature mentions `Secret`, which is visible only in its folder",
                Some("mark `Secret` `public`"),
            ),
            (
                "`Kept` is visible in its project, but its signature mentions `Secret`, which is visible only in its folder",
                Some("mark `Secret` `internal`"),
            ),
            (
                "`Open` is visible everywhere, but its signature mentions `Key`, which is visible only in its project",
                Some("mark `Key` `public`"),
            ),
            (
                "`Unbox` is visible in its project, but its signature mentions `Item`, which is visible only in its folder",
                None,
            ),
            (
                "`Issue` is visible in its folder, but its signature mentions `Token`, which is visible only in its file",
                None,
            ),
            (
                "`P` is visible in its folder, but its signature mentions `In`, which is visible only inside its type",
                None,
            ),
        ];
        let expected: Vec<_> = expected
            .iter()
            .map(|(message, fix)| {
                (
                    "MR0123".to_owned(),
                    (*message).to_owned(),
                    fix.map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(reported, expected);
    }

    #[test]
    fn type_keywords_are_suggested_only_for_a_first_segment_in_a_type() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[(
                "src/Program.mr",
                "using strng;\nF(a: strng, b: UsersApi.strng) {}\n",
            )],
        );
        let fixes: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| d.fixes.first().map(|fix| fix.message.clone()))
            .collect();
        assert_eq!(
            fixes,
            [None, Some("did you mean `string`?".to_owned()), None]
        );
    }

    #[test]
    fn names_in_the_arguments_of_a_non_type_are_checked() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "const Max: int = 1;\nF() {}\n\
                     G(a: Max<Usr>, b: F<Usr>, c: Store<Usr>, d: Mising<Usr>) {}\n",
                ),
                ("src/Store/A.mr", ""),
            ],
        );
        assert_eq!(
            messages(diagnostics(&db, project)),
            [
                "`Max` is not a type",
                "cannot find `Usr`",
                "`F` is not a type",
                "cannot find `Usr`",
                "`Store` is not a type",
                "cannot find `Usr`",
                "cannot find `Mising`",
                "cannot find `Usr`",
            ]
        );
    }

    #[test]
    fn a_member_name_is_declared_once_unless_methods_overload_it() {
        let text = "struct User {\n    Name: string;\n    Address: string;\n    Name: int;\n    \
                    struct Address {}\n    Count() {}\n    Count: int;\n    Find(a: int) {}\n    \
                    Find(b: string) {}\n}\nclass Store {\n    Load() {}\n    class Load {}\n}\n";
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", text)]);
        let at = |pattern: &str, offset: usize| text.find(pattern).unwrap() + offset;
        let reported: Vec<_> = diagnostics(&db, project)
            .iter()
            .map(|d| {
                (
                    d.message.clone(),
                    usize::from(d.primary.span.range.start()),
                    usize::from(d.secondary[0].span.range.start()),
                )
            })
            .collect();
        let expected = [
            (
                "`Name` is already declared in `User`",
                at("Name: int", 0),
                at("Name: string", 0),
            ),
            (
                "`Address` is already declared in `User`",
                at("struct Address", 7),
                at("Address: string", 0),
            ),
            (
                "`Count` is already declared in `User`",
                at("Count: int", 0),
                at("Count()", 0),
            ),
            (
                "`Load` is already declared in `Store`",
                at("class Load", 6),
                at("Load()", 0),
            ),
        ];
        let expected: Vec<_> = expected
            .iter()
            .map(|&(message, primary, first)| (message.to_owned(), primary, first))
            .collect();
        assert_eq!(reported, expected);
    }
}
