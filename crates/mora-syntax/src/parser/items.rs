//! Compilation units, declarations and members (§4, §6, §8.2, §9.2).

use super::exprs;
use super::types::{self, name, name_ref, scan_type_no_fn};
use super::{Input, Marker, Parser, stmts};
use crate::SyntaxKind;

use SyntaxKind as K;

/// Keywords that are always modifiers (§6.1).
const MODIFIERS: &[SyntaxKind] = &[
    K::PublicKw,
    K::InternalKw,
    K::PrivateKw,
    K::StaticKw,
    K::MutKw,
    K::UniqueKw,
    K::SharedKw,
    K::RefKw,
    K::AsyncKw,
    K::LazyKw,
    K::ConstKw,
    K::IndirectKw,
    K::ExternKw,
    K::MoveKw,
    K::InKw,
    K::ParamsKw,
    K::AbstractKw,
];

/// Tokens a member list or the file can recover at: the start of the next
/// declaration.
const ITEM_RECOVERY: &[SyntaxKind] = &[
    K::At,
    K::PublicKw,
    K::InternalKw,
    K::PrivateKw,
    K::StaticKw,
    K::StructKw,
    K::ClassKw,
    K::UnionKw,
    K::EnumKw,
    K::InterfaceKw,
    K::MacroKw,
    K::UsingKw,
];

pub(crate) fn source_file(p: &mut Parser<'_>) {
    let m = p.start();
    // Whether an item or a statement has been seen: `using` directives
    // come before them (§4.2).
    let mut seen_item = false;
    while !p.at_end() {
        let before = p.pos();
        seen_item |= top_level(p, seen_item);
        if p.pos() == before {
            // Nothing could use the token: take it, so the loop moves on.
            let e = p.start();
            p.error("expected a declaration or a statement");
            p.bump_any();
            e.complete(p, K::Error);
        }
    }
    m.complete(p, K::SourceFile);
}

/// Parses one top-level element and returns whether it was an item or a
/// statement. A `using` directive after one is still parsed, with an error.
fn top_level(p: &mut Parser<'_>, seen_item: bool) -> bool {
    if p.at(K::UsingKw) || (p.at_contextual("global") && p.nth(1) == K::UsingKw) {
        if seen_item {
            p.error("`using` directives come before the declarations");
        }
        using_directive(p);
        return false;
    }
    if !declaration(p, Context::TopLevel) {
        stmts::statement(p);
    }
    true
}

/// `using X;`, `using static X;`, `using A = X;`, `global using X;` (§4.3).
fn using_directive(p: &mut Parser<'_>) {
    let m = p.start();
    if p.at_contextual("global") {
        p.bump_remap(K::GlobalKw);
    }
    p.bump(K::UsingKw);
    p.eat(K::StaticKw);
    if p.at(K::Ident) && p.nth(1) == K::Eq {
        let alias = p.start();
        name(p);
        p.bump(K::Eq);
        alias.complete(p, K::UsingAlias);
    }
    types::path(p, true);
    p.expect(K::Semicolon);
    m.complete(p, K::UsingDirective);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context<'t> {
    /// At the top of a file, where statements are allowed too.
    TopLevel,
    /// In the member list of a type, named `owner` (none in `code { }`),
    /// or a local function.
    Member { owner: Option<&'t str> },
    /// In the member list of an interface, where `Name: T;` declares a
    /// getter (§6.8).
    Interface,
}

/// Parses a declaration with its attributes and modifiers. At the top
/// level, returns `false` without taking anything when the next tokens are
/// a statement instead.
fn declaration(p: &mut Parser<'_>, cx: Context<'_>) -> bool {
    if cx == Context::TopLevel && !at_declaration_start(p) {
        return false;
    }
    let start = p.pos();
    let m = p.start();
    while p.at(K::At) {
        attribute(p);
    }
    let modifiers = modifiers(p);
    let member = cx != Context::TopLevel;
    match p.current() {
        K::StructKw => struct_or_class(p, m, K::StructKw, K::StructDecl),
        K::ClassKw => struct_or_class(p, m, K::ClassKw, K::ClassDecl),
        K::InterfaceKw => interface_decl(p, m),
        K::UnionKw => union_decl(p, m),
        K::EnumKw => enum_decl(p, m),
        K::MacroKw => macro_decl(p, m),
        K::Ident if !member && at_extension(p.input(), p.pos()) => extension_decl(p, m),
        K::Tilde if member => destructor(p, m),
        K::Ident if member && p.at_contextual("type") && p.nth(1) == K::Ident => assoc_type(p, m),
        K::OperatorKw => operator(p, m),
        K::ThisKw if member && p.nth(1) == K::LBracket => indexer(p, m, 0),
        _ => {
            // `@Cfg(Debug = true) Log(x);` at the top level (§6.11). There,
            // without modifiers, only a function has a shape, so tokens
            // taken without one are attributes before a statement.
            let statement = cx == Context::TopLevel && modifiers.is_empty();
            // `@Cfg(Debug = true) match (x) { ... }` is a statement, not a
            // function `match`, as it is without attributes (§14.4).
            if statement && exprs::at_match(p) {
                stmts::statement(p);
                m.complete(p, K::AttributedStmt);
                return true;
            }
            match shape(p, cx, !modifiers.is_empty()) {
                Some(shape) => member_with_shape(p, m, shape),
                _ if p.pos() == start => {
                    // Nothing taken: the caller reports and recovers.
                    m.abandon(p);
                    return false;
                }
                _ if statement => {
                    stmts::statement(p);
                    m.complete(p, K::AttributedStmt);
                }
                _ => {
                    p.error("expected a declaration after the attributes and modifiers");
                    m.complete(p, K::Error);
                }
            }
        }
    }
    true
}

/// Whether a top-level declaration starts here rather than a statement:
/// attributes, a modifier, a declaration keyword, or a function.
fn at_declaration_start(p: &Parser<'_>) -> bool {
    if p.at(K::At) || p.at_any(MODIFIERS) || at_contextual_modifier(p) {
        return true;
    }
    if p.at_any(&[
        K::StructKw,
        K::ClassKw,
        K::InterfaceKw,
        K::UnionKw,
        K::EnumKw,
        K::MacroKw,
    ]) {
        return true;
    }
    // `match (x) { ... }` is a statement, not a function `match` (§14.4).
    if exprs::at_match(p) {
        return false;
    }
    // `Add(a: int, b: int): int => a + b;`, `extension<T>(xs: List<T>) { }`
    let input = p.input();
    at_function(input, p.pos()) || at_extension(input, p.pos())
}

/// Whether a function declared name first starts at `at`: a name, its type
/// parameters and its parameters, then what only a declaration has after
/// them: a body, a return type, a `uses` bound or a `where` clause.
/// `Log(x);` is a call.
pub(crate) fn at_function(input: &Input<'_>, at: usize) -> bool {
    match after_signature(input, at) {
        Some(i) => {
            matches!(
                input.kind(i),
                K::LBrace | K::FatArrow | K::ThinArrow | K::Colon | K::WhereKw
            ) || (input.kind(i) == K::Ident && input.text(i) == "uses")
        }
        None => false,
    }
}

/// Whether an extension block starts at `at`: the word `extension`, type
/// parameters, the receiver in parentheses, then `{` or a `where` clause
/// (§6.9). `extension(x);` is a call.
fn at_extension(input: &Input<'_>, at: usize) -> bool {
    input.kind(at) == K::Ident
        && input.text(at) == "extension"
        && matches!(
            after_signature(input, at).map(|i| input.kind(i)),
            Some(K::LBrace | K::WhereKw)
        )
}

/// After a name at `at`, its optional generic arguments and a balanced
/// parenthesized list, the position of the next token.
fn after_signature(input: &Input<'_>, at: usize) -> Option<usize> {
    if input.kind(at) != K::Ident {
        return None;
    }
    let mut i = at + 1;
    if let Some(end) = types::scan_generic_args(input, i) {
        i = end;
    }
    if input.kind(i) != K::LParen {
        return None;
    }
    let mut depth = 0usize;
    loop {
        match input.kind(i) {
            K::LParen => depth += 1,
            K::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ if i >= input.len() => return None,
            _ => {}
        }
        i += 1;
    }
}

/// The contextual modifier `file` or `stored` here, when another word of
/// the declaration follows (§6.1, §6.9).
fn contextual_modifier(p: &Parser<'_>) -> Option<SyntaxKind> {
    if p.at_contextual("file") && (p.nth(1) == K::Ident || p.nth(1).is_keyword()) {
        Some(K::FileKw)
    } else if p.at_contextual("stored") && (p.nth(1) == K::Ident || MODIFIERS.contains(&p.nth(1))) {
        Some(K::StoredKw)
    } else {
        None
    }
}

fn at_contextual_modifier(p: &Parser<'_>) -> bool {
    contextual_modifier(p).is_some()
}

/// Takes modifiers and returns their kinds.
fn modifiers(p: &mut Parser<'_>) -> Vec<SyntaxKind> {
    let mut kinds = Vec::new();
    loop {
        let m = p.start();
        if let Some(kind) = contextual_modifier(p) {
            p.bump_remap(kind);
            kinds.push(kind);
        } else if p.at(K::MutKw) {
            kinds.push(K::MutKw);
            p.bump(K::MutKw);
            if at_write_scope(p) {
                write_scope(p);
            }
        } else if p.at_any(MODIFIERS) {
            kinds.push(p.current());
            p.bump_any();
        } else {
            m.abandon(p);
            return kinds;
        }
        m.complete(p, K::Modifier);
    }
}

/// One word in parentheses after `mut`: a write scope (§6.9).
fn at_write_scope(p: &Parser<'_>) -> bool {
    p.at(K::LParen) && p.nth(2) == K::RParen
}

/// `(private)`, `(ns)` or `(internal)`; any other word is reported.
fn write_scope(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::LParen);
    if p.at_any(&[K::PrivateKw, K::InternalKw]) {
        p.bump_any();
    } else if p.at_contextual("ns") {
        p.bump_remap(K::NsKw);
    } else {
        let e = p.start();
        p.error("expected `private`, `ns` or `internal`");
        p.bump_any();
        e.complete(p, K::Error);
    }
    p.bump(K::RParen);
    m.complete(p, K::WriteScope);
}

/// `@Json`, `@Cfg(Os = "linux")`, `@Allow("MR2001")` (§6.11).
pub(crate) fn attribute(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::At);
    types::path(p, true);
    if p.at(K::LParen) {
        let args = p.start();
        p.bump(K::LParen);
        while !p.at(K::RParen) && !p.at_end() {
            let arg = p.start();
            if p.at(K::Ident) && p.nth(1) == K::Eq {
                name_ref(p);
                p.bump(K::Eq);
            }
            exprs::expr(p);
            arg.complete(p, K::AttributeArg);
            if !p.eat(K::Comma) {
                break;
            }
        }
        p.expect(K::RParen);
        args.complete(p, K::AttributeArgList);
    }
    m.complete(p, K::Attribute);
}

/// The type declaration's name, type parameters, base list and `where`
/// clauses.
fn type_head(p: &mut Parser<'_>) {
    name(p);
    if p.at(K::Lt) {
        type_param_list(p);
    }
}

fn type_tail(p: &mut Parser<'_>) {
    if p.at(K::Colon) {
        base_list(p);
    }
    while p.at(K::WhereKw) {
        where_clause(p);
    }
}

/// A struct may have a parameter list, which makes it a positional struct
/// (§6.5).
fn struct_or_class(p: &mut Parser<'_>, m: Marker, keyword: SyntaxKind, kind: SyntaxKind) {
    p.bump(keyword);
    let owner = p.nth_text(0);
    type_head(p);
    if keyword == K::StructKw && p.at(K::LParen) {
        param_list(p);
    }
    type_tail(p);
    if !p.eat(K::Semicolon) {
        member_list(p, Some(owner));
    }
    m.complete(p, kind);
}

/// `extension<T>(list: List<T>) where T : IEquatable<T> { members }` (§6.9).
fn extension_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump_remap(K::ExtensionKw);
    if p.at(K::Lt) {
        type_param_list(p);
    }
    p.expect(K::LParen);
    param(p);
    p.expect(K::RParen);
    while p.at(K::WhereKw) {
        where_clause(p);
    }
    member_list(p, None);
    m.complete(p, K::ExtensionDecl);
}

fn interface_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::InterfaceKw);
    type_head(p);
    type_tail(p);
    members_in_braces(p, Context::Interface);
    m.complete(p, K::InterfaceDecl);
}

/// `union Json { Null, Bool(Value: bool), indirect Array(Items: List<Json>); members }`.
fn union_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::UnionKw);
    let owner = p.nth_text(0);
    type_head(p);
    type_tail(p);
    let body = p.start();
    if p.expect(K::LBrace) {
        while !p.at_any(&[K::RBrace, K::Semicolon]) && !p.at_end() {
            let case = p.start();
            while p.at(K::At) {
                attribute(p);
            }
            p.eat(K::IndirectKw);
            name(p);
            if p.at(K::LParen) {
                param_list(p);
            }
            case.complete(p, K::UnionCase);
            if !p.eat(K::Comma) {
                break;
            }
        }
        if p.eat(K::Semicolon) {
            members(p, Context::Member { owner: Some(owner) });
        }
        p.expect(K::RBrace);
    }
    body.complete(p, K::UnionBody);
    m.complete(p, K::UnionDecl);
}

/// `enum Color : byte { Red, Green, Blue = 10 }`.
fn enum_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::EnumKw);
    name(p);
    if p.at(K::Colon) {
        let base = p.start();
        p.bump(K::Colon);
        types::ty(p);
        base.complete(p, K::EnumBase);
    }
    let body = p.start();
    if p.expect(K::LBrace) {
        while !p.at(K::RBrace) && !p.at_end() {
            let member = p.start();
            while p.at(K::At) {
                attribute(p);
            }
            name(p);
            if p.eat(K::Eq) {
                exprs::expr(p);
            }
            member.complete(p, K::EnumMember);
            if !p.eat(K::Comma) {
                break;
            }
        }
        p.expect(K::RBrace);
    }
    body.complete(p, K::EnumBody);
    m.complete(p, K::EnumDecl);
}

/// `macro Name(params) adds T { ... }` (§9.2).
fn macro_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::MacroKw);
    name(p);
    param_list(p);
    if p.at_contextual("adds") {
        let adds = p.start();
        p.bump_remap(K::AddsKw);
        types::ty(p);
        while p.eat(K::Comma) {
            types::ty(p);
        }
        adds.complete(p, K::AddsClause);
    }
    stmts::block(p);
    m.complete(p, K::MacroDecl);
}

/// `{ members }` of the type named `owner`, or of a `code { }` quote.
pub(crate) fn member_list(p: &mut Parser<'_>, owner: Option<&str>) {
    members_in_braces(p, Context::Member { owner });
}

fn members_in_braces(p: &mut Parser<'_>, cx: Context<'_>) {
    let _ = p.nested(|p| member_list_inner(p, cx));
}

fn member_list_inner(p: &mut Parser<'_>, cx: Context<'_>) {
    let m = p.start();
    if p.expect(K::LBrace) {
        members(p, cx);
        p.expect(K::RBrace);
    }
    m.complete(p, K::MemberList);
}

/// Members up to the closing `}`, recovering at each member boundary.
fn members(p: &mut Parser<'_>, cx: Context<'_>) {
    while !p.at(K::RBrace) && !p.at_end() {
        let before = p.pos();
        // `$..members;` is a splice of members; `$f: int;` declares a field.
        if p.at(K::Dollar) && shape(p, cx, false).is_none() {
            let m = p.start();
            exprs::splice(p);
            p.eat(K::Semicolon);
            m.complete(p, K::SpliceMember);
        } else {
            declaration(p, cx);
        }
        if p.pos() == before {
            // Skip to something that can start a member, as one error.
            let e = p.start();
            p.error("expected a member");
            if p.at(K::LBrace) {
                skip_braces(p);
            } else {
                p.bump_any();
            }
            while !p.at_end()
                && !p.at(K::RBrace)
                && !p.at_any(ITEM_RECOVERY)
                && !p.at_any(MODIFIERS)
                && !types::at_type_start(p)
            {
                p.bump_any();
            }
            e.complete(p, K::Error);
        }
    }
}

/// What a member is, decided from the tokens before taking any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Constructor,
    Getter,
    Field,
    Indexer,
}

/// A member's kind and the segments of its dotted name (more than one for
/// an explicit implementation, `IShape.Area`).
#[derive(Clone, Copy)]
struct Shape {
    kind: Kind,
    segments: usize,
}

/// The member that starts here, or `None`. At the top
/// level a field needs `modifiers` (`const`, `static`): without them a
/// declaration is a local, which is a statement.
fn shape(p: &Parser<'_>, cx: Context<'_>, modifiers: bool) -> Option<Shape> {
    let input = p.input();
    let at = p.pos();
    let member = cx != Context::TopLevel;
    let fields = member || modifiers;
    let shape = |kind, segments| Some(Shape { kind, segments });
    // Name first: `Area(): float`, `Count: int`, `IList<int>.this[i: int]`.
    let (segments, name_at, end) = if p.at(K::Dollar) {
        let end = types::scan_splice(input, at)?;
        (1, at, end)
    } else {
        name_segments(p, at)
    };
    if segments > 0 {
        match input.kind(end) {
            K::Dot if input.kind(end + 1) == K::ThisKw && member => {
                return shape(Kind::Indexer, segments);
            }
            K::LParen | K::Lt => {
                if let Context::Member { owner: Some(owner) } = cx
                    && segments == 1
                    && input.text(name_at) == owner
                {
                    return shape(Kind::Constructor, 1);
                }
                if member || at_function(input, name_at) || p.at(K::Dollar) {
                    return shape(Kind::Function, segments);
                }
            }
            K::Colon => {
                let after = scan_type_no_fn(input, end + 1).map(|i| input.kind(i));
                if member && matches!(after, Some(K::FatArrow | K::ThinArrow)) {
                    return shape(Kind::Getter, segments);
                }
                // `IShape.Area: float { ... }`: an explicit implementation is
                // a getter, whose braces are reported when parsed.
                if member && segments > 1 && after == Some(K::LBrace) {
                    return shape(Kind::Getter, segments);
                }
                // `Sides: int;` in an interface: a getter without a default.
                if cx == Context::Interface && after == Some(K::Semicolon) {
                    return shape(Kind::Getter, segments);
                }
                if fields && segments == 1 {
                    return shape(Kind::Field, 1);
                }
            }
            K::Eq | K::Semicolon | K::Comma if fields && segments == 1 && !p.at(K::Dollar) => {
                // `Count;`: a field without its type, reported when parsed.
                return shape(Kind::Field, 1);
            }
            _ => {}
        }
    }
    None
}

fn member_with_shape(p: &mut Parser<'_>, m: Marker, shape: Shape) {
    if shape.kind == Kind::Field {
        variable_declarator(p, Declarators::Field);
        if p.at(K::LBrace) {
            accessor_body(p, "expected `;`");
            p.eat(K::Semicolon);
        } else {
            p.expect(K::Semicolon);
        }
        m.complete(p, K::FieldDecl);
        return;
    }
    match shape.kind {
        Kind::Function => function(p, m, shape.segments),
        Kind::Constructor => constructor(p, m),
        Kind::Getter => getter(p, m, shape.segments),
        _ => indexer(p, m, shape.segments),
    }
}

/// `{ ... }` after a field's or a getter's type: neither has accessors
/// (§6.9). The braces and an initializer after them are taken as one error.
fn accessor_body(p: &mut Parser<'_>, message: &str) {
    let e = p.start();
    p.error(message);
    skip_braces(p);
    if p.eat(K::Eq) {
        exprs::expr(p);
    }
    e.complete(p, K::Error);
}

/// Takes a `{` and everything up to its matching `}`.
fn skip_braces(p: &mut Parser<'_>) {
    let mut depth = 0usize;
    loop {
        match p.current() {
            K::LBrace => depth += 1,
            K::RBrace => depth -= 1,
            _ => {}
        }
        p.bump_any();
        if depth == 0 || p.at_end() {
            return;
        }
    }
}

/// The name of a function or a getter: a dotted explicit implementation
/// `IShape.Area` when there are several `segments`, or a splice.
fn member_name(p: &mut Parser<'_>, segments: usize) {
    if segments > 1 {
        explicit_impl(p, segments - 1);
    }
    if p.at(K::Dollar) {
        exprs::splice(p);
    } else {
        name(p);
    }
}

/// `Deposit(amount: decimal): decimal { ... }`, `Max<T>(a: T, b: T): T
/// where T : IComparable<T>`, `Tick() { }` (§6.1).
fn function(p: &mut Parser<'_>, m: Marker, segments: usize) {
    member_name(p, segments);
    if p.at(K::Lt) {
        type_param_list(p);
    }
    param_list(p);
    if p.at(K::Colon) {
        types::member_annotation(p);
    }
    if p.at_contextual("uses") {
        types::uses_clause(p);
    }
    while p.at(K::WhereKw) {
        where_clause(p);
    }
    body_or_semicolon(p);
    m.complete(p, K::FunctionDecl);
}

/// `IsEmpty: bool => Count == 0;`, `stored Label: string => ...;` (§6.9).
fn getter(p: &mut Parser<'_>, m: Marker, segments: usize) {
    member_name(p, segments);
    required_annotation(p, true);
    if p.at(K::LBrace) {
        accessor_body(p, "expected `=>`");
        p.eat(K::Semicolon);
    } else if !p.eat(K::Semicolon) {
        expr_body(p);
        p.expect(K::Semicolon);
    }
    m.complete(p, K::GetterDecl);
}

/// `: Type` where a declaration must have one. A `member`'s type takes a
/// function type only in parentheses (§5.8).
fn required_annotation(p: &mut Parser<'_>, member: bool) {
    if p.at(K::Colon) && member {
        types::member_annotation(p);
    } else if p.at(K::Colon) {
        types::annotation(p);
    } else {
        p.error("expected `:` and a type");
    }
}

/// The dotted name after the type at `at`: how many segments it has
/// (more than one means an explicit implementation, `IShape.Area`), the
/// index of the last one, the member's own name, and the index after it
/// and its generic arguments.
fn name_segments(p: &Parser<'_>, at: usize) -> (usize, usize, usize) {
    let input = p.input();
    let mut i = at;
    let mut segments = 0;
    while input.kind(i) == K::Ident {
        segments += 1;
        let mut j = i + 1;
        if let Some(end) = types::scan_generic_args(input, j) {
            j = end;
        }
        if input.kind(j) == K::Dot && input.kind(j + 1) == K::Ident {
            i = j + 1;
        } else {
            return (segments, i, j);
        }
    }
    (segments, i, i)
}

/// `IShape.` before the member name: the first `segments` segments of the
/// dotted name and the dot after them.
fn explicit_impl(p: &mut Parser<'_>, segments: usize) {
    let m = p.start();
    let path = p.start();
    for i in 0..segments {
        let segment = p.start();
        name_ref(p);
        if p.at(K::Lt) {
            types::generic_arg_list(p);
        }
        segment.complete(p, K::PathSegment);
        if i + 1 < segments {
            p.expect(K::Dot);
        }
    }
    path.complete(p, K::Path);
    p.expect(K::Dot);
    m.complete(p, K::ExplicitImpl);
}

/// Where a declarator is, which decides whether it needs a type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Declarators {
    /// Fields, constants and statics: the declarator has a type.
    Field,
    /// Locals, after `let` or `var`: a type is optional.
    Local,
}

/// `Count: int = 0` or, for a local, `x = 1`: one name per declaration.
pub(crate) fn variable_declarator(p: &mut Parser<'_>, at: Declarators) {
    let d = p.start();
    if p.at(K::Dollar) {
        exprs::splice(p);
    } else {
        name(p);
    }
    if at == Declarators::Field {
        required_annotation(p, true);
    } else if p.at(K::Colon) {
        types::annotation(p);
    }
    if p.eat(K::Eq) {
        exprs::expr(p);
    }
    d.complete(p, K::VariableDeclarator);
}

fn accessor_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::LBrace);
    while !p.at(K::RBrace) && !p.at_end() {
        let accessor = p.start();
        while p.at(K::At) {
            attribute(p);
        }
        modifiers(p);
        let kind = ["get", "set"]
            .iter()
            .zip([K::GetKw, K::SetKw])
            .find(|(word, _)| p.at_contextual(word))
            .map(|(_, kind)| kind);
        let Some(kind) = kind else {
            p.error("expected `get` or `set`");
            accessor.complete(p, K::Error);
            break;
        };
        p.bump_remap(kind);
        body_or_semicolon(p);
        accessor.complete(p, K::Accessor);
    }
    p.expect(K::RBrace);
    m.complete(p, K::AccessorList);
}

/// `public this[i: int]: T { ... }`; with `segments`, an explicit
/// implementation such as `IList<int>.this[i: int]: int`.
fn indexer(p: &mut Parser<'_>, m: Marker, segments: usize) {
    if segments > 0 {
        explicit_impl(p, segments);
    }
    p.expect(K::ThisKw);
    p.expect(K::LBracket);
    while !p.at(K::RBracket) && !p.at_end() {
        param(p);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RBracket);
    required_annotation(p, true);
    if p.at(K::LBrace) {
        accessor_list(p);
    } else {
        expr_body(p);
        p.expect(K::Semicolon);
    }
    m.complete(p, K::IndexerDecl);
}

/// `public Counter(name: string) : this(name, 1) { ... }` (§6.9).
fn constructor(p: &mut Parser<'_>, m: Marker) {
    name(p);
    param_list(p);
    if p.at(K::Colon) {
        let init = p.start();
        p.bump(K::Colon);
        p.expect(K::ThisKw);
        exprs::arg_list(p);
        init.complete(p, K::ConstructorInitializer);
    }
    body_or_semicolon(p);
    m.complete(p, K::ConstructorDecl);
}

fn destructor(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::Tilde);
    name(p);
    p.expect(K::LParen);
    p.expect(K::RParen);
    body_or_semicolon(p);
    m.complete(p, K::DestructorDecl);
}

/// The operators a type may declare (§6.9), longest first.
const OVERLOADABLE: &[SyntaxKind] = &[
    K::UShr,
    K::Shr,
    K::GtEq,
    K::PlusPercent,
    K::MinusPercent,
    K::StarPercent,
    K::PlusPlus,
    K::MinusMinus,
    K::EqEq,
    K::BangEq,
    K::LtEq,
    K::Shl,
    K::Plus,
    K::Minus,
    K::Star,
    K::Slash,
    K::Percent,
    K::Amp,
    K::Pipe,
    K::Caret,
    K::Bang,
    K::Tilde,
    K::Lt,
    K::Gt,
];

/// `public static operator +(a: Vec, b: Vec): Vec => ...` (§6.9).
fn operator(p: &mut Parser<'_>, m: Marker) {
    p.bump(K::OperatorKw);
    match OVERLOADABLE.iter().find(|&&kind| p.at(kind)) {
        Some(&kind) => p.bump(kind),
        None => p.error("expected an operator"),
    }
    param_list(p);
    if p.at(K::Colon) {
        types::member_annotation(p);
    }
    body_or_semicolon(p);
    m.complete(p, K::OperatorDecl);
}

/// `type Enumerator : IEnumerator<T>;`, `type Enumerator = ListEnumerator<T>;` (§8.7).
fn assoc_type(p: &mut Parser<'_>, m: Marker) {
    p.bump_remap(K::TypeKw);
    name(p);
    if p.at(K::Colon) {
        base_list(p);
    }
    if p.eat(K::Eq) {
        types::ty(p);
    }
    p.expect(K::Semicolon);
    m.complete(p, K::AssocTypeDecl);
}

pub(crate) fn param_list(p: &mut Parser<'_>) {
    let m = p.start();
    if p.expect(K::LParen) {
        while !p.at(K::RParen)
            && !p.at_end()
            && !p.at_any(&[K::LBrace, K::Semicolon, K::FatArrow, K::ThinArrow])
        {
            param(p);
            if !p.eat(K::Comma) {
                break;
            }
        }
        p.expect(K::RParen);
    }
    m.complete(p, K::ParamList);
}

/// `amount: decimal`, `ref p: Point`, `port: int = 80` (§10.3).
fn param(p: &mut Parser<'_>) {
    let m = p.start();
    while p.at(K::At) {
        attribute(p);
    }
    modifiers(p);
    name(p);
    required_annotation(p, false);
    if p.eat(K::Eq) {
        exprs::expr(p);
    }
    m.complete(p, K::Param);
}

fn type_param_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::Lt);
    while !p.at(K::Gt) && !p.at_end() {
        let param = p.start();
        while p.at(K::At) {
            attribute(p);
        }
        if p.at(K::InKw) || p.at(K::OutKw) {
            p.bump_any();
        }
        name(p);
        param.complete(p, K::TypeParam);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::Gt);
    m.complete(p, K::TypeParamList);
}

fn base_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::Colon);
    types::ty(p);
    while p.eat(K::Comma) {
        types::ty(p);
    }
    m.complete(p, K::BaseList);
}

/// `where T : IComparable<T>, allows move` (§8.2).
fn where_clause(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::WhereKw);
    name_ref(p);
    p.expect(K::Colon);
    loop {
        let c = p.start();
        if p.at_any(&[K::StructKw, K::ClassKw, K::UniqueKw]) && !p.nth_at(1, K::Lt) {
            p.bump_any();
            c.complete(p, K::KindConstraint);
        } else if p.at_contextual("allows") && matches!(p.nth(1), K::MoveKw | K::RefKw) {
            p.bump_remap(K::AllowsKw);
            if !p.eat(K::MoveKw) {
                p.bump(K::RefKw);
                p.expect(K::StructKw);
            }
            c.complete(p, K::AllowsConstraint);
        } else {
            types::ty(p);
            c.complete(p, K::TypeConstraint);
        }
        if !p.eat(K::Comma) {
            break;
        }
    }
    m.complete(p, K::WhereClause);
}

/// A body: a block, `=> expr;`, or `;` for a declaration without one.
fn body_or_semicolon(p: &mut Parser<'_>) {
    if p.at(K::LBrace) {
        stmts::block(p);
    } else if p.at(K::FatArrow) || p.at(K::ThinArrow) {
        expr_body(p);
        p.expect(K::Semicolon);
    } else {
        p.expect(K::Semicolon);
    }
}

fn expr_body(p: &mut Parser<'_>) {
    let m = p.start();
    types::arrow(p);
    exprs::expr(p);
    m.complete(p, K::ExprBody);
}

/// A local function in a body: modifiers such as `static` or `async`, a
/// name and the rest of a function (§7.11).
pub(crate) fn local_function(p: &mut Parser<'_>) {
    let m = p.start();
    modifiers(p);
    function(p, m, 1);
}
