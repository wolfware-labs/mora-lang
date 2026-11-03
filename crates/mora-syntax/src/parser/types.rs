//! Types (§5), and a lookahead that recognizes a type without parsing it.

use super::{Input, MAX_DEPTH, Parser, describe, exprs};
use crate::SyntaxKind;

use SyntaxKind as K;

/// The keywords that name predefined types (§5.2, §5.3).
pub(crate) const PREDEFINED: &[SyntaxKind] = &[
    K::BoolKw,
    K::ByteKw,
    K::SbyteKw,
    K::ShortKw,
    K::UshortKw,
    K::IntKw,
    K::UintKw,
    K::LongKw,
    K::UlongKw,
    K::NintKw,
    K::NuintKw,
    K::FloatKw,
    K::DoubleKw,
    K::CharKw,
    K::StringKw,
];

/// Parses a type, or reports that one is missing.
pub(crate) fn ty(p: &mut Parser<'_>) {
    ty_with(p, true, true);
}

/// The element type of `new int[10]` and `stackalloc byte[256]`: the
/// brackets that follow belong to the allocation, not to the type.
pub(crate) fn element_ty(p: &mut Parser<'_>) {
    ty_with(p, false, true);
}

/// A member's type or a function's return type, which a `=>` body may
/// follow: there `(int, int) => ...` is a tuple type and a body, so a
/// function type must be written in parentheses (§5.8).
pub(crate) fn member_ty(p: &mut Parser<'_>) {
    ty_with(p, true, false);
}

fn ty_with(p: &mut Parser<'_>, arrays: bool, functions: bool) {
    let _ = p.nested_type(|p| ty_inner(p, arrays, functions));
}

fn ty_inner(p: &mut Parser<'_>, arrays: bool, functions: bool) {
    if !at_type_start(p) {
        p.error("expected a type");
        return;
    }
    let mut lhs = match p.current() {
        // `any IShape` (§8.4); `any` alone is an ordinary name.
        K::Ident if at_any(p.input(), p.pos()) => {
            let m = p.start();
            p.bump_remap(K::AnyKw);
            let t = p.start();
            path(p, true);
            t.complete(p, K::PathType);
            m.complete(p, K::AnyType)
        }
        kind if PREDEFINED.contains(&kind) => {
            let m = p.start();
            p.bump_any();
            m.complete(p, K::PredefinedType)
        }
        K::LParen => {
            let tuple = tuple_type(p);
            if functions && (p.at(K::FatArrow) || p.at(K::ThinArrow)) {
                // `(int, int) => bool` (§5.8).
                let m = tuple.precede(p);
                arrow(p);
                ty(p);
                if p.at_contextual("uses") {
                    uses_clause(p);
                }
                m.complete(p, K::FnType)
            } else {
                tuple
            }
        }
        K::WeakKw => {
            let m = p.start();
            p.bump(K::WeakKw);
            ty(p);
            m.complete(p, K::WeakType)
        }
        K::Dollar => {
            let m = p.start();
            exprs::splice(p);
            m.complete(p, K::SpliceType)
        }
        _ => {
            let m = p.start();
            path(p, true);
            m.complete(p, K::PathType)
        }
    };
    // `T?`, `T[]`, `T?[]`, `T[]?`.
    loop {
        if p.at(K::Question) {
            let m = lhs.precede(p);
            p.bump(K::Question);
            lhs = m.complete(p, K::OptionalType);
        } else if arrays && p.at(K::LBracket) && p.nth_at(1, K::RBracket) {
            let m = lhs.precede(p);
            p.bump(K::LBracket);
            p.bump(K::RBracket);
            lhs = m.complete(p, K::ArrayType);
        } else {
            break;
        }
    }
}

pub(crate) fn at_type_start(p: &Parser<'_>) -> bool {
    let kind = p.current();
    PREDEFINED.contains(&kind) || matches!(kind, K::Ident | K::LParen | K::WeakKw | K::Dollar)
}

/// Whether `any` at `at` starts an `any I` type: the word `any` followed by
/// a name.
pub(crate) fn at_any(input: &Input<'_>, at: usize) -> bool {
    input.text(at) == "any" && input.kind(at + 1) == K::Ident
}

/// `()`, `(int, string)`, `(x: int, y: string)`, and the parameters of a
/// function type.
fn tuple_type(p: &mut Parser<'_>) -> super::CompletedMarker {
    let m = p.start();
    p.bump(K::LParen);
    while !p.at(K::RParen) && !p.at_end() {
        let elem = p.start();
        if p.at(K::Ident) && p.nth(1) == K::Colon {
            name(p);
            annotation(p);
        } else {
            ty(p);
        }
        elem.complete(p, K::TupleTypeElem);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RParen);
    m.complete(p, K::TupleType)
}

/// The `=>` of a function type or a lambda. `->` is reserved for nothing
/// else, so it is reported with a fix to `=>` and taken in its place.
pub(crate) fn arrow(p: &mut Parser<'_>) {
    if p.at(K::ThinArrow) {
        let m = p.start();
        p.error_with_fix("expected `=>`", "write `=>`", "=>");
        p.bump(K::ThinArrow);
        m.complete(p, K::Error);
    } else {
        p.expect(K::FatArrow);
    }
}

/// `uses none`, `uses clock`, `uses (fs.read, net.outbound)` (§6.14).
pub(crate) fn uses_clause(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_remap(K::UsesKw);
    if !p.eat(K::NoneKw) {
        if p.at(K::LParen) {
            let list = p.start();
            p.bump(K::LParen);
            if p.at(K::RParen) {
                p.error("expected a capability");
            }
            while !p.at(K::RParen) && !p.at_end() {
                capability(p);
                if !p.eat(K::Comma) {
                    break;
                }
            }
            p.expect(K::RParen);
            list.complete(p, K::CapabilityList);
        } else if p.at(K::Ident) {
            capability(p);
        } else {
            p.error("expected `none`, a capability or `(`");
        }
    }
    m.complete(p, K::UsesClause);
}

/// `clock`, `fs.read`.
fn capability(p: &mut Parser<'_>) {
    let m = p.start();
    let path = p.start();
    loop {
        let segment = p.start();
        name_ref(p);
        segment.complete(p, K::PathSegment);
        if p.at(K::Dot) && p.nth(1) == K::Ident {
            p.bump(K::Dot);
        } else {
            break;
        }
    }
    path.complete(p, K::Path);
    m.complete(p, K::Capability);
}

/// `: Type` after a name or a parameter list.
pub(crate) fn annotation(p: &mut Parser<'_>) {
    p.bump(K::Colon);
    ty(p);
}

/// `: Type` after a member's name or a function's parameters (see
/// [`member_ty`]).
pub(crate) fn member_annotation(p: &mut Parser<'_>) {
    p.bump(K::Colon);
    member_ty(p);
}

/// `Name`: a declared identifier.
pub(crate) fn name(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(K::Ident);
    m.complete(p, K::Name);
}

/// `NameRef`: a use of an identifier.
pub(crate) fn name_ref(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(K::Ident);
    m.complete(p, K::NameRef);
}

/// A path: `List<int>`, `Std.Json.JsonValue`, `Core.Parse`. In a type,
/// `<` always opens generic arguments; in an expression, only when the
/// lookahead of §14.4 says so.
pub(crate) fn path(p: &mut Parser<'_>, in_type: bool) {
    let m = p.start();
    path_segment(p, in_type);
    // In an expression a path is one segment: later dots are member access.
    while in_type && p.at(K::Dot) && p.nth(1) == K::Ident {
        p.bump(K::Dot);
        path_segment(p, in_type);
    }
    m.complete(p, K::Path);
}

fn path_segment(p: &mut Parser<'_>, in_type: bool) {
    let m = p.start();
    name_ref(p);
    if p.at(K::Lt) && (in_type || generic_args_follow(p.input(), p.pos())) {
        generic_arg_list(p);
    }
    m.complete(p, K::PathSegment);
}

pub(crate) fn generic_arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::Lt);
    while !p.at(K::Gt) && !p.at_end() {
        ty(p);
        if !p.eat(K::Comma) {
            break;
        }
    }
    if !p.eat(K::Gt) {
        p.error(format!("expected {}", describe(K::Gt)));
    }
    m.complete(p, K::GenericArgList);
}

/// Whether the `<` at `at` opens generic arguments in an expression: the
/// arguments must be types and the token after the closing `>` one of
/// `( ) ] } : ; , . ? == != | ^ && || & [` (§14.4).
pub(crate) fn generic_args_follow(input: &Input<'_>, at: usize) -> bool {
    let Some(end) = scan_generic_args(input, at) else {
        return false;
    };
    matches!(
        input.kind(end),
        K::LParen
            | K::RParen
            | K::RBracket
            | K::RBrace
            | K::Colon
            | K::Semicolon
            | K::Comma
            | K::Dot
            | K::Question
            | K::EqEq
            | K::BangEq
            | K::Pipe
            | K::Caret
            | K::AmpAmp
            | K::PipePipe
            | K::Amp
            | K::LBracket
            | K::QuestionDot
    ) || end >= input.len()
}

/// The index after `<...>` at `at`, if it holds a list of types. Like
/// `generic_arg_list`, it takes an empty list and a trailing comma.
pub(crate) fn scan_generic_args(input: &Input<'_>, at: usize) -> Option<usize> {
    generic_args_at(input, at, 0)
}

/// [`scan_generic_args`] inside `depth` levels of types.
fn generic_args_at(input: &Input<'_>, at: usize, depth: usize) -> Option<usize> {
    if input.kind(at) != K::Lt {
        return None;
    }
    let mut i = at + 1;
    loop {
        if input.kind(i) == K::Gt {
            return Some(i + 1);
        }
        i = scan(input, i, true, depth)?;
        match input.kind(i) {
            K::Comma => i += 1,
            K::Gt => return Some(i + 1),
            _ => return None,
        }
    }
}

/// The index after a type that starts at `at`, if one does. Used to decide
/// between a declaration and an expression before parsing either.
pub(crate) fn scan_type(input: &Input<'_>, at: usize) -> Option<usize> {
    scan(input, at, true, 0)
}

/// Like [`scan_type`], but parentheses followed by `=>` end the type
/// instead of starting a function type: where a lambda can stand, as in
/// `((x) => x)(5)`, `(x) => x` is read as a lambda.
pub(crate) fn scan_type_no_fn(input: &Input<'_>, at: usize) -> Option<usize> {
    scan(input, at, false, 0)
}

/// After a `uses` bound at `at`, if there is one, the index after it:
/// `uses none`, `uses fs.read`, `uses (clock, env)`.
fn scan_uses(input: &Input<'_>, at: usize) -> usize {
    if input.text(at) != "uses" {
        return at;
    }
    let mut i = at + 1;
    if input.kind(i) == K::NoneKw {
        return i + 1;
    }
    let list = input.kind(i) == K::LParen;
    i += usize::from(list);
    while matches!(input.kind(i), K::Ident | K::Dot) || (list && input.kind(i) == K::Comma) {
        i += 1;
    }
    if list && input.kind(i) == K::RParen {
        i += 1;
    }
    i
}

/// The type at `at`, inside `depth` levels of types. Past [`MAX_DEPTH`]
/// the rest of the type is skipped without recursion, so that the parser
/// takes it as a type and reports `MR0018` there.
fn scan(input: &Input<'_>, at: usize, functions: bool, depth: usize) -> Option<usize> {
    if depth == MAX_DEPTH {
        return skip_type(input, at);
    }
    let depth = depth + 1;
    let mut i = match input.kind(at) {
        kind if PREDEFINED.contains(&kind) => at + 1,
        K::WeakKw => return scan(input, at + 1, functions, depth),
        K::Ident if at_any(input, at) => return scan(input, at + 1, functions, depth),
        // `$t`, `$..ts`, `$(expr)` (§9.3).
        K::Dollar => scan_splice(input, at)?,
        K::LParen => {
            // `()`, the unit type, or a tuple.
            let end = if input.kind(at + 1) == K::RParen {
                at + 2
            } else {
                let mut i = at + 1;
                loop {
                    // `x: int` or `int`.
                    if input.kind(i) == K::Ident && input.kind(i + 1) == K::Colon {
                        i += 2;
                    }
                    i = scan(input, i, true, depth)?;
                    match input.kind(i) {
                        // A trailing comma, as `tuple_type` takes: `(int, string,)`.
                        K::Comma if input.kind(i + 1) == K::RParen => break i + 2,
                        K::Comma => i += 1,
                        K::RParen => break i + 1,
                        _ => return None,
                    }
                }
            };
            // `(int) => bool`: the result type, and a `uses` bound after it,
            // end the function type.
            if functions && matches!(input.kind(end), K::FatArrow | K::ThinArrow) {
                return scan(input, end + 1, true, depth).map(|i| scan_uses(input, i));
            }
            end
        }
        K::Ident => {
            let mut i = at;
            loop {
                if input.kind(i) != K::Ident {
                    return None;
                }
                i += 1;
                if input.kind(i) == K::Lt {
                    i = generic_args_at(input, i, depth)?;
                }
                if input.kind(i) == K::Dot && input.kind(i + 1) == K::Ident {
                    i += 1;
                } else {
                    break i;
                }
            }
        }
        _ => return None,
    };
    loop {
        match input.kind(i) {
            K::Question => i += 1,
            K::LBracket if input.kind(i + 1) == K::RBracket => i += 2,
            _ => return Some(i),
        }
    }
}

/// The index after the tokens at `at` that a type can be made of, up to a
/// `,` or a closing bracket that is not theirs.
fn skip_type(input: &Input<'_>, at: usize) -> Option<usize> {
    let mut open = 0usize;
    let mut i = at;
    loop {
        match input.kind(i) {
            K::Lt | K::LParen | K::LBracket => open += 1,
            K::Gt | K::RParen | K::RBracket | K::Comma if open == 0 => break,
            K::Gt | K::RParen | K::RBracket => open -= 1,
            // A splice can hold an expression: take it whole.
            K::Dollar => {
                i = scan_splice(input, i)?;
                continue;
            }
            K::Ident | K::Dot | K::Question | K::Comma | K::Colon | K::WeakKw => {}
            // `(int) => bool uses none`.
            K::FatArrow | K::ThinArrow | K::NoneKw => {}
            kind if PREDEFINED.contains(&kind) => {}
            _ => return None,
        }
        i += 1;
    }
    (i > at).then_some(i)
}

/// The index after a splice `$t`, `$..ts` or `$(expr)` at `at` (§9.3).
pub(crate) fn scan_splice(input: &Input<'_>, at: usize) -> Option<usize> {
    if input.kind(at) != K::Dollar {
        return None;
    }
    let mut i = at + 1;
    if input.kind(i) == K::DotDot {
        i += 1;
    }
    match input.kind(i) {
        K::Ident => Some(i + 1),
        K::LParen => {
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
        _ => None,
    }
}
