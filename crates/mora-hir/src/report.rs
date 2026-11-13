use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{
    Db, FileId, Project, ProjectKind, SourceFile, Span, TextRange, TextSize, to_text_size,
};
use mora_syntax::SyntaxKind;
use mora_syntax::ast::{self, AstNode};

use crate::arena::arena_index;
use crate::bodies::{
    BodyError, LocalKind, RefDefaultRanges, body, body_owners, body_resolution, body_scope,
    body_source_map,
};
use crate::defs::{DefMap, FolderProblem, ItemId, def_map};
use crate::inference::{Source, type_error, type_errors};
use crate::items::{ItemKind, Visibility, item_source_map, item_tree, parse, signature_sources};
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
/// `MR0123`), the member names of its types (`MR0146`), the modifiers
/// written twice or that conflict (`MR0111`, `MR0112`), the locals of its
/// bodies (`MR0180`), the types in its bodies (`MR0010`, `MR0203`,
/// `MR0204`) and its entry point (`MR0124`, `MR0125`). Lexical and
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
        out.extend(modifier_diagnostics(db, file).iter().cloned());
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
        let source = &sources[arena_index(idx)];
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

/// `MR0209`: the `ref` parameter `name` has a default value, with the fixes
/// that remove the default or the `ref`.
fn ref_default(db: &dyn Db, file: SourceFile, name: &str, written: RefDefaultRanges) -> Diagnostic {
    let id = file.id(db);
    // The `ref` goes with the space after it, so the name stays.
    let keyword = with_space_after(file.text(db), written.keyword);
    Diagnostic::error(
        codes::MR0209,
        format!(
            "the `ref` parameter `{name}` cannot have a default value, because every call must pass a variable it can change"
        ),
        Label::new(
            Span::new(id, written.value),
            "this default cannot be passed by `ref`",
        ),
    )
    .with_note("a `ref` argument is a variable the function changes, and a default is a fixed value")
    .with_fix(Fix::new(
        "remove the default value",
        vec![TextEdit::replace(Span::new(id, written.default), "")],
    ))
    .with_fix(Fix::new(
        "remove `ref`",
        vec![TextEdit::replace(Span::new(id, keyword), "")],
    ))
}

/// The errors in the bodies of a file: `MR0180`, names and types that do
/// not resolve, and type errors. A file whose bodies did not change keeps
/// its result.
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
        for &local in &body.ref_defaults {
            out.push(ref_default(
                db,
                file,
                &body.locals[local].name,
                map.ref_defaults[&local],
            ));
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
        let source = Source {
            db,
            file,
            body,
            map,
        };
        for error in &type_errors(db, project, file, owner) {
            out.push(type_error(&source, error));
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

/// `MR0111` and `MR0112`: a modifier written again in the modifiers of
/// one declaration, parameter or lambda, or one that conflicts with an
/// earlier one (§6.1). The grammar takes any number of them, so the parser
/// lets both through. The fix for a repetition removes it when it is
/// written as the first one; a `mut` whose write scope differs from the
/// first one's leaves the choice to the author. The fix for a conflict
/// removes the later modifier. A file whose text did not change keeps its
/// result.
#[salsa::tracked(returns(ref))]
fn modifier_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let id = file.id(db);
    let text = file.text(db);
    let mut out = Vec::new();
    for node in parse(db, file).syntax().descendants() {
        let written: Vec<(SyntaxKind, TextRange)> = node
            .children()
            .filter_map(ast::Modifier::cast)
            .filter_map(|modifier| {
                let keyword = modifier.kind()?.kind();
                let mut tokens = modifier
                    .syntax()
                    .tokens()
                    .filter(|token| !token.kind().is_trivia());
                let start = tokens.next()?.text_range();
                let end = tokens.last().map_or(start, |token| token.text_range());
                Some((keyword, start.cover(end)))
            })
            .collect();
        for (index, &(keyword, range)) in written.iter().enumerate() {
            let earlier = &written[..index];
            let name = keyword.text().unwrap_or_default();
            if let Some(&(_, first)) = earlier.iter().find(|(kind, _)| *kind == keyword) {
                out.push(repeated_modifier(id, text, name, range, first));
                continue;
            }
            let Some(exclusive) = exclusive(node.kind(), keyword) else {
                continue;
            };
            if let Some(&(other, first)) = earlier
                .iter()
                .find(|(kind, _)| exclusive.keywords.contains(kind))
            {
                let other = other.text().unwrap_or_default();
                out.push(conflicting_modifier(
                    id, text, name, range, other, first, exclusive,
                ));
            }
        }
    }
    out
}

/// `MR0111`: the modifier `name` at `range` repeats the one at `first`.
/// The fix removes it only when both are spelled the same.
fn repeated_modifier(
    file: FileId,
    text: &str,
    name: &str,
    range: TextRange,
    first: TextRange,
) -> Diagnostic {
    let spelled = |range: TextRange| -> String { text[range].split_whitespace().collect() };
    let diagnostic = Diagnostic::error(
        codes::MR0111,
        format!("`{name}` is written twice"),
        Label::new(Span::new(file, range), "written again here"),
    )
    .with_secondary(Label::new(Span::new(file, first), "first written here"));
    if spelled(range) != spelled(first) {
        return diagnostic
            .with_note("the two give different write scopes; keep the one that is meant");
    }
    diagnostic
        .with_note("a modifier says the same thing however often it is written")
        .with_fix(Fix::new(
            format!("remove the repeated `{name}`"),
            vec![TextEdit::replace(
                Span::new(file, with_space_after(text, range)),
                "",
            )],
        ))
}

/// `MR0112`: the modifier `name` at `range` conflicts with the earlier
/// `other` at `first` under the rule of `exclusive`. The fix removes the
/// later one.
fn conflicting_modifier(
    file: FileId,
    text: &str,
    name: &str,
    range: TextRange,
    other: &str,
    first: TextRange,
    exclusive: &Exclusive,
) -> Diagnostic {
    Diagnostic::error(
        codes::MR0112,
        format!("`{name}` conflicts with `{other}`: {}", exclusive.rule),
        Label::new(
            Span::new(file, range),
            format!("`{other}` is written already"),
        ),
    )
    .with_secondary(Label::new(Span::new(file, first), "written first here"))
    .with_note("keep the one that is meant; the fix keeps the first")
    .with_fix(Fix::new(
        format!("remove `{name}`"),
        vec![TextEdit::replace(
            Span::new(file, with_space_after(text, range)),
            "",
        )],
    ))
}

/// `range` with the white space after it in `text`: what a fix removes so
/// that a keyword goes without leaving its space behind. A comment after
/// it stays.
pub(crate) fn with_space_after(text: &str, range: TextRange) -> TextRange {
    let space = text
        .get(usize::from(range.end())..)
        .map_or(0, |rest| rest.len() - rest.trim_start().len());
    TextRange::new(range.start(), range.end() + to_text_size(space))
}

/// Modifiers of which a declaration, parameter or lambda takes one at
/// most, and the rule that says so.
struct Exclusive {
    /// The modifiers.
    keywords: &'static [SyntaxKind],
    /// Why only one of them fits, for the message.
    rule: &'static str,
}

/// The visibilities (§4.5).
const VISIBILITIES: Exclusive = Exclusive {
    keywords: &[
        SyntaxKind::PrivateKw,
        SyntaxKind::FileKw,
        SyntaxKind::InternalKw,
        SyntaxKind::PublicKw,
    ],
    rule: "a declaration has one visibility",
};

/// The modes of a parameter (§10.3).
const MODES: Exclusive = Exclusive {
    keywords: &[SyntaxKind::InKw, SyntaxKind::RefKw, SyntaxKind::MoveKw],
    rule: "a parameter has one mode",
};

/// How a method takes `this`: `mut` by `ref`, `move` by `move` (§10.3).
const RECEIVERS: Exclusive = Exclusive {
    keywords: &[SyntaxKind::MutKw, SyntaxKind::MoveKw],
    rule: "a method takes `this` one way",
};

/// The modifiers that exclude `keyword` in the modifiers of a node of the
/// kind `node`, if any do.
fn exclusive(node: SyntaxKind, keyword: SyntaxKind) -> Option<&'static Exclusive> {
    let sets: &[&'static Exclusive] = match node {
        SyntaxKind::Param | SyntaxKind::LambdaParam => &[&VISIBILITIES, &MODES],
        SyntaxKind::FunctionDecl => &[&VISIBILITIES, &RECEIVERS],
        _ => &[&VISIBILITIES],
    };
    sets.iter()
        .copied()
        .find(|set| set.keywords.contains(&keyword))
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
                .map(|(value, source)| (value.name.as_str(), source.name, false)),
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
        ResolveErrorKind::NotVisible(item) => {
            not_visible(db, scope, name, span, Declared::Item(item))
        }
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
            vec![widen(db, Declared::Item(ty), keyword)],
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

/// A declaration a use may not see: an item, or the value member at an
/// index of a type, such as a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Declared {
    Item(ItemId),
    Value(ItemId, u32),
}

impl Declared {
    /// The item, or the type that declares the value member.
    fn item(self) -> ItemId {
        match self {
            Self::Item(item) | Self::Value(item, _) => item,
        }
    }

    /// The visibility after the types around the declaration narrow it.
    fn visibility(self, db: &dyn Db) -> Visibility {
        match self {
            Self::Item(item) => effective_visibility(db, item),
            Self::Value(ty, index) => item_data(db, ty).values[index as usize]
                .visibility
                .min(effective_visibility(db, ty)),
        }
    }

    /// Where its name is written.
    fn name_span(self, db: &dyn Db) -> Span {
        match self {
            Self::Item(item) => item_name_span(db, item),
            Self::Value(ty, index) => {
                let source = item_source_map(db, ty.file).item(ty.item);
                Span::new(ty.file.id(db), source.values[index as usize].name)
            }
        }
    }

    /// Its visibility modifier, when it has one, and where a modifier goes.
    fn modifiers(self, db: &dyn Db) -> (Option<TextRange>, TextSize) {
        let item = self.item();
        let source = item_source_map(db, item.file).item(item.item);
        match self {
            Self::Item(_) => (source.visibility, source.modifiers_start),
            Self::Value(_, index) => {
                let value = &source.values[index as usize];
                (value.visibility, value.modifiers_start)
            }
        }
    }
}

/// `MR0129`: the use at `span`, in `scope`, names `name`, the declaration
/// `declared`, which it cannot see. Names where the declaration is visible
/// and where the use is, and offers to widen it (§4.5).
pub(crate) fn not_visible(
    db: &dyn Db,
    scope: Scope,
    name: &str,
    span: Span,
    declared: Declared,
) -> Diagnostic {
    let item = declared.item();
    let declared_in = file_namespace(db, item.project, item.file).unwrap_or_default();
    let used_in = file_namespace(db, scope.project, scope.file).unwrap_or_default();
    let same_project = item.project == scope.project;
    let (message, note) = match declared.visibility(db) {
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
        .with_secondary(Label::new(declared.name_span(db), "declared here"))
        .with_note(note)
        .with_fix(Fix::new(
            format!("mark `{name}` `{keyword}`"),
            vec![widen(db, declared, keyword)],
        ))
}

/// The edit that gives `declared` the visibility modifier `keyword`. Only
/// the declaration needs it: a type around it that the use cannot see
/// fails the lookup first.
fn widen(db: &dyn Db, declared: Declared, keyword: &str) -> TextEdit {
    let file = declared.item().file.id(db);
    match declared.modifiers(db) {
        (Some(range), _) => TextEdit::replace(Span::new(file, range), keyword),
        (None, start) => TextEdit::insert(file, start, format!("{keyword} ")),
    }
}

pub(crate) fn item_name_span(db: &dyn Db, item: ItemId) -> Span {
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

    use crate::test_db::{
        TestDb, app, codes, diagnostics_of, function, messages, resolve, signature_of,
    };
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
    fn a_dependency_field_that_is_not_public_gets_a_public_fix() {
        let db = TestDb::new();
        let core = db.project(
            "core",
            ProjectKind::Lib,
            &[(
                "src/Text/Token.mr",
                "public struct Token {\n    public Kind: int;\n    internal Width: int = 1;\n}\n",
            )],
            Vec::new(),
        );
        let project = db.project(
            "users-api",
            ProjectKind::App,
            &[(
                "src/Program.mr",
                "using Core.Text;\nlet t = new Token { Kind = 1, Width = 2 };\n",
            )],
            vec![Dependency {
                name: "Core".into(),
                project: core,
            }],
        );
        let reported = diagnostics(&db, project);
        assert_eq!(codes(reported), ["MR0129"]);
        assert_eq!(
            reported[0].message,
            "`Width` is not public, so `UsersApi` cannot see it"
        );
        assert_eq!(
            reported[0].secondary[0].message.as_deref(),
            Some("declared here")
        );
        assert_eq!(reported[0].fixes[0].message, "mark `Width` `public`");
        let edit = &reported[0].fixes[0].edits[0];
        assert_eq!(edit.replacement, "public");
        assert_eq!(u32::from(edit.span.range.len()), 8);
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

    #[test]
    fn a_modifier_written_twice_is_mr0111_with_a_fix_that_removes_it() {
        let text = "struct Q {\n    public mut mut N: int;\n    public public X: int;\n    \
                    static static F(): int => 1;\n    public M(ref ref n: int) {}\n}\n\
                    G(): int {\n    static static L(): int => 1;\n    return L();\n}\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0111"; 5]);
        assert_eq!(found[0].message, "`mut` is written twice");
        let primary = |d: &Diagnostic| &text[d.primary.span.range];
        let first = |d: &Diagnostic| &text[d.secondary[0].span.range];
        assert_eq!((primary(&found[0]), first(&found[0])), ("mut", "mut"));
        assert!(found[0].primary.span.range.start() > found[0].secondary[0].span.range.start());
        assert_eq!(primary(&found[3]), "ref");
        let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert_eq!(
            fixed.text,
            "struct Q {\n    public mut N: int;\n    public X: int;\n    \
             static F(): int => 1;\n    public M(ref n: int) {}\n}\n\
             G(): int {\n    static L(): int => 1;\n    return L();\n}\n"
        );
        // Write scopes that differ leave the choice to the author, and the
        // same scope written twice is one repetition.
        let text = "struct S {\n    public mut mut(private) N: int;\n    \
                    public mut(private) mut(private) M: int;\n}\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0111"; 2]);
        assert_eq!(&text[found[0].primary.span.range], "mut(private)");
        assert!(found[0].fixes.is_empty(), "{found:#?}");
        assert_eq!(found[1].fixes.len(), 1, "{found:#?}");
        // Three times is two repetitions of the first.
        let found = diagnostics_of("struct S {\n    public public public N: int;\n}\n");
        assert_eq!(codes(&found), ["MR0111"; 2]);
    }

    #[test]
    fn modifiers_that_conflict_are_mr0112_with_a_fix_that_removes_the_later() {
        let text = "struct Q {\n    public private N: int;\n    internal file X: int;\n    \
                    public M(ref move n: int, in ref m: int) {}\n    public mut move K() {}\n}\n\
                    G(): int {\n    let f = (ref in x: int) => 1;\n    return 1;\n}\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0112"; 6]);
        assert_eq!(
            found[0].message,
            "`private` conflicts with `public`: a declaration has one visibility"
        );
        assert_eq!(&text[found[0].primary.span.range], "private");
        assert_eq!(&text[found[0].secondary[0].span.range], "public");
        assert_eq!(
            found[2].message,
            "`move` conflicts with `ref`: a parameter has one mode"
        );
        assert_eq!(
            found[4].message,
            "`move` conflicts with `mut`: a method takes `this` one way"
        );
        let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert_eq!(
            fixed.text,
            "struct Q {\n    public N: int;\n    internal X: int;\n    \
             public M(ref n: int, in m: int) {}\n    public mut K() {}\n}\n\
             G(): int {\n    let f = (ref x: int) => 1;\n    return 1;\n}\n"
        );
        // A repetition of the first is `MR0111` alone, and `mut` with
        // `move` conflicts only on a method.
        let text = "struct S {\n    public private public N: int;\n}\n";
        assert_eq!(codes(&diagnostics_of(text)), ["MR0112", "MR0111"]);
        let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
        assert_eq!(fixed.text, "struct S {\n    public N: int;\n}\n");
        assert_eq!(
            codes(&diagnostics_of(
                "struct S {\n    public mut N: int;\n    static mut M: int = 1;\n}\n"
            )),
            Vec::<String>::new()
        );
    }
}
