//! Expressions (§7), by Pratt parsing over the precedence table of §7.1.
//!
//! Each level has a binding power, tightest last. Binary operators,
//! `is`/`as`, `with`, ranges, the conditional operator and
//! assignment all go through [`expr_bp`]; unary operators, casts and
//! `await` through [`unary`]; member access, calls, indexing, `++`/`--` and
//! propagation through [`postfix`].

use super::types::{self, PREDEFINED, scan_type_no_fn};
use super::{CompletedMarker, Input, Parser, items, patterns, stmts};
use crate::SyntaxKind;

use SyntaxKind as K;

/// Tokens that are literals on their own (§3.5).
const LITERALS: &[SyntaxKind] = &[
    K::IntNumber,
    K::RealNumber,
    K::Char,
    K::String,
    K::RawString,
    K::TrueKw,
    K::FalseKw,
    K::NoneKw,
];

/// Binding powers, loosest first (§7.1).
const ASSIGN: u8 = 1;
const CONDITIONAL: u8 = 2;
const COALESCE: u8 = 3;
const RELATIONAL: u8 = 10;
const WITH: u8 = 14;
const RANGE: u8 = 15;

/// Binary operators and their binding power. `??` associates to the
/// right; the others to the left.
const BINARY: &[(SyntaxKind, u8)] = &[
    (K::QuestionQuestion, COALESCE),
    (K::PipePipe, 4),
    (K::AmpAmp, 5),
    (K::Pipe, 6),
    (K::Caret, 7),
    (K::Amp, 8),
    (K::EqEq, 9),
    (K::BangEq, 9),
    (K::Lt, RELATIONAL),
    (K::Gt, RELATIONAL),
    (K::LtEq, RELATIONAL),
    (K::GtEq, RELATIONAL),
    (K::Shl, 11),
    (K::Shr, 11),
    (K::UShr, 11),
    (K::Plus, 12),
    (K::Minus, 12),
    (K::PlusPercent, 12),
    (K::MinusPercent, 12),
    (K::Star, 13),
    (K::Slash, 13),
    (K::Percent, 13),
    (K::StarPercent, 13),
];

/// Assignment operators, longest first so `>>>=` wins over `>>=`.
const ASSIGNMENTS: &[SyntaxKind] = &[
    K::UShrEq,
    K::ShrEq,
    K::Eq,
    K::PlusEq,
    K::MinusEq,
    K::StarEq,
    K::SlashEq,
    K::PercentEq,
    K::AmpEq,
    K::PipeEq,
    K::CaretEq,
    K::ShlEq,
    K::QuestionQuestionEq,
    K::PlusPercentEq,
    K::MinusPercentEq,
    K::StarPercentEq,
];

/// Prefix operators (§7.1); `^` is an index from the end (§7.13).
const PREFIX: &[SyntaxKind] = &[
    K::Plus,
    K::Minus,
    K::Bang,
    K::Tilde,
    K::Caret,
    K::PlusPlus,
    K::MinusMinus,
];

pub(crate) fn expr(p: &mut Parser<'_>) {
    required_expr(p, 0);
}

/// An expression whose operators bind at least as tightly as `min`, or an
/// error that it is missing.
fn required_expr(p: &mut Parser<'_>, min: u8) {
    if expr_bp(p, min).is_none() {
        p.error("expected an expression");
    }
}

/// An expression above the conditional operator: an operand of a
/// relational pattern or a constant pattern (§7.8), where `and`, `or`,
/// `when` and `=>` end it.
pub(crate) fn pattern_operand(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    p.lambdas(false, |p| expr_bp(p, RELATIONAL + 1))
}

/// A `when` guard: a full expression, but `=>` ends it.
pub(crate) fn guard(p: &mut Parser<'_>) {
    p.lambdas(false, expr);
}

/// Whether an expression can start at the next token.
pub(crate) fn at_expr_start(p: &Parser<'_>) -> bool {
    let kind = p.current();
    LITERALS.contains(&kind)
        || PREFIX.contains(&kind)
        || PREDEFINED.contains(&kind)
        || matches!(
            kind,
            K::Ident
                | K::InterpStart
                | K::ThisKw
                | K::LParen
                | K::LBracket
                | K::NewKw
                | K::DefaultKw
                | K::SizeofKw
                | K::Hash
                | K::Dollar
                | K::AwaitKw
                | K::StackallocKw
                | K::DotDot
                | K::AsyncKw
                | K::StaticKw
        )
}

/// The binary operator at the next token, longest first, so `>>` wins
/// over `>`; `None` before an assignment such as `>>=`.
fn binary_op(p: &Parser<'_>) -> Option<(SyntaxKind, u8)> {
    if ASSIGNMENTS.iter().any(|&kind| p.at(kind)) {
        return None;
    }
    const LONGEST_FIRST: &[SyntaxKind] = &[K::UShr, K::Shr, K::GtEq];
    let kind = LONGEST_FIRST
        .iter()
        .copied()
        .find(|&kind| p.at(kind))
        .or_else(|| BINARY.iter().map(|(k, _)| *k).find(|&kind| p.at(kind)))?;
    BINARY.iter().copied().find(|(k, _)| *k == kind)
}

/// An expression whose operators bind at least as tightly as `min`, one
/// level deeper (§14.4).
fn expr_bp(p: &mut Parser<'_>, min: u8) -> Option<CompletedMarker> {
    p.nested(|p| expr_bp_inner(p, min)).unwrap_or_else(Some)
}

fn expr_bp_inner(p: &mut Parser<'_>, min: u8) -> Option<CompletedMarker> {
    // A range does not associate: `a..b..c` takes one `..`, then stops.
    let mut is_range = p.at(K::DotDot) && min <= RANGE;
    let mut lhs = if is_range {
        // `..y` and `..`.
        let m = p.start();
        p.bump(K::DotDot);
        // The end is optional: without one, this takes nothing.
        expr_bp(p, RANGE + 1);
        m.complete(p, K::RangeExpr)
    } else {
        unary(p)?
    };
    loop {
        // `x..y`, `x..`.
        if p.at(K::DotDot) && min <= RANGE && !is_range {
            let m = lhs.precede(p);
            p.bump(K::DotDot);
            expr_bp(p, RANGE + 1);
            lhs = m.complete(p, K::RangeExpr);
            is_range = true;
            continue;
        }
        if min <= WITH && p.at_contextual("with") && p.nth(1) == K::LBrace {
            let m = lhs.precede(p);
            p.bump_remap(K::WithKw);
            object_initializer(p);
            lhs = m.complete(p, K::WithExpr);
            continue;
        }
        if min <= RELATIONAL && p.at(K::IsKw) {
            let m = lhs.precede(p);
            p.bump(K::IsKw);
            patterns::pattern(p);
            lhs = m.complete(p, K::IsExpr);
            continue;
        }
        if min <= RELATIONAL && p.at(K::AsKw) {
            let m = lhs.precede(p);
            p.bump(K::AsKw);
            types::ty(p);
            lhs = m.complete(p, K::AsExpr);
            continue;
        }
        if let Some((op, power)) = binary_op(p)
            && power >= min
        {
            let m = lhs.precede(p);
            p.bump(op);
            let next = if op == K::QuestionQuestion {
                power
            } else {
                power + 1
            };
            required_expr(p, next);
            lhs = m.complete(p, K::BinExpr);
            continue;
        }
        if min <= CONDITIONAL && p.at(K::Question) && conditional_follows(p.input(), p.pos()) {
            // Each arm is a whole expression, assignment included:
            // `ok ? x = 1 : y = 2` assigns in either arm.
            let m = lhs.precede(p);
            p.bump(K::Question);
            required_expr(p, ASSIGN);
            p.expect(K::Colon);
            required_expr(p, ASSIGN);
            lhs = m.complete(p, K::ConditionalExpr);
            continue;
        }
        if min <= ASSIGN
            && let Some(&op) = ASSIGNMENTS.iter().find(|&&kind| p.at(kind))
        {
            let m = lhs.precede(p);
            p.bump(op);
            required_expr(p, ASSIGN);
            lhs = m.complete(p, K::AssignExpr);
            continue;
        }
        return Some(lhs);
    }
}

/// §7.4: after an operand, `?` begins a conditional expression when a `:`
/// at the same nesting depth follows before the expression ends (`;`, `)`,
/// `]`, `}` or `,` at depth zero); otherwise it is propagation.
pub(crate) fn conditional_follows(input: &Input<'_>, at: usize) -> bool {
    // `r? : d` in an arm: a conditional cannot have an empty first arm.
    if matches!(
        input.kind(at + 1),
        K::Colon | K::RParen | K::Semicolon | K::Comma
    ) {
        return false;
    }
    let mut depth = 0usize;
    let mut i = at + 1;
    while i < input.len() {
        match input.kind(i) {
            K::LParen | K::LBracket | K::LBrace | K::HoleOpen => depth += 1,
            K::RParen | K::RBracket | K::RBrace | K::HoleClose => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            K::Semicolon | K::Comma if depth == 0 => return false,
            K::Colon if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn unary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    unary_with(p, true)
}

/// A unary expression. Without `propagate` (the operand of `await`), no
/// postfix `?` is taken at any depth of it, so `await -f()?` is
/// `(await -f())?`.
fn unary_with(p: &mut Parser<'_>, propagate: bool) -> Option<CompletedMarker> {
    if PREFIX.iter().any(|&kind| p.at(kind)) {
        let m = p.start();
        p.bump_any();
        required_operand(p, propagate);
        return Some(m.complete(p, K::PrefixExpr));
    }
    if p.at(K::AwaitKw) {
        // `await e?` is `(await e)?` (§7.4): the operand takes no `?`, and
        // propagation applies to the awaited value.
        let m = p.start();
        p.bump(K::AwaitKw);
        required_operand(p, false);
        let awaited = m.complete(p, K::AwaitExpr);
        return Some(if propagate {
            propagation(p, awaited)
        } else {
            awaited
        });
    }
    if p.at(K::LParen) && at_cast(p) {
        let m = p.start();
        p.bump(K::LParen);
        types::ty(p);
        p.bump(K::RParen);
        required_operand(p, propagate);
        return Some(m.complete(p, K::CastExpr));
    }
    let lhs = primary(p)?;
    Some(postfix(p, lhs, propagate))
}

/// The operand of a prefix operator, `await` or a cast, one level deeper,
/// or an error that it is missing.
fn required_operand(p: &mut Parser<'_>, propagate: bool) {
    let operand = p.nested(|p| unary_with(p, propagate)).unwrap_or_else(Some);
    if operand.is_none() {
        p.error("expected an expression");
    }
}

/// Whether `(T)x` is a cast (§14.4): the parentheses hold a type, and what
/// follows is an identifier, a literal, `(`, `~`, `!` or a keyword that can
/// start an expression; after a predefined type, any operand.
fn at_cast(p: &Parser<'_>) -> bool {
    let input = p.input();
    let at = p.pos();
    let Some(end) = scan_type_no_fn(input, at + 1) else {
        return false;
    };
    if input.kind(end) != K::RParen {
        return false;
    }
    let next = input.kind(end + 1);
    let predefined = PREDEFINED.contains(&input.kind(at + 1)) && end == at + 2;
    if predefined {
        return end + 1 < input.len()
            && !matches!(next, K::RParen | K::Semicolon | K::Comma | K::Dot);
    }
    LITERALS.contains(&next)
        || matches!(
            next,
            K::Ident
                | K::LParen
                | K::Tilde
                | K::Bang
                | K::ThisKw
                | K::NewKw
                | K::DefaultKw
                | K::SizeofKw
                | K::AwaitKw
                | K::StackallocKw
                | K::InterpStart
        )
        || (PREDEFINED.contains(&next))
}

/// Whether a lambda starts here: `x =>`, `(…) =>`, `async`, `static`, or a
/// capture list `[weak …]` / `[move …]`.
fn at_lambda(p: &Parser<'_>) -> bool {
    if !p.lambdas_allowed() {
        return false;
    }
    match p.current() {
        K::Ident => matches!(p.nth(1), K::FatArrow | K::ThinArrow),
        K::AsyncKw | K::StaticKw => true,
        K::LBracket => matches!(p.nth(1), K::WeakKw | K::MoveKw),
        K::LParen => {
            let input = p.input();
            input
                .after_parens(p.pos())
                .is_some_and(|end| matches!(input.kind(end), K::FatArrow | K::ThinArrow))
        }
        _ => false,
    }
}

/// `x => x * 2`, `(x: int, y: int) => x + y`, `async (req: Request) => ...`,
/// `[weak this] () => this?.Refresh()` (§7.6).
fn lambda(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(K::LBracket) {
        let captures = p.start();
        p.bump(K::LBracket);
        while !p.at(K::RBracket) && !p.at_end() {
            let capture = p.start();
            if p.at(K::WeakKw) || p.at(K::MoveKw) {
                p.bump_any();
            } else {
                p.error("expected `weak` or `move`");
            }
            if !p.eat(K::ThisKw) {
                types::name_ref(p);
            }
            capture.complete(p, K::Capture);
            if !p.eat(K::Comma) {
                break;
            }
        }
        p.expect(K::RBracket);
        captures.complete(p, K::CaptureList);
    }
    while p.at(K::AsyncKw) || p.at(K::StaticKw) {
        let modifier = p.start();
        p.bump_any();
        modifier.complete(p, K::Modifier);
    }
    if p.at(K::LParen) {
        let list = p.start();
        p.bump(K::LParen);
        while !p.at(K::RParen) && !p.at_end() {
            lambda_param(p);
            if !p.eat(K::Comma) {
                break;
            }
        }
        p.expect(K::RParen);
        list.complete(p, K::LambdaParamList);
    } else {
        let param = p.start();
        types::name(p);
        param.complete(p, K::LambdaParam);
    }
    types::arrow(p);
    if p.at(K::LBrace) {
        stmts::block(p);
    } else {
        expr(p);
    }
    m.complete(p, K::LambdaExpr)
}

/// `x: int`, `x`, `ref x: int`, `x: int = 0`.
fn lambda_param(p: &mut Parser<'_>) {
    let m = p.start();
    while p.at_any(&[K::RefKw, K::InKw, K::MoveKw, K::ParamsKw]) {
        let modifier = p.start();
        p.bump_any();
        modifier.complete(p, K::Modifier);
    }
    types::name(p);
    if p.at(K::Colon) {
        types::annotation(p);
    }
    if p.eat(K::Eq) {
        expr(p);
    }
    m.complete(p, K::LambdaParam);
}

fn primary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if at_lambda(p) {
        return Some(lambda(p));
    }
    let kind = p.current();
    if LITERALS.contains(&kind) {
        let m = p.start();
        p.bump_any();
        return Some(m.complete(p, K::LiteralExpr));
    }
    if PREDEFINED.contains(&kind) {
        // `int.Parse(...)`.
        let m = p.start();
        let t = p.start();
        p.bump_any();
        t.complete(p, K::PredefinedType);
        return Some(m.complete(p, K::PredefinedTypeExpr));
    }
    let m = p.start();
    let done = match kind {
        K::InterpStart => {
            m.abandon(p);
            return Some(interp_string(p));
        }
        K::Ident if p.at_contextual("nameof") && p.nth(1) == K::LParen => {
            p.bump_remap(K::NameofKw);
            p.bump(K::LParen);
            expr(p);
            p.expect(K::RParen);
            m.complete(p, K::NameofExpr)
        }
        K::Ident if at_match(p) => {
            match_head(p);
            m.complete(p, K::MatchExpr)
        }
        K::Ident if p.at_contextual("code") && matches!(p.nth(1), K::LBrace | K::LParen) => {
            quote(p);
            m.complete(p, K::QuoteExpr)
        }
        K::Ident => {
            types::path(p, false);
            m.complete(p, K::PathExpr)
        }
        K::ThisKw => {
            p.bump(K::ThisKw);
            m.complete(p, K::ThisExpr)
        }
        K::LParen => {
            m.abandon(p);
            return Some(paren_or_tuple(p));
        }
        K::LBracket => {
            m.abandon(p);
            return Some(collection(p));
        }
        K::NewKw => {
            m.abandon(p);
            return Some(new_expr(p));
        }
        K::StackallocKw => {
            m.abandon(p);
            return Some(stackalloc(p));
        }
        K::DefaultKw => {
            p.bump(K::DefaultKw);
            if p.eat(K::LParen) {
                types::ty(p);
                p.expect(K::RParen);
            }
            m.complete(p, K::DefaultExpr)
        }
        K::SizeofKw => {
            p.bump(K::SizeofKw);
            p.expect(K::LParen);
            types::ty(p);
            p.expect(K::RParen);
            m.complete(p, K::SizeofExpr)
        }
        K::Hash => {
            p.bump(K::Hash);
            types::name_ref(p);
            arg_list(p);
            m.complete(p, K::MacroCallExpr)
        }
        K::Dollar => {
            splice(p);
            m.complete(p, K::SpliceExpr)
        }
        _ => {
            m.abandon(p);
            return None;
        }
    };
    Some(done)
}

/// `code { members }`, `code(expr)`, `code(type)` (§9.3). Inside the
/// parentheses, a type that cannot be an expression (a predefined type, a
/// tuple, generic arguments, `?` or `[]`) is a type; anything else an
/// expression.
fn quote(p: &mut Parser<'_>) {
    p.bump_remap(K::CodeKw);
    if p.at(K::LBrace) {
        items::member_list(p, None);
        return;
    }
    p.bump(K::LParen);
    let input = p.input();
    let at = p.pos();
    let is_type = scan_type_no_fn(input, at).is_some_and(|end| {
        input.kind(end) == K::RParen
            && (PREDEFINED.contains(&input.kind(at))
                || input.kind(at) == K::LParen
                || types::at_any(input, at)
                || (at..end).any(|i| matches!(input.kind(i), K::Lt | K::Question | K::LBracket)))
    });
    if is_type {
        types::ty(p);
    } else {
        expr(p);
    }
    p.expect(K::RParen);
}

/// `stackalloc byte[256]` (§5.9).
fn stackalloc(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::StackallocKw);
    types::element_ty(p);
    array_size(p);
    m.complete(p, K::StackallocExpr)
}

/// `[size]` after the element type of `new` or `stackalloc`.
fn array_size(p: &mut Parser<'_>) {
    p.expect(K::LBracket);
    expr(p);
    p.expect(K::RBracket);
}

/// Member access, calls, indexing, `x++`, `x--` and, unless `propagate`
/// is off (inside `await`), the propagation `x?`.
fn postfix(p: &mut Parser<'_>, mut lhs: CompletedMarker, propagate: bool) -> CompletedMarker {
    loop {
        lhs = if p.at(K::Dot) || p.at(K::QuestionDot) {
            let m = lhs.precede(p);
            p.bump_any();
            if p.at(K::Dollar) {
                splice(p);
            } else {
                types::name_ref(p);
            }
            if types::generic_args_follow(p.input(), p.pos()) {
                types::generic_arg_list(p);
            }
            m.complete(p, K::MemberExpr)
        } else if p.at(K::LParen) {
            let m = lhs.precede(p);
            arg_list(p);
            m.complete(p, K::CallExpr)
        } else if p.at(K::LBracket) || p.at(K::QuestionLBracket) {
            // `?[` written without a space is a null-conditional index, even
            // in an arm of a conditional (`c ? a?[0] : b`); with a space,
            // `flag ? [1] : []` is a conditional.
            let m = lhs.precede(p);
            if !p.eat(K::QuestionLBracket) {
                p.bump(K::LBracket);
            }
            while !p.at(K::RBracket) && !p.at_end() {
                arg(p);
                if !p.eat(K::Comma) {
                    break;
                }
            }
            p.expect(K::RBracket);
            m.complete(p, K::IndexExpr)
        } else if p.at(K::PlusPlus) || p.at(K::MinusMinus) {
            let m = lhs.precede(p);
            p.bump_any();
            m.complete(p, K::PostfixExpr)
        } else if propagate && at_propagation(p) {
            // Member access after propagation needs parentheses (§7.4):
            // `(r?).Name`. Only another `?` may follow here.
            let m = lhs.precede(p);
            p.bump(K::Question);
            let propagated = m.complete(p, K::PropagateExpr);
            return propagation(p, propagated);
        } else {
            return lhs;
        };
    }
}

/// `x?` that is not the start of `x ? a : b` (§7.4).
fn at_propagation(p: &Parser<'_>) -> bool {
    p.current() == K::Question
        && !p.at(K::QuestionQuestion)
        && !conditional_follows(p.input(), p.pos())
}

fn propagation(p: &mut Parser<'_>, mut lhs: CompletedMarker) -> CompletedMarker {
    while at_propagation(p) {
        let m = lhs.precede(p);
        p.bump(K::Question);
        lhs = m.complete(p, K::PropagateExpr);
    }
    // Member access after propagation needs parentheses (§7.4); `(r?).Name`
    // is a parenthesized expression, so the operand stops here.
    lhs
}

/// Whether `match (x) {` starts here (§14.4): the word `match`, a
/// parenthesized scrutinee with balanced parentheses, and `{` after it. A
/// call `match(x)` is never followed by `{`, so anything else leaves
/// `match` an identifier.
pub(crate) fn at_match(p: &Parser<'_>) -> bool {
    if !p.at_contextual("match") {
        return false;
    }
    let input = p.input();
    input
        .after_parens(p.pos() + 1)
        .is_some_and(|end| input.kind(end) == K::LBrace)
}

/// `match (x) { arms }`, the same in a statement and in an expression
/// (§7.7). An arm's value is an expression or a block; a comma separates
/// arms, and after a block it may be left out.
pub(crate) fn match_head(p: &mut Parser<'_>) {
    p.bump_remap(K::MatchKw);
    stmts::parenthesized(p);
    let list = p.start();
    if p.expect(K::LBrace) {
        while !p.at(K::RBrace) && !p.at_end() {
            let arm = p.start();
            patterns::pattern(p);
            if p.at_contextual("when") {
                patterns::when_clause(p);
            }
            types::arrow(p);
            let block = p.at(K::LBrace);
            if block {
                stmts::block(p);
                arm.complete(p, K::MatchBlockArm);
            } else {
                expr(p);
                arm.complete(p, K::MatchArm);
            }
            if !p.eat(K::Comma) && !block {
                break;
            }
        }
        p.expect(K::RBrace);
    }
    list.complete(p, K::MatchArmList);
}

pub(crate) fn arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    if !p.expect(K::LParen) {
        m.complete(p, K::ArgList);
        return;
    }
    while !p.at(K::RParen) && !p.at_end() && !p.at(K::Semicolon) && !p.at(K::RBrace) {
        arg(p);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RParen);
    m.complete(p, K::ArgList);
}

fn arg(p: &mut Parser<'_>) {
    p.lambdas(true, arg_inner);
}

fn arg_inner(p: &mut Parser<'_>) {
    let m = p.start();
    if p.at(K::Ident) && p.nth(1) == K::Colon {
        types::name_ref(p);
        p.bump(K::Colon);
    }
    if p.at_any(&[K::RefKw, K::InKw, K::MoveKw]) {
        p.bump_any();
    }
    expr(p);
    m.complete(p, K::Arg);
}

/// `(e)`, `()`, `(a, b)`, `(Id: 1, Name: "a")`. A parenthesized
/// expression holds its expression directly; a name or a comma makes it a
/// tuple, and only then is the first expression wrapped in a `TupleElem`.
fn paren_or_tuple(p: &mut Parser<'_>) -> CompletedMarker {
    p.lambdas(true, paren_or_tuple_inner)
}

fn paren_or_tuple_inner(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::LParen);
    if p.eat(K::RParen) {
        return m.complete(p, K::TupleExpr);
    }
    if !(p.at(K::Ident) && p.nth(1) == K::Colon) {
        let first = expr_bp(p, 0);
        if !p.at(K::Comma) {
            if first.is_none() {
                p.error("expected an expression");
            }
            p.expect(K::RParen);
            return m.complete(p, K::ParenExpr);
        }
        match first {
            Some(first) => {
                let e = first.precede(p);
                e.complete(p, K::TupleElem);
            }
            None => p.error("expected an expression"),
        }
        p.bump(K::Comma);
    }
    while !p.at(K::RParen) && !p.at_end() {
        let e = p.start();
        if p.at(K::Ident) && p.nth(1) == K::Colon {
            types::name_ref(p);
            p.bump(K::Colon);
        }
        expr(p);
        e.complete(p, K::TupleElem);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RParen);
    m.complete(p, K::TupleExpr)
}

fn collection(p: &mut Parser<'_>) -> CompletedMarker {
    p.lambdas(true, collection_inner)
}

fn collection_inner(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::LBracket);
    while !p.at(K::RBracket) && !p.at_end() {
        let e = p.start();
        p.eat(K::DotDot);
        expr(p);
        e.complete(p, K::CollectionElem);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RBracket);
    m.complete(p, K::CollectionExpr)
}

/// `new T(args) { P = v }`, `new()`, `new T { P = v }`, `new int[10]`.
fn new_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::NewKw);
    if types::at_type_start(p) && !p.at(K::LParen) {
        types::element_ty(p);
        if p.at(K::LBracket) {
            array_size(p);
            return m.complete(p, K::NewArrayExpr);
        }
    }
    if p.at(K::LParen) {
        arg_list(p);
    }
    if p.at(K::LBrace) {
        object_initializer(p);
    }
    m.complete(p, K::NewExpr)
}

pub(crate) fn object_initializer(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::LBrace);
    while !p.at(K::RBrace) && !p.at_end() {
        let init = p.start();
        types::name_ref(p);
        p.expect(K::Eq);
        expr(p);
        init.complete(p, K::MemberInit);
        if !p.eat(K::Comma) {
            break;
        }
    }
    p.expect(K::RBrace);
    m.complete(p, K::ObjectInitializer);
}

/// `$x`, `$(expr)`, `$..xs` inside `code { }` (§9.3).
pub(crate) fn splice(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::Dollar);
    p.eat(K::DotDot);
    if p.eat(K::LParen) {
        expr(p);
        p.expect(K::RParen);
    } else {
        types::name_ref(p);
    }
    m.complete(p, K::Splice);
}

/// `$"x = {x,5:F2}"`: text pieces and holes between the start and the end.
fn interp_string(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(K::InterpStart);
    loop {
        if p.at(K::InterpText) {
            let t = p.start();
            p.bump(K::InterpText);
            t.complete(p, K::InterpTextPart);
        } else if p.at(K::HoleOpen) {
            let h = p.start();
            p.bump(K::HoleOpen);
            expr(p);
            if p.eat(K::Comma) {
                expr(p);
            }
            p.eat(K::HoleFormat);
            if !p.eat(K::HoleClose) {
                p.error("expected the `}` that closes the hole");
                // Skip to the end of the hole or the string.
                while !p.at_end() && !p.at_any(&[K::HoleClose, K::InterpEnd]) {
                    p.bump_any();
                }
                p.eat(K::HoleClose);
            }
            h.complete(p, K::InterpHole);
        } else {
            break;
        }
    }
    // A string the lexer found unclosed has no end token; MR0012 says so.
    p.eat(K::InterpEnd);
    m.complete(p, K::InterpString)
}
