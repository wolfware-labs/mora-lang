//! The modifiers of declarations, parameters and lambdas, and the members
//! a declaration may have (§6.1, §6.6, §6.8, §6.9, §6.10, §9.2):
//! [`modifier_diagnostics`] reports a modifier written twice (`MR0111`),
//! one that conflicts with an earlier one (`MR0112`; §4.5, §10.3), one that
//! does not apply (`MR0113`), a constant without a value (`MR0114`) and a
//! member its declaration cannot have (`MR0115`).

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Db, FileId, SourceFile, Span, TextRange, to_text_size};
use mora_syntax::ast::{self, AstNode};
use mora_syntax::{SyntaxError, SyntaxKind, SyntaxNode, SyntaxNodeChildren};

use crate::items::parse;

/// `MR0111`, `MR0112` and `MR0113`: a modifier written again in the
/// modifiers of one declaration, parameter or lambda, one that conflicts
/// with an earlier one, or one that does not apply to the declaration
/// (§6.1). The grammar takes any number of them, so the parser lets all
/// three through. The fix for a repetition removes it when it is written
/// as the first one; a `mut` whose write scope differs from the first
/// one's leaves the choice to the author. The fix for a conflict removes
/// the later modifier, and the one for a modifier that does not apply
/// removes it, unless it is a `const` member of an interface or an
/// extension, where only the author knows what is meant, or a member that
/// would be `MR0115` without it. With them comes `MR0114`, a `const` field
/// without a value where a constant may be declared, and `MR0115`, a
/// member that its declaration cannot have, neither of which has a fix. A
/// file whose text did not change keeps its result.
#[salsa::tracked(returns(ref))]
pub(super) fn modifier_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let id = file.id(db);
    let text = file.text(db);
    // One walk over the tree, with a frame for each node on the way down:
    // the walk gathers the modifiers among the node's children as it passes
    // them, on one list for the whole walk, and the node is checked once
    // they are all seen. Each diagnostic carries its node's place in
    // preorder, so they come out in the order of the nodes.
    let mut found: Vec<(usize, Diagnostic)> = Vec::new();
    let parsed = parse(db, file);
    let errors = parsed.errors();
    let root = parsed.syntax();
    let mut written = Vec::new();
    let mut stack = vec![WalkFrame::new(root, root.kind(), 0, 0)];
    let mut visited = 0;
    // The frame on top is taken off while the walk looks at it, and put
    // back while it has children to walk.
    while let Some(mut frame) = stack.pop() {
        let Some(child) = frame.children.next() else {
            let kind = frame.node.kind();
            // Most nodes have no modifiers and no rule of `MR0115` to break:
            // nothing to check.
            if written.len() != frame.written_from || member_rule_may_apply(frame.container, kind) {
                let checked = ModifierFrame {
                    node: frame.node,
                    container: frame.container,
                    written: &written[frame.written_from..],
                    owner: indexer_of(&stack, kind),
                };
                check_node(id, text, errors, &checked, |diagnostic| {
                    found.push((frame.order, diagnostic));
                });
            }
            written.truncate(frame.written_from);
            continue;
        };
        if let Some(modifier) = ast::Modifier::cast(child).and_then(written_modifier) {
            written.push(modifier);
        }
        visited += 1;
        // A member list or a union body is part of the declaration that
        // holds it.
        let container = match frame.node.kind() {
            SyntaxKind::MemberList | SyntaxKind::UnionBody => frame.container,
            kind => kind,
        };
        stack.push(frame);
        stack.push(WalkFrame::new(child, container, visited, written.len()));
    }
    // A stable sort keeps the diagnostics of one node in their order.
    found.sort_by_key(|&(order, _)| order);
    found
        .into_iter()
        .map(|(_, diagnostic)| diagnostic)
        .collect()
}

/// A node on the way down the walk of [`modifier_diagnostics`].
struct WalkFrame<'a> {
    /// The node.
    node: SyntaxNode<'a>,
    /// The children not walked yet.
    children: SyntaxNodeChildren<'a>,
    /// The kind of the node the node is in, as [`ModifierFrame::container`].
    container: SyntaxKind,
    /// The node's place in preorder.
    order: usize,
    /// Where the modifiers among the node's children start on the walk's
    /// list: those of the node's own children follow, since the walk
    /// removes those of a child's children when it leaves the child.
    written_from: usize,
}

impl<'a> WalkFrame<'a> {
    fn new(node: SyntaxNode<'a>, container: SyntaxKind, order: usize, written_from: usize) -> Self {
        Self {
            node,
            children: node.children(),
            container,
            order,
            written_from,
        }
    }
}

/// The indexer that a node of the kind `kind` belongs to, with the kind of
/// node the indexer is in, as [`ModifierFrame::owner`]: for an accessor
/// list and its accessors. `stack` holds the nodes the node is in, its
/// parent last. An accessor's verdict depends on its indexer.
fn indexer_of<'a>(
    stack: &[WalkFrame<'a>],
    mut kind: SyntaxKind,
) -> Option<(SyntaxKind, SyntaxNode<'a>)> {
    let mut parents = stack.iter().rev();
    loop {
        let parent = parents.next()?;
        match parent.node.kind() {
            SyntaxKind::IndexerDecl if kind == SyntaxKind::AccessorList => {
                return Some((parent.container, parent.node));
            }
            // An accessor belongs to the indexer of its accessor list.
            SyntaxKind::AccessorList => kind = SyntaxKind::AccessorList,
            _ => return None,
        }
    }
}

/// A node of the walk of [`modifier_diagnostics`] whose children are all
/// seen, as the checks read it.
struct ModifierFrame<'a, 'w> {
    /// The node.
    node: SyntaxNode<'a>,
    /// The kind of the node the node is in: its parent, or for a member
    /// in a member list or a union body, the declaration that holds it. It
    /// tells a method from a function, and a member of an interface, an
    /// extension or a union from one of another type.
    container: SyntaxKind,
    /// The keyword and the range of each modifier among the children.
    written: &'w [(SyntaxKind, TextRange)],
    /// For an accessor list and its accessors, the indexer they belong to
    /// and the kind of node it is in.
    owner: Option<(SyntaxKind, SyntaxNode<'a>)>,
}

impl ModifierFrame<'_, '_> {
    /// Where the modifier `keyword` is first written on the node, if it is.
    fn modifier(&self, keyword: SyntaxKind) -> Option<TextRange> {
        self.written
            .iter()
            .find(|&&(kind, _)| kind == keyword)
            .map(|&(_, range)| range)
    }

    /// Whether `const` is written on the node and does not apply to it
    /// (`MR0113`). The node then gets no `MR0114` or `MR0115`, so that it
    /// has one error.
    fn misplaced_const(&self) -> bool {
        self.modifier(SyntaxKind::ConstKw).is_some()
            && misplaced(self, SyntaxKind::ConstKw).is_some()
    }
}

/// Reports, through `report`, what is wrong with the modifiers written on
/// the node of `frame` and with the node as a member of its container.
fn check_node(
    id: FileId,
    text: &str,
    syntax_errors: &[SyntaxError],
    frame: &ModifierFrame<'_, '_>,
    mut report: impl FnMut(Diagnostic),
) {
    check_modifiers(id, text, frame, &mut report);
    if let Some(diagnostic) = constant_without_value(id, frame, syntax_errors) {
        report(diagnostic);
    }
    if let Some(diagnostic) = misplaced_member(id, frame, syntax_errors) {
        report(diagnostic);
    }
}

/// The keyword of `modifier` and where it is written, from its first token
/// to its last, trivia aside.
fn written_modifier(modifier: ast::Modifier<'_>) -> Option<(SyntaxKind, TextRange)> {
    let keyword = modifier.kind()?.kind();
    let mut tokens = modifier
        .syntax()
        .tokens()
        .filter(|token| !token.kind().is_trivia());
    let start = tokens.next()?.text_range();
    let end = tokens.last().map_or(start, |token| token.text_range());
    Some((keyword, start.cover(end)))
}

/// Reports, through `report`, each of the modifiers written on the node of
/// `frame` that repeats an earlier one, does not apply to the node or
/// conflicts with an earlier one.
fn check_modifiers(
    id: FileId,
    text: &str,
    frame: &ModifierFrame<'_, '_>,
    mut report: impl FnMut(Diagnostic),
) {
    let node = frame.node.kind();
    let written = frame.written;
    for (index, &(keyword, range)) in written.iter().enumerate() {
        let earlier = &written[..index];
        let name = keyword.text().unwrap_or_default();
        if let Some(&(_, first)) = earlier.iter().find(|(kind, _)| *kind == keyword) {
            report(repeated_modifier(id, text, name, range, first));
            continue;
        }
        if let Some(misplaced) = misplaced(frame, keyword) {
            report(misplaced_modifier(id, text, name, range, &misplaced));
            continue;
        }
        let Some(exclusive) = exclusive(node, keyword) else {
            continue;
        };
        if let Some(&(other, first)) = earlier
            .iter()
            .find(|(kind, _)| exclusive.keywords.contains(kind))
        {
            let other = other.text().unwrap_or_default();
            report(conflicting_modifier(
                id, text, name, range, other, first, exclusive,
            ));
        }
    }
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
        .with_fix(remove_keyword(
            file,
            text,
            range,
            format!("remove the repeated `{name}`"),
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
    .with_fix(remove_keyword(
        file,
        text,
        range,
        format!("remove `{name}`"),
    ))
}

/// A modifier on a declaration it does not apply to, for the message.
struct Misplaced {
    /// The kind of declaration, with its article.
    declaration: &'static str,
    /// Why the modifier does not apply to it.
    reason: &'static str,
    /// What to write instead, when removing the modifier would not give
    /// what is meant. The diagnostic then has no fix.
    instead: Option<String>,
}

/// Why the modifier `keyword` does not apply to the node of `frame`, if it
/// does not.
/// `const` declares a constant, a value fixed at compile time, so of the
/// declarations, parameters and accessors the parser takes it on, it
/// applies to a field alone, of a struct, a class or a union or in a
/// namespace (§6.1, D-156). An interface and an extension have no fields:
/// there `const` on a field, or on a getter without a body, which is how
/// an interface reads `const K: int;`, gets no fix, since the author may
/// mean a constant in the namespace or a member without `const`. Nor does
/// one where, without `const`, the member would be one that its interface,
/// extension or type cannot have ([`member_rule`]), or an accessor of such
/// an indexer: the note then says why it cannot be there. An error
/// node, where the parser found no declaration after the modifiers, is
/// reported already. A function in the file is declared in a namespace;
/// any other is a method of a type, an interface or an extension, since a
/// local function never starts with `const`.
fn misplaced(frame: &ModifierFrame<'_, '_>, keyword: SyntaxKind) -> Option<Misplaced> {
    use SyntaxKind as K;
    if keyword != K::ConstKw {
        return None;
    }
    let (container, node) = (frame.container, frame.node);
    let field = node.kind() == K::FieldDecl
        || ast::GetterDecl::cast(node).is_some_and(|getter| getter.expr_body().is_none());
    let (declaration, reason, instead) = match container {
        K::InterfaceDecl if field => (
            "a member of an interface",
            "an interface has no fields",
            "a value every implementation shares is a constant in the namespace, next to \
             the interface, as `const K: int = 1;`; a value each implementing type gives is \
             a static abstract getter, as `static abstract K: int;`",
        ),
        K::ExtensionDecl if field => (
            "a member of an extension",
            "an extension adds no fields",
            "a constant goes in the namespace, next to the extension, or in the type \
             itself, as `const K: int = 1;`",
        ),
        _ => {
            let mut misplaced = misplaced_anywhere(container, node.kind())?;
            let rule = match frame.owner {
                Some((container, indexer)) => {
                    member_rule(container, indexer, |keyword| modifier_of(indexer, keyword))
                }
                None => member_rule(container, node, |keyword| frame.modifier(keyword)),
            };
            misplaced.instead = rule.map(|rule| format!("{}; {}", rule.message, rule.note));
            return Some(misplaced);
        }
    };
    Some(Misplaced {
        declaration,
        reason,
        instead: Some(instead.to_owned()),
    })
}

/// Why `const` does not apply to a node of the kind `node` in a node of the
/// kind `container`, wherever it is, if it does not: removing it gives a
/// declaration that is right as it is.
fn misplaced_anywhere(container: SyntaxKind, node: SyntaxKind) -> Option<Misplaced> {
    use SyntaxKind as K;
    /// What runs as code holds no value fixed at compile time.
    const CODE: &str = "it is code that runs, not a value";
    /// What declares a type holds no value at all.
    const TYPE: &str = "it declares a type, not a value";
    let (declaration, reason) = match node {
        K::GetterDecl => ("a getter", "a getter computes its value on each read"),
        K::FunctionDecl if container == K::SourceFile => ("a function", CODE),
        K::FunctionDecl => ("a method", CODE),
        K::ConstructorDecl => ("a constructor", CODE),
        K::DestructorDecl => ("a destructor", CODE),
        K::OperatorDecl => ("an operator", CODE),
        K::IndexerDecl => ("an indexer", CODE),
        K::Accessor => ("an accessor", CODE),
        K::MacroDecl => ("a macro", CODE),
        K::Param => ("a parameter", "a parameter takes its value from each call"),
        K::AssocTypeDecl => ("an associated type", TYPE),
        K::ExtensionDecl => ("an extension", "it adds members to a type, not a value"),
        kind => (type_declaration(kind)?, TYPE),
    };
    Some(Misplaced {
        declaration,
        reason,
        instead: None,
    })
}

/// `MR0113`: the modifier `name` at `range` does not apply to the
/// declaration it is on, for the reason in `misplaced`. The fix removes it,
/// unless `misplaced` says what to write instead.
fn misplaced_modifier(
    file: FileId,
    text: &str,
    name: &str,
    range: TextRange,
    misplaced: &Misplaced,
) -> Diagnostic {
    let diagnostic = Diagnostic::error(
        codes::MR0113,
        format!("`{name}` does not apply to {}", misplaced.declaration),
        Label::new(Span::new(file, range), misplaced.reason),
    );
    if let Some(instead) = &misplaced.instead {
        return diagnostic.with_note(instead.as_str());
    }
    diagnostic
        .with_note(
            "a constant is a value fixed at compile time; for one, write a field with an \
         initializer, as `const K: int = 1;`",
        )
        .with_fix(remove_keyword(
            file,
            text,
            range,
            format!("remove `{name}`"),
        ))
}

/// `MR0114`: the node of `frame` is a `const` field without a value, where
/// a constant may be declared (§6.1, D-156): in an interface or an
/// extension, `const` is `MR0113` whatever the value. A constant is a value
/// fixed at compile time, so it is declared with it. No fix: only the
/// author knows the value. A field with a syntax error in it, or a
/// missing token at its end, as `const K: int => 1;` or `const K;`, is
/// reported by the parser alone ([`head_has_syntax_error`]).
fn constant_without_value(
    file: FileId,
    frame: &ModifierFrame<'_, '_>,
    syntax_errors: &[SyntaxError],
) -> Option<Diagnostic> {
    frame.modifier(SyntaxKind::ConstKw)?;
    let declarator = ast::FieldDecl::cast(frame.node)?.variable_declarator()?;
    // After a syntax error in the field, the value may be what the parser
    // could not take, as in `const K: int => 1;`.
    if declarator.eq_token().is_some()
        || head_has_syntax_error(frame.node, syntax_errors)
        || frame.misplaced_const()
    {
        return None;
    }
    let name = declarator.name()?.ident_token()?;
    Some(
        Diagnostic::error(
            codes::MR0114,
            format!("constant `{}` has no value", name.text()),
            Label::new(
                Span::new(file, name.text_range()),
                "a constant is declared with its value",
            ),
        )
        .with_note(
            "a constant is a value fixed at compile time; write that value after the type, \
             as `const K: int = 1;`",
        ),
    )
}

/// `MR0115`: the node of `frame` is a member that the declaration it is in
/// cannot have (§6.6, §6.8, §6.9, §9.2, D-157). An interface has no
/// fields, destructors or nested types, and its static members are
/// `static abstract`; an extension adds only methods and getters, so no
/// fields, destructors, static members, indexers, nested or associated
/// types, and an operator is a static method; a union's data is in its
/// cases, so it has no instance fields; and a macro is declared in a
/// namespace, never among the members of a type. Where
/// `const` on the node is `MR0113`, that is the one error reported. No
/// fix: only the author knows where the member belongs. A constructor
/// cannot be written in an interface or an extension: there, `P() {}` is
/// a method. A member with a syntax error in its head is reported by the
/// parser alone ([`head_has_syntax_error`]).
fn misplaced_member(
    file: FileId,
    frame: &ModifierFrame<'_, '_>,
    syntax_errors: &[SyntaxError],
) -> Option<Diagnostic> {
    if frame.misplaced_const() {
        return None;
    }
    let rule = member_rule(frame.container, frame.node, |keyword| {
        frame.modifier(keyword)
    })?;
    // A member the parser built around an error may be no member the
    // author wrote, as the fields it makes of a broken lambda. Asked only
    // of a misplaced member, so the walk does not search the errors at
    // every node.
    if head_has_syntax_error(frame.node, syntax_errors) {
        return None;
    }
    Some(
        Diagnostic::error(
            codes::MR0115,
            rule.message,
            Label::new(Span::new(file, rule.range), rule.label),
        )
        .with_note(rule.note),
    )
}

/// Whether one of `syntax_errors` lies in the head of `node`, the part
/// before its body (its block, expression body, accessors or members), or
/// is an empty one, a missing token, at the head's end. A non-empty error
/// that starts at the end is about the text after the node. A token the
/// lexer rejected (`MR0017`) does not count: the parser reads the head as if
/// it were not there.
fn head_has_syntax_error(node: SyntaxNode<'_>, syntax_errors: &[SyntaxError]) -> bool {
    use SyntaxKind as K;
    let start = node.text_range().start();
    let end = node
        .children()
        .find(|child| {
            matches!(
                child.kind(),
                K::Block
                    | K::ExprBody
                    | K::AccessorList
                    | K::MemberList
                    | K::UnionBody
                    | K::EnumBody
            )
        })
        .map_or(node.text_range().end(), |body| body.text_range().start());
    syntax_errors.iter().any(|error| {
        let range = error.range;
        if error.code == codes::MR0017 {
            false
        } else if range.is_empty() {
            start <= range.start() && range.start() <= end
        } else {
            range.start() < end && start < range.end()
        }
    })
}

/// What `MR0115` says about a member.
struct MemberRule {
    /// The rule the member breaks, the diagnostic's message.
    message: &'static str,
    /// Where the diagnostic points.
    range: TextRange,
    /// What the member is, the label.
    label: &'static str,
    /// Where the member goes instead.
    note: &'static str,
}

/// Where the modifier `keyword` is first written on `node`, if it is: the
/// lookup for a node without its frame at hand, as the indexer of an
/// accessor, where [`ModifierFrame::modifier`] reads the walk's own.
fn modifier_of(node: SyntaxNode<'_>, keyword: SyntaxKind) -> Option<TextRange> {
    node.children()
        .filter_map(ast::Modifier::cast)
        .filter_map(written_modifier)
        .find(|&(kind, _)| kind == keyword)
        .map(|(_, range)| range)
}

/// Whether a node of the kind `node`, in one of the kind `container`, is
/// one that [`member_rule`] has a rule about: a member of an interface, an
/// extension or a union, or a macro anywhere. The walk of
/// [`modifier_diagnostics`] checks no other node without modifiers.
fn member_rule_may_apply(container: SyntaxKind, node: SyntaxKind) -> bool {
    use SyntaxKind as K;
    matches!(
        (container, node),
        (K::InterfaceDecl | K::ExtensionDecl | K::UnionDecl, _) | (_, K::MacroDecl)
    )
}

/// The rule of `MR0115` that `node`, in a node of the kind `container`,
/// breaks, whatever `const` written on it, if it breaks one. `modifier`
/// says where a modifier is first written on `node`; it is asked only of
/// the members a rule is about, so most nodes cost a match on two kinds.
fn member_rule(
    container: SyntaxKind,
    node: SyntaxNode<'_>,
    modifier: impl Fn(SyntaxKind) -> Option<TextRange>,
) -> Option<MemberRule> {
    use SyntaxKind as K;
    let static_kw = || modifier(K::StaticKw);
    let field = || {
        let name = ast::FieldDecl::cast(node)?.variable_declarator()?.name()?;
        Some(name.syntax().text_range())
    };
    let destructor = || {
        let destructor = ast::DestructorDecl::cast(node)?;
        let tilde = destructor.tilde_token()?.text_range();
        Some(tilde.cover(destructor.name()?.syntax().text_range()))
    };
    let operator = || {
        Some(
            ast::OperatorDecl::cast(node)?
                .operator_token()?
                .text_range(),
        )
    };
    let name = || {
        Some(
            node.children()
                .find_map(ast::Name::cast)?
                .syntax()
                .text_range(),
        )
    };
    let (message, range, label, note) = match (container, node.kind()) {
        (K::InterfaceDecl, K::FieldDecl) if static_kw().is_some() => (
            "an interface has no fields",
            field()?,
            "a static field",
            "a value each implementing type gives is a static abstract getter, as \
             `static abstract K: int;`; a value every implementation shares is a constant in \
             the namespace, next to the interface, as `const K: int = 1;`",
        ),
        (K::InterfaceDecl, K::FieldDecl) => (
            "an interface has no fields",
            field()?,
            "a field, since it has a value",
            "a value each implementing type gives is a getter, as `K: int;`, which may have a \
             default, as `K: int => 1;`; a value every implementation shares is a constant in \
             the namespace, next to the interface, as `const K: int = 1;`",
        ),
        (container, K::MacroDecl) if let Some(message) = macro_container(container) => (
            message,
            name()?,
            "a macro",
            "a macro is declared in a namespace, like a function, and found by name through \
             `using` directives",
        ),
        // A union's constants and static fields are its own (§6.10).
        (K::UnionDecl, K::FieldDecl) if static_kw().is_none() && modifier(K::ConstKw).is_none() => {
            (
                "a union has no instance fields",
                field()?,
                "a union has no fields besides its cases",
                "put the value in each case, or wrap the union in a struct, as \
                 `struct Drawn { Shape: Shape; Color: string; }`",
            )
        }
        (K::InterfaceDecl, K::DestructorDecl) => (
            "an interface has no destructors",
            destructor()?,
            "a destructor",
            "a destructor is declared in each type that needs one, which may implement the \
             interface",
        ),
        (K::InterfaceDecl, kind @ (K::FunctionDecl | K::GetterDecl | K::IndexerDecl))
            if modifier(K::AbstractKw).is_none()
                && let Some(range) = static_kw() =>
        {
            (
                "a static member of an interface is `static abstract`",
                range,
                match kind {
                    K::FunctionDecl => "a static method without `abstract`",
                    K::GetterDecl => "a static getter without `abstract`",
                    _ => "a static indexer without `abstract`",
                },
                "a member each implementing type gives is `static abstract`, as \
                 `static abstract M(): int;`, with a body for a default, as \
                 `static abstract M(): int => 1;`; a function every implementation shares goes \
                 in the namespace, next to the interface",
            )
        }
        (K::InterfaceDecl, kind) if let Some(declaration) = type_declaration(kind) => (
            "an interface has no nested types",
            name()?,
            declaration,
            "a type is declared in the namespace, next to the interface",
        ),
        (K::ExtensionDecl, K::FieldDecl) if static_kw().is_some() => (
            "an extension adds no fields",
            field()?,
            "a static field",
            "a static field is declared in the type itself, or in the namespace, next to the \
             extension, as `static mut K: int = 1;`",
        ),
        (K::ExtensionDecl, K::FieldDecl) => (
            "an extension adds no fields",
            field()?,
            "a field",
            "a field is declared in the type itself; an extension may add a getter, as \
             `K: int => 1;`, which computes its value on each read",
        ),
        (K::ExtensionDecl, K::DestructorDecl) => (
            "an extension adds no destructors",
            destructor()?,
            "a destructor",
            "a destructor is declared in the type itself",
        ),
        (K::ExtensionDecl, K::IndexerDecl) => (
            "an extension adds no indexers",
            ast::IndexerDecl::cast(node)?.this_token()?.text_range(),
            "an indexer",
            "an indexer is declared in the type itself; an extension may add a method that \
             takes the index, as `At(i: int): int`",
        ),
        (K::ExtensionDecl, K::AssocTypeDecl) => (
            "an extension adds no associated types",
            name()?,
            "an associated type",
            "an associated type is declared in an interface and bound by each type that \
             implements it",
        ),
        (K::ExtensionDecl, kind) if let Some(declaration) = type_declaration(kind) => (
            "an extension adds no nested types",
            name()?,
            declaration,
            "a type is declared in the namespace, next to the extension",
        ),
        (K::ExtensionDecl, kind) => {
            let (range, label) = match (kind, static_kw()) {
                (K::OperatorDecl, range) => {
                    (range.or_else(operator)?, "an operator is a static method")
                }
                (K::FunctionDecl, Some(range)) => (range, "a static method"),
                (K::GetterDecl, Some(range)) => (range, "a static getter"),
                _ => return None,
            };
            (
                "an extension adds no static members",
                range,
                label,
                "a static member is declared in the type itself, or as a function in the \
                 namespace, next to the extension",
            )
        }
        _ => return None,
    };
    Some(MemberRule {
        message,
        range,
        label,
        note,
    })
}

/// The rule that a declaration of the kind `container` breaks by holding a
/// macro, if it is a type, an interface or an extension: macros are
/// declared in a namespace (§9.2).
fn macro_container(container: SyntaxKind) -> Option<&'static str> {
    use SyntaxKind as K;
    Some(match container {
        K::StructDecl => "a struct has no macros",
        K::ClassDecl => "a class has no macros",
        K::UnionDecl => "a union has no macros",
        K::InterfaceDecl => "an interface has no macros",
        K::ExtensionDecl => "an extension adds no macros",
        _ => return None,
    })
}

/// What a declaration of the kind `kind` that declares a type (a struct, a
/// class, a union, an enum or an interface) is, with its article, if it
/// is one.
fn type_declaration(kind: SyntaxKind) -> Option<&'static str> {
    use SyntaxKind as K;
    Some(match kind {
        K::StructDecl => "a struct",
        K::ClassDecl => "a class",
        K::UnionDecl => "a union",
        K::EnumDecl => "an enum",
        K::InterfaceDecl => "an interface",
        _ => return None,
    })
}

/// The fix, described by `message`, that removes the keyword at `range` in
/// the text `text` of `file`, with the white space after it
/// ([`with_space_after`]).
pub(crate) fn remove_keyword(
    file: FileId,
    text: &str,
    range: TextRange,
    message: impl Into<String>,
) -> Fix {
    Fix::new(
        message,
        vec![TextEdit::replace(
            Span::new(file, with_space_after(text, range)),
            "",
        )],
    )
}

/// `range` with the white space after it in `text`: what a fix removes so
/// that a keyword goes without leaving its space behind. A comment after
/// it stays.
fn with_space_after(text: &str, range: TextRange) -> TextRange {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mora_diag::Diagnostic;

    use crate::test_db::{codes, diagnostics_of, messages};

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
        // A declaration's modifiers come before those of its parameters.
        let found =
            diagnostics_of("struct S {\n    static static F(ref ref n: int): int => n;\n}\n");
        assert_eq!(
            messages(&found),
            ["`static` is written twice", "`ref` is written twice"]
        );
    }

    #[test]
    fn const_on_a_getter_is_mr0113_with_a_fix_that_removes_it() {
        let text = "struct Q {\n    public const K: int => 1;\n    const static S: int => 2;\n    \
                    X: int;\n}\ninterface I {\n    const Name: string => \"i\";\n}\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0113"; 3]);
        assert_eq!(found[0].message, "`const` does not apply to a getter");
        assert_eq!(&text[found[0].primary.span.range], "const");
        assert_eq!(
            found[0].primary.message.as_deref(),
            Some("a getter computes its value on each read")
        );
        let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
        assert_eq!(fixed.remaining, Vec::<String>::new());
        assert_eq!(
            fixed.text,
            "struct Q {\n    public K: int => 1;\n    static S: int => 2;\n    \
             X: int;\n}\ninterface I {\n    Name: string => \"i\";\n}\n"
        );
        // Written twice, the first does not apply and the second repeats it.
        let text = "struct Q {\n    const const K: int => 1;\n}\n";
        assert_eq!(codes(&diagnostics_of(text)), ["MR0113", "MR0111"]);
        let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
        assert_eq!(fixed.text, "struct Q {\n    K: int => 1;\n}\n");
        // A constant is a field with `const`, in a type or a namespace.
        assert_eq!(
            codes(&diagnostics_of(
                "struct S {\n    public const C: int = 1;\n    static T: int => 2;\n}\n\
                 const N: int = 3;\n"
            )),
            Vec::<String>::new()
        );
    }

    #[test]
    fn const_on_any_declaration_but_a_field_is_mr0113_with_a_fix_that_removes_it() {
        let code = "it is code that runs, not a value";
        let ty = "it declares a type, not a value";
        let cases = [
            (
                "struct S {\n    public const M(): int => 1;\n}\n",
                "a method",
                code,
            ),
            ("interface I {\n    const M(): int;\n}\n", "a method", code),
            (
                "union U {\n    A;\n    const M(): int => 1;\n}\n",
                "a method",
                code,
            ),
            (
                "extension(x: int) {\n    const M(): int => x;\n}\n",
                "a method",
                code,
            ),
            ("const F(): int => 1;\n", "a function", code),
            (
                "class C {\n    public const C() {}\n}\n",
                "a constructor",
                code,
            ),
            ("class C {\n    const ~C() {}\n}\n", "a destructor", code),
            (
                "struct V {\n    public const static operator +(a: V, b: V): V => a;\n}\n",
                "an operator",
                code,
            ),
            (
                "struct S {\n    public const this[i: int]: int { get => i; }\n}\n",
                "an indexer",
                code,
            ),
            (
                "struct S {\n    public this[i: int]: int { const get => i; }\n}\n",
                "an accessor",
                code,
            ),
            ("const macro M() {}\n", "a macro", code),
            (
                "F(const x: int) {}\n",
                "a parameter",
                "a parameter takes its value from each call",
            ),
            (
                "extension(const x: int) {}\n",
                "a parameter",
                "a parameter takes its value from each call",
            ),
            ("const struct S {\n    public X: int;\n}\n", "a struct", ty),
            ("const class C {}\n", "a class", ty),
            ("const interface I {}\n", "an interface", ty),
            ("const union U { A }\n", "a union", ty),
            ("const enum E { A }\n", "an enum", ty),
            (
                "interface I {\n    const type E;\n}\n",
                "an associated type",
                ty,
            ),
            (
                "const extension(x: int) {}\n",
                "an extension",
                "it adds members to a type, not a value",
            ),
        ];
        for (text, declaration, reason) in cases {
            let found = diagnostics_of(text);
            assert_eq!(codes(&found), ["MR0113"], "{text}");
            assert_eq!(
                found[0].message,
                format!("`const` does not apply to {declaration}"),
                "{text}"
            );
            assert_eq!(&text[found[0].primary.span.range], "const", "{text}");
            assert_eq!(found[0].primary.message.as_deref(), Some(reason), "{text}");
            let fixed = crate::test_support::apply_fixes(text, diagnostics_of);
            assert_eq!(fixed.remaining, Vec::<String>::new(), "{text}");
            assert_eq!(fixed.text, text.replacen("const ", "", 1), "{text}");
        }
        // Written twice, the first does not apply and the second repeats it.
        let text = "struct Q {\n    const const M(): int => 1;\n}\n";
        assert_eq!(codes(&diagnostics_of(text)), ["MR0113", "MR0111"]);
        // Other modifiers on these declarations are not checked here.
        assert_eq!(
            codes(&diagnostics_of(
                "public static struct S {\n    public static M(ref x: int): int => x;\n}\n"
            )),
            Vec::<String>::new()
        );
    }

    #[test]
    fn const_on_a_field_of_an_interface_or_an_extension_is_mr0113_without_a_fix() {
        let interface = (
            "a member of an interface",
            "an interface has no fields",
            "a value every implementation shares is a constant in the namespace, next to the \
             interface, as `const K: int = 1;`; a value each implementing type gives is a \
             static abstract getter, as `static abstract K: int;`",
        );
        let extension = (
            "a member of an extension",
            "an extension adds no fields",
            "a constant goes in the namespace, next to the extension, or in the type itself, \
             as `const K: int = 1;`",
        );
        let cases = [
            ("interface I {\n    const K: int = 1;\n}\n", interface),
            (
                "interface I {\n    public const K: int = 1;\n}\n",
                interface,
            ),
            // Without a value, an interface reads it as a getter.
            ("interface I {\n    const K: int;\n}\n", interface),
            (
                "struct P {}\nextension(p: P) {\n    const K: int = 1;\n}\n",
                extension,
            ),
            // Without a value, `const` is wrong there all the same, and
            // `MR0114` is not reported as well.
            (
                "struct P {}\nextension(p: P) {\n    const K: int;\n}\n",
                extension,
            ),
            (
                "interface J {}\nextension<T>(p: T) where T : J {\n    const K: int = 1;\n}\n",
                extension,
            ),
        ];
        for (text, (declaration, reason, note)) in cases {
            let found = diagnostics_of(text);
            assert_eq!(codes(&found), ["MR0113"], "{text}");
            assert_eq!(
                found[0].message,
                format!("`const` does not apply to {declaration}"),
                "{text}"
            );
            assert_eq!(&text[found[0].primary.span.range], "const", "{text}");
            assert_eq!(found[0].primary.message.as_deref(), Some(reason), "{text}");
            assert_eq!(found[0].note.as_deref(), Some(note), "{text}");
            assert!(found[0].fixes.is_empty(), "{text}: {found:#?}");
        }
        // A getter with a body is a getter there too, and the fix removes
        // `const`; a constant of a union is one.
        let text = "interface I {\n    const K: int => 1;\n}\n\
                    union U {\n    A;\n    const K: int = 1;\n}\n";
        let found = diagnostics_of(text);
        assert_eq!(messages(&found), ["`const` does not apply to a getter"]);
        assert_eq!(found[0].fixes.len(), 1);
    }

    #[test]
    fn a_member_an_interface_an_extension_or_a_union_cannot_have_is_mr0115_without_a_fix() {
        const INTERFACE: &str = "an interface has no fields";
        const FIELDS: &str = "an extension adds no fields";
        const STATICS: &str = "an extension adds no static members";
        const STATIC_NOTE: &str = "a static member is declared in the type itself, or as a \
                                   function in the namespace, next to the extension";
        const INDEXER_NOTE: &str = "an indexer is declared in the type itself; an extension \
                                    may add a method that takes the index, as `At(i: int): int`";
        const INTERFACE_STATIC_NOTE: &str = "a member each implementing type gives is \
             `static abstract`, as `static abstract M(): int;`, with a body for a default, as \
             `static abstract M(): int => 1;`; a function every implementation shares goes in \
             the namespace, next to the interface";
        const MACRO_NOTE: &str = "a macro is declared in a namespace, like a function, and \
                                  found by name through `using` directives";
        const UNION: &str = "a union has no instance fields";
        const UNION_LABEL: &str = "a union has no fields besides its cases";
        const UNION_NOTE: &str = "put the value in each case, or wrap the union in a struct, \
                                  as `struct Drawn { Shape: Shape; Color: string; }`";
        const FIELD_NOTE: &str = "a field is declared in the type itself; an extension may \
                                  add a getter, as `K: int => 1;`, which computes its value on \
                                  each read";
        const STATIC_FIELD_NOTE: &str = "a static field is declared in the type itself, or in \
                                         the namespace, next to the extension, as \
                                         `static mut K: int = 1;`";
        let cases = [
            (
                "interface I {\n    K: int = 1;\n}\n",
                "K",
                INTERFACE,
                "a field, since it has a value",
                "a value each implementing type gives is a getter, as `K: int;`, which may \
                 have a default, as `K: int => 1;`; a value every implementation shares is a \
                 constant in the namespace, next to the interface, as `const K: int = 1;`",
            ),
            (
                "interface I {\n    public static mut K: int = 1;\n}\n",
                "K",
                INTERFACE,
                "a static field",
                "a value each implementing type gives is a static abstract getter, as \
                 `static abstract K: int;`; a value every implementation shares is a constant \
                 in the namespace, next to the interface, as `const K: int = 1;`",
            ),
            (
                "interface I {\n    ~I() {}\n}\n",
                "~I",
                "an interface has no destructors",
                "a destructor",
                "a destructor is declared in each type that needs one, which may implement \
                 the interface",
            ),
            (
                "struct P {}\nextension(p: P) {\n    K: int = 1;\n}\n",
                "K",
                FIELDS,
                "a field",
                FIELD_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    mut K: int;\n}\n",
                "K",
                FIELDS,
                "a field",
                FIELD_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    static K: int = 1;\n}\n",
                "K",
                FIELDS,
                "a static field",
                STATIC_FIELD_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    ~ P() {}\n}\n",
                "~ P",
                "an extension adds no destructors",
                "a destructor",
                "a destructor is declared in the type itself",
            ),
            (
                "struct P {}\nextension(p: P) {\n    public static Make(): int => 1;\n}\n",
                "static",
                STATICS,
                "a static method",
                STATIC_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    static Z: int => 1;\n}\n",
                "static",
                STATICS,
                "a static getter",
                STATIC_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    this[i: int]: int => i;\n}\n",
                "this",
                "an extension adds no indexers",
                "an indexer",
                INDEXER_NOTE,
            ),
            // A static indexer is an indexer all the same.
            (
                "struct P {}\nextension(p: P) {\n    static this[i: int]: int => i;\n}\n",
                "this",
                "an extension adds no indexers",
                "an indexer",
                INDEXER_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    type T = int;\n}\n",
                "T",
                "an extension adds no associated types",
                "an associated type",
                "an associated type is declared in an interface and bound by each type that \
                 implements it",
            ),
            (
                "struct P {}\nextension(p: P) {\n    struct In {}\n}\n",
                "In",
                "an extension adds no nested types",
                "a struct",
                "a type is declared in the namespace, next to the extension",
            ),
            (
                "struct P {}\nextension(p: P) {\n    public enum E { A }\n}\n",
                "E",
                "an extension adds no nested types",
                "an enum",
                "a type is declared in the namespace, next to the extension",
            ),
            (
                "interface I {\n    class C {}\n}\n",
                "C",
                "an interface has no nested types",
                "a class",
                "a type is declared in the namespace, next to the interface",
            ),
            (
                "interface I {\n    union U { A }\n}\n",
                "U",
                "an interface has no nested types",
                "a union",
                "a type is declared in the namespace, next to the interface",
            ),
            (
                "interface I {\n    interface J {}\n}\n",
                "J",
                "an interface has no nested types",
                "an interface",
                "a type is declared in the namespace, next to the interface",
            ),
            (
                "interface I {\n    public static M(): int => 1;\n}\n",
                "static",
                "a static member of an interface is `static abstract`",
                "a static method without `abstract`",
                INTERFACE_STATIC_NOTE,
            ),
            (
                "interface I {\n    static G: int => 1;\n}\n",
                "static",
                "a static member of an interface is `static abstract`",
                "a static getter without `abstract`",
                INTERFACE_STATIC_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    \
                 public static operator +(a: P, b: P): P => a;\n}\n",
                "static",
                STATICS,
                "an operator is a static method",
                STATIC_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    operator +(a: P, b: P): P => a;\n}\n",
                "operator",
                STATICS,
                "an operator is a static method",
                STATIC_NOTE,
            ),
            (
                "union U { A, B(X: int);\n    Color: string = \"black\";\n}\n",
                "Color",
                UNION,
                UNION_LABEL,
                UNION_NOTE,
            ),
            (
                "union U { A;\n    public mut Color: string;\n}\n",
                "Color",
                UNION,
                UNION_LABEL,
                UNION_NOTE,
            ),
            (
                "interface I {\n    static this[i: int]: int { get => i; }\n}\n",
                "static",
                "a static member of an interface is `static abstract`",
                "a static indexer without `abstract`",
                INTERFACE_STATIC_NOTE,
            ),
            (
                "struct S {\n    macro M(t: Type) {}\n}\n",
                "M",
                "a struct has no macros",
                "a macro",
                MACRO_NOTE,
            ),
            (
                "class C {\n    macro M(t: Type) {}\n}\n",
                "M",
                "a class has no macros",
                "a macro",
                MACRO_NOTE,
            ),
            (
                "union U { A;\n    macro M(t: Type) {}\n}\n",
                "M",
                "a union has no macros",
                "a macro",
                MACRO_NOTE,
            ),
            (
                "interface I {\n    macro M(t: Type) {}\n}\n",
                "M",
                "an interface has no macros",
                "a macro",
                MACRO_NOTE,
            ),
            (
                "struct P {}\nextension(p: P) {\n    public macro M(t: Type) {}\n}\n",
                "M",
                "an extension adds no macros",
                "a macro",
                MACRO_NOTE,
            ),
        ];
        for (text, at, message, label, note) in cases {
            let found = diagnostics_of(text);
            assert_eq!(codes(&found), ["MR0115"], "{text}: {found:#?}");
            assert_eq!(found[0].message, message, "{text}");
            assert_eq!(&text[found[0].primary.span.range], at, "{text}");
            assert_eq!(found[0].primary.message.as_deref(), Some(label), "{text}");
            assert_eq!(found[0].note.as_deref(), Some(note), "{text}");
            assert!(found[0].fixes.is_empty(), "{text}: {found:#?}");
        }
        // What an interface, an extension or a union may have, the same
        // members in a struct and a macro in the namespace are not
        // reported; nor is `MR0115` on top of `MR0113`, and a union
        // constant without a value is `MR0114`.
        let text = "interface I {\n    K: int;\n    static abstract S: int;\n    \
                    M(): int => 1;\n    static abstract operator +(a: I, b: I): I;\n    \
                    abstract static D(): int => 1;\n    static abstract E: int => 1;\n    \
                    type T : I;\n    this[i: int]: int { get; }\n    \
                    static abstract this[i: int]: int { get; }\n}\nmacro N(t: Type) {}\n\
                    struct P {\n    K: int = 1;\n    static S: int = 1;\n    ~P() {}\n    \
                    static M(): int => 1;\n    struct In {}\n    this[i: int]: int => i;\n}\n\
                    extension(p: P) {\n    G: int => 1;\n    public M(): int => 1;\n    \
                    const ~P() {}\n    const static K: int = 1;\n}\n\
                    interface J {\n    const static K: int = 1;\n}\n\
                    union U { A;\n    const C: int = 1;\n    static S: int = 1;\n    \
                    static mut T: int;\n    G: int => 1;\n}\n\
                    union V { A;\n    const N: int;\n}\n";
        assert_eq!(
            mora_syntax::parse(text).errors(),
            &[] as &[mora_syntax::SyntaxError],
            "{text}"
        );
        let found = diagnostics_of(text);
        assert_eq!(
            codes(&found),
            ["MR0113", "MR0113", "MR0113", "MR0114"],
            "{found:#?}"
        );
        // The fields the parser makes of a broken lambda are not judged; the
        // indexer it is in, whose head is whole, is.
        let text = "struct S {}\nextension(s: S) {\n    \
                    this[i: int]: int { get => (const x: int) => x; }\n}\n";
        assert!(!mora_syntax::parse(text).errors().is_empty(), "{text}");
        let found = diagnostics_of(text);
        assert_eq!(
            messages(&found),
            ["an extension adds no indexers"],
            "{found:#?}"
        );
    }

    #[test]
    fn const_on_a_member_that_cannot_be_there_without_it_is_mr0113_without_a_fix() {
        // Each member, without `const`, is `MR0115`, so removing `const`
        // would not give valid code: the note says why.
        let cases = [
            (
                "interface I {\n    const ~I() {}\n}\n",
                "a destructor",
                "an interface has no destructors; a destructor is declared in each type that \
                 needs one, which may implement the interface",
            ),
            (
                "interface I {\n    const struct N {}\n}\n",
                "a struct",
                "an interface has no nested types; a type is declared in the namespace, next to \
                 the interface",
            ),
            (
                "interface I {\n    const static M(): int => 1;\n}\n",
                "a method",
                "a static member of an interface is `static abstract`; a member each \
                 implementing type gives is `static abstract`, as `static abstract M(): int;`, \
                 with a body for a default, as `static abstract M(): int => 1;`; a function \
                 every implementation shares goes in the namespace, next to the interface",
            ),
            (
                "struct P {}\nextension(p: P) {\n    const this[i: int]: int { get => i; }\n}\n",
                "an indexer",
                "an extension adds no indexers; an indexer is declared in the type itself; an \
                 extension may add a method that takes the index, as `At(i: int): int`",
            ),
            (
                "struct P {}\nextension(p: P) {\n    const operator +(a: P, b: P): P => a;\n}\n",
                "an operator",
                "an extension adds no static members; a static member is declared in the type \
                 itself, or as a function in the namespace, next to the extension",
            ),
            (
                "struct P {}\nextension(p: P) {\n    const type E = int;\n}\n",
                "an associated type",
                "an extension adds no associated types; an associated type is declared in an \
                 interface and bound by each type that implements it",
            ),
            (
                "struct S {\n    const macro M(t: Type) {}\n}\n",
                "a macro",
                "a struct has no macros; a macro is declared in a namespace, like a function, \
                 and found by name through `using` directives",
            ),
            // An accessor of an indexer that cannot be there: the indexer
            // keeps its `MR0115`.
            (
                "struct P {}\nextension(p: P) {\n    this[i: int]: int { const get => i; }\n}\n",
                "an accessor",
                "an extension adds no indexers; an indexer is declared in the type itself; an \
                 extension may add a method that takes the index, as `At(i: int): int`",
            ),
            // The rule of an accessor's indexer reads the indexer's own
            // modifiers: `static` without `abstract`.
            (
                "interface I {\n    static this[i: int]: int { const get => i; }\n}\n",
                "an accessor",
                "a static member of an interface is `static abstract`; a member each \
                 implementing type gives is `static abstract`, as `static abstract M(): int;`, \
                 with a body for a default, as `static abstract M(): int => 1;`; a function \
                 every implementation shares goes in the namespace, next to the interface",
            ),
        ];
        for (text, declaration, note) in cases {
            let found = diagnostics_of(text);
            let mr0113: Vec<_> = found
                .iter()
                .filter(|d| d.code == mora_diag::codes::MR0113)
                .collect();
            assert_eq!(mr0113.len(), 1, "{text}: {found:#?}");
            assert_eq!(
                mr0113[0].message,
                format!("`const` does not apply to {declaration}"),
                "{text}"
            );
            assert_eq!(mr0113[0].note.as_deref(), Some(note), "{text}");
            assert!(mr0113[0].fixes.is_empty(), "{text}: {found:#?}");
        }
        let accessor = diagnostics_of(cases[7].0);
        assert_eq!(codes(&accessor), ["MR0115", "MR0113"]);
        // An accessor of an indexer that can be there keeps its fix.
        let found = diagnostics_of("struct S {\n    this[i: int]: int { const get => i; }\n}\n");
        assert_eq!(codes(&found), ["MR0113"]);
        assert_eq!(found[0].fixes.len(), 1);
        // A parameter of such an indexer is not one of its accessors: its
        // fix removes a mistake of its own, and the indexer keeps `MR0115`.
        let found = diagnostics_of(
            "struct P {}\nextension(p: P) {\n    this[const i: int]: int => i;\n}\n",
        );
        assert_eq!(codes(&found), ["MR0115", "MR0113"]);
        assert_eq!(found[1].message, "`const` does not apply to a parameter");
        assert_eq!(found[1].fixes.len(), 1);
    }

    /// The diagnostics of the modifier and member checks of `text`.
    fn member_family(text: &str) -> Vec<Diagnostic> {
        use mora_diag::codes;
        let family = [
            codes::MR0111,
            codes::MR0112,
            codes::MR0113,
            codes::MR0114,
            codes::MR0115,
        ];
        diagnostics_of(text)
            .into_iter()
            .filter(|d| family.contains(&d.code))
            .collect()
    }

    /// The diagnostics of the modifier and member checks that applying
    /// every fix of theirs to `text` leaves and `text` did not report, with
    /// both texts; `None` when there are none: a fix must not trade one of
    /// them for another. A diagnostic is told apart by its code, its message
    /// and the text it points at.
    fn member_errors_the_fixes_add(text: &str) -> Option<String> {
        let reported = |text: &str| -> BTreeSet<(String, String, String)> {
            member_family(text)
                .iter()
                .map(|d| {
                    (
                        d.code.to_string(),
                        d.message.clone(),
                        text[d.primary.span.range].to_owned(),
                    )
                })
                .collect()
        };
        let before = reported(text);
        let fixed = crate::test_support::apply_fixes(text, member_family).text;
        let after = reported(&fixed);
        let new: Vec<_> = after.difference(&before).collect();
        (!new.is_empty()).then(|| format!("{text}\n=> {fixed}\n{new:#?}"))
    }

    #[test]
    fn no_fix_of_a_modifier_leaves_a_member_error() {
        // Each member form with `const`, alone and with `static`, in each
        // declaration that holds members.
        let members = [
            "K: int = 1;",
            "K: int;",
            "G: int => 1;",
            "M(): int => 1;",
            "M(): int;",
            "C() {}",
            "~C() {}",
            "operator +(a: int, b: int): int => a;",
            "this[i: int]: int => i;",
            "this[i: int]: int { get => i; }",
            "this[i: int]: int { const get => i; }",
            "type E = int;",
            "type E;",
            "struct N {}",
            "class N {}",
            "union N { A }",
            "enum N { A }",
            "interface N {}",
            "macro Mac(t: Type) {}",
            "F(const x: int) {}",
        ];
        let containers = [
            "struct C {\n    {}\n}\n",
            "class C {\n    {}\n}\n",
            "union C { A;\n    {}\n}\n",
            "interface C {\n    {}\n}\n",
            "struct C {}\nextension(p: C) {\n    {}\n}\n",
            "{}\n",
        ];
        let mut failures = Vec::new();
        for container in containers {
            for member in members {
                for modifiers in ["const ", "const static ", "static const ", "public const "] {
                    let text = container.replace("{}", &format!("{modifiers}{member}"));
                    failures.extend(member_errors_the_fixes_add(&text));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    #[test]
    fn no_fix_in_the_ui_tests_leaves_a_member_error() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/ui");
        let mut stack = vec![root];
        let mut failures = Vec::new();
        let mut cases = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("the UI tests") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "mr") {
                    let text = std::fs::read_to_string(&path).expect("a case");
                    cases += 1;
                    let name = path.display();
                    let failure = member_errors_the_fixes_add(&text);
                    failures.extend(failure.map(|failure| format!("{name}: {failure}")));
                }
            }
        }
        assert!(cases > 50, "{cases}");
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    #[test]
    fn a_constant_without_a_value_is_mr0114_without_a_fix() {
        let text = "struct S {\n    public const K: int;\n    public X: int;\n}\n\
                    const N: int;\nstatic T: int;\n";
        let found = diagnostics_of(text);
        assert_eq!(codes(&found), ["MR0114"; 2]);
        assert_eq!(
            messages(&found),
            ["constant `K` has no value", "constant `N` has no value"]
        );
        assert_eq!(&text[found[0].primary.span.range], "K");
        assert_eq!(&text[found[1].primary.span.range], "N");
        assert_eq!(
            found[0].primary.message.as_deref(),
            Some("a constant is declared with its value")
        );
        assert!(found[0].fixes.is_empty(), "{found:#?}");
        // Written twice, `const` repeats and the constant still has no value.
        let found = diagnostics_of("struct S {\n    const const K: int;\n}\n");
        assert_eq!(codes(&found), ["MR0111", "MR0114"]);
        // A field the parser recovered is a syntax error alone, whatever it
        // holds after the error.
        for text in [
            "const G: int => 1;\n",
            "struct C {\n    const K: int { get => 1; }\n}\n",
            "struct C {\n    const K;\n}\n",
            "struct C {\n    const K: int, L: int;\n}\n",
        ] {
            assert!(!mora_syntax::parse(text).errors().is_empty(), "{text}");
            let found = diagnostics_of(text);
            assert_eq!(codes(&found), Vec::<String>::new(), "{text}: {found:#?}");
        }
        // An error that starts where a complete field ends is about the
        // text after it: the constant still has no value.
        let text = "struct S {\n    const K: int;)\n}\n";
        assert!(!mora_syntax::parse(text).errors().is_empty(), "{text}");
        assert_eq!(codes(&diagnostics_of(text)), ["MR0114"]);
        // So is one that ends where it starts.
        let text = "struct S {\n    )const K: int;\n}\n";
        assert!(!mora_syntax::parse(text).errors().is_empty(), "{text}");
        assert_eq!(codes(&diagnostics_of(text)), ["MR0114"]);
    }

    #[test]
    fn a_rejected_token_in_a_head_still_gets_its_own_diagnostics() {
        // The parser reads the head as if the token were not there, so the
        // constant still has no value.
        for text in [
            "public class P {\n    const K: int\\;\n}\n",
            "public class P {\n    const \\K: int;\n}\n",
        ] {
            assert_eq!(codes(&diagnostics_of(text)), ["MR0114"], "{text}");
        }
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

    /// The walk skips a node without modifiers only where no rule of
    /// `MR0115` can apply, which no diagnostic shows when it errs.
    #[test]
    fn member_rules_apply_to_the_members_of_three_declarations_and_to_macros() {
        use super::member_rule_may_apply;
        use mora_syntax::SyntaxKind as K;
        for container in [K::InterfaceDecl, K::ExtensionDecl, K::UnionDecl] {
            assert!(
                member_rule_may_apply(container, K::FieldDecl),
                "{container:?}"
            );
        }
        assert!(member_rule_may_apply(K::StructDecl, K::MacroDecl));
        assert!(!member_rule_may_apply(K::StructDecl, K::FieldDecl));
        assert!(!member_rule_may_apply(K::ClassDecl, K::FunctionDecl));
    }
}
