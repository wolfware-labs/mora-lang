//! Patterns (§7.8). `or` binds loosest, then `and`, then `not` and the
//! relational patterns, then the rest.

use super::exprs::{self, pattern_operand};
use super::types::{self, PREDEFINED, scan_type};
use super::{CompletedMarker, Parser};
use crate::SyntaxKind;

use SyntaxKind as K;

pub(crate) fn pattern(p: &mut Parser<'_>) {
    if or_pattern(p).is_none() {
        p.error("expected a pattern");
    }
}

/// `when guard` after a pattern (§7.7).
pub(crate) fn when_clause(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_remap(K::WhenKw);
    exprs::guard(p);
    m.complete(p, K::WhenClause);
}

fn or_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = and_pattern(p)?;
    while p.at_contextual("or") {
        let m = lhs.precede(p);
        p.bump_remap(K::OrKw);
        if and_pattern(p).is_none() {
            p.error("expected a pattern");
        }
        lhs = m.complete(p, K::BinPattern);
    }
    Some(lhs)
}

fn and_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = unary_pattern(p)?;
    while p.at_contextual("and") {
        let m = lhs.precede(p);
        p.bump_remap(K::AndKw);
        if unary_pattern(p).is_none() {
            p.error("expected a pattern");
        }
        lhs = m.complete(p, K::BinPattern);
    }
    Some(lhs)
}

fn unary_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    p.nested(unary_pattern_inner).unwrap_or_else(Some)
}

fn unary_pattern_inner(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at_contextual("not") {
        let m = p.start();
        p.bump_remap(K::NotKw);
        if unary_pattern(p).is_none() {
            p.error("expected a pattern");
        }
        return Some(m.complete(p, K::NotPattern));
    }
    // `>=` and the others: `>` and `>=` come as pieces, so check `>=` first.
    if let Some(&op) = [K::LtEq, K::GtEq, K::Lt, K::Gt]
        .iter()
        .find(|&&op| p.at(op))
    {
        let m = p.start();
        p.bump(op);
        if pattern_operand(p).is_none() {
            p.error("expected a constant");
        }
        return Some(m.complete(p, K::RelationalPattern));
    }
    primary_pattern(p)
}

fn primary_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let input = p.input();
    let at = p.pos();
    if p.at_contextual("_") && !matches!(p.nth(1), K::Dot | K::LParen) {
        let m = p.start();
        p.bump_remap(K::Underscore);
        return Some(m.complete(p, K::DiscardPattern));
    }
    if p.at(K::LetKw) || p.at(K::VarKw) {
        let m = p.start();
        p.bump_any();
        designation(p, false);
        return Some(m.complete(p, K::BindingPattern));
    }
    if p.at(K::LParen) {
        return Some(paren_or_positional(p));
    }
    if p.at(K::LBracket) {
        return Some(list_pattern(p));
    }
    if p.at(K::LBrace) {
        let m = p.start();
        property_body(p);
        return Some(m.complete(p, K::PropertyPattern));
    }
    if let Some(end) = scan_type(input, at) {
        let next = input.kind(end);
        // `User u`, `Circle c`, `(int, int) pair`.
        if next == K::Ident && !is_pattern_word(p.nth_text(end - at)) {
            let m = p.start();
            types::ty(p);
            designation(p, false);
            return Some(m.complete(p, K::DeclarationPattern));
        }
        // `User { Name: "Ada" } u`.
        if next == K::LBrace {
            let m = p.start();
            types::ty(p);
            property_body(p);
            return Some(m.complete(p, K::PropertyPattern));
        }
        // `Circle(let r)`, `Shape.Rect(_, > 0)`: a path and its fields.
        if next == K::LParen && input.kind(at) == K::Ident && is_plain_path(input, at, end) {
            let m = p.start();
            types::path(p, true);
            paren_or_positional_fields(p);
            return Some(m.complete(p, K::CasePattern));
        }
        // Type syntax that cannot be an expression: `int`, `int[]`, `User?`,
        // `List<int>`, `(int, int)`.
        if type_only(input, at, end) {
            let m = p.start();
            types::ty(p);
            return Some(m.complete(p, K::TypePattern));
        }
    }
    // `0`, `"GET"`, `none`, `-1`, `Color.Red`, `None`.
    let m = p.start();
    if pattern_operand(p).is_none() {
        m.abandon(p);
        return None;
    }
    Some(m.complete(p, K::ConstantPattern))
}

/// The contextual words that continue a pattern rather than name a binding.
fn is_pattern_word(text: &str) -> bool {
    matches!(text, "and" | "or" | "when")
}

/// Whether the type at `at..end` is a dotted name without generic
/// arguments, `?` or `[]`, which a case pattern starts with.
fn is_plain_path(input: &super::Input<'_>, at: usize, end: usize) -> bool {
    (at..end).all(|i| matches!(input.kind(i), K::Ident | K::Dot))
}

/// Whether the type at `at..end` can only be a type: a predefined type
/// (unless member access follows) or one with generic arguments, `?` or
/// `[]`. Parentheses never get here: `(int, string)` is a
/// positional pattern of two type patterns.
fn type_only(input: &super::Input<'_>, at: usize, end: usize) -> bool {
    if PREDEFINED.contains(&input.kind(at)) {
        return input.kind(end) != K::Dot;
    }
    (at..end).any(|i| matches!(input.kind(i), K::Lt | K::Question | K::LBracket))
}

/// A name, `_` or `(a, b)` after `let` or `var`. In a statement (`typed`),
/// a name may have a type, `x: int`. In a pattern a name has none: `x: int`
/// would read as the end of a case label.
pub(crate) fn designation(p: &mut Parser<'_>, typed: bool) {
    if p.at(K::LParen) {
        tuple_designation(p, typed);
    } else if p.at_contextual("_") {
        let m = p.start();
        p.bump_remap(K::Underscore);
        m.complete(p, K::DiscardDesignation);
    } else {
        let m = p.start();
        types::name(p);
        if typed && p.at(K::Colon) {
            types::annotation(p);
        }
        m.complete(p, K::NameDesignation);
    }
}

pub(crate) fn tuple_designation(p: &mut Parser<'_>, typed: bool) {
    let m = p.start();
    p.bump(K::LParen);
    while !p.at(K::RParen) && !p.at_end() {
        designation(p, typed);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RParen);
    m.complete(p, K::TupleDesignation);
}

/// `(p)` or `(0, let y)`, `(X: 0, Y: let y)`. One unnamed pattern without a
/// comma is a parenthesized pattern.
fn paren_or_positional(p: &mut Parser<'_>) -> CompletedMarker {
    let named = p.nth(1) == K::Ident && p.nth(2) == K::Colon;
    if !named && single_pattern_in_parens(p) {
        let m = p.start();
        p.bump(K::LParen);
        pattern(p);
        p.expect(K::RParen);
        return m.complete(p, K::ParenPattern);
    }
    let m = p.start();
    fields(p, K::LParen, K::RParen);
    m.complete(p, K::PositionalPattern)
}

/// The fields of a case pattern: always positional.
fn paren_or_positional_fields(p: &mut Parser<'_>) {
    let m = p.start();
    fields(p, K::LParen, K::RParen);
    m.complete(p, K::PositionalPattern);
}

/// Whether the parentheses at the next token hold one pattern: no comma
/// at their top level.
fn single_pattern_in_parens(p: &Parser<'_>) -> bool {
    let input = p.input();
    let mut depth = 0usize;
    let mut i = p.pos();
    loop {
        match input.kind(i) {
            K::LParen | K::LBracket | K::LBrace => depth += 1,
            K::RParen | K::RBracket | K::RBrace => {
                depth -= 1;
                if depth == 0 {
                    // `()` is an empty positional pattern.
                    return i > p.pos() + 1;
                }
            }
            K::Comma if depth == 1 => return false,
            _ if i >= input.len() => return false,
            _ => {}
        }
        i += 1;
    }
}

/// `{ Name: "Ada", Age: > 18 } x`: the body and an optional designation.
fn property_body(p: &mut Parser<'_>) {
    fields(p, K::LBrace, K::RBrace);
    if p.at(K::Ident) && !is_pattern_word(p.nth_text(0)) || p.at(K::LParen) {
        designation(p, false);
    }
}

fn fields(p: &mut Parser<'_>, open: SyntaxKind, close: SyntaxKind) {
    p.bump(open);
    while !p.at(close) && !p.at_end() {
        let field = p.start();
        if p.at(K::Ident) && p.nth(1) == K::Colon {
            types::name_ref(p);
            p.bump(K::Colon);
        }
        pattern(p);
        field.complete(p, K::PatternField);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(close);
}

/// `[1, 2, .. let rest]`, `[_, _]`.
fn list_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::LBracket);
    while !p.at(K::RBracket) && !p.at_end() {
        if p.at(K::DotDot) {
            let slice = p.start();
            p.bump(K::DotDot);
            if !p.at(K::Comma) && !p.at(K::RBracket) {
                pattern(p);
            }
            slice.complete(p, K::SlicePattern);
        } else {
            pattern(p);
        }
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RBracket);
    m.complete(p, K::ListPattern)
}
