//! Statements (§7.10, §7.11).

use super::exprs::{self, at_expr_start};
use super::items::Declarators;
use super::{Parser, items, patterns};
use crate::SyntaxKind;

use SyntaxKind as K;

/// Access modifiers never start a statement, so meeting one where a
/// statement should start means the block's `}` is missing.
const ACCESS: &[SyntaxKind] = &[K::PublicKw, K::PrivateKw, K::InternalKw];

/// `{ statements }`.
pub(crate) fn block(p: &mut Parser<'_>) {
    let m = p.start();
    if !p.expect(K::LBrace) {
        m.complete(p, K::Block);
        return;
    }
    while !p.at(K::RBrace) {
        if p.at_end() || p.at_any(ACCESS) {
            p.error(format!("expected {}", super::describe(K::RBrace)));
            m.complete(p, K::Block);
            return;
        }
        statement(p);
    }
    p.bump(K::RBrace);
    m.complete(p, K::Block);
}

/// One statement. Always takes at least one token.
pub(crate) fn statement(p: &mut Parser<'_>) {
    let before = p.pos();
    if !p.nested_statement(statement_inner, skip_statement) {
        p.eat(K::Semicolon);
    }
    if p.pos() == before {
        let e = p.start();
        p.error("expected a statement");
        p.bump_any();
        e.complete(p, K::Error);
    }
}

/// Skips the statement ahead the way it is written: a block, an `if`
/// with its `else` branches, a loop with its body, `do ... while (a);`,
/// or the tokens up to its `;` or, for a local function or a `match`,
/// its block. The statements that hold the one being skipped wait in
/// `holding`, so a long `else if` chain does not recurse.
fn skip_statement(p: &mut Parser<'_>) {
    enum Holds {
        If,
        Do,
    }
    let start = p.pos();
    let mut holding = Vec::new();
    'statement: loop {
        while p.at(K::At) {
            p.bump_any();
            while p.at(K::Ident) || p.at(K::Dot) {
                p.bump_any();
            }
            skip_group(p);
        }
        match p.current() {
            K::IfKw | K::WhileKw | K::ForKw | K::ForeachKw => {
                if p.at(K::IfKw) {
                    holding.push(Holds::If);
                }
                p.bump_any();
                skip_group(p);
                continue;
            }
            K::DoKw => {
                holding.push(Holds::Do);
                p.bump_any();
                continue;
            }
            K::UnsafeKw if p.nth(1) == K::LBrace => {
                p.bump_any();
                skip_group(p);
            }
            K::LBrace => skip_group(p),
            K::Ident if exprs::at_match(p) => skip_to_end(p, true),
            _ => {
                let local_function = at_local_function(p);
                skip_to_end(p, local_function);
            }
        }
        // The statement ended, and so may the ones that hold it.
        while let Some(holds) = holding.pop() {
            match holds {
                Holds::If if p.eat(K::ElseKw) => continue 'statement,
                Holds::If => {}
                Holds::Do => {
                    if p.eat(K::WhileKw) {
                        skip_group(p);
                    }
                    p.eat(K::Semicolon);
                }
            }
        }
        break;
    }
    // A stray closing bracket is the statement, as one error.
    if p.pos() == start && !p.at_end() {
        p.bump_any();
    }
}

const OPENERS: &[SyntaxKind] = &[K::LParen, K::LBracket, K::LBrace, K::HoleOpen];
const CLOSERS: &[SyntaxKind] = &[K::RParen, K::RBracket, K::RBrace, K::HoleClose];

/// Skips the bracketed group ahead, if there is one.
fn skip_group(p: &mut Parser<'_>) {
    if !p.at_any(OPENERS) {
        return;
    }
    let mut open = 0usize;
    while !p.at_end() {
        if p.at_any(OPENERS) {
            open += 1;
        } else if p.at_any(CLOSERS) {
            open -= 1;
        }
        p.bump_any();
        if open == 0 {
            return;
        }
    }
}

/// Skips up to and with the `;` that ends the statement, or, with
/// `ends_at_block`, its first block; a closing bracket that is not its
/// own ends it too.
fn skip_to_end(p: &mut Parser<'_>, ends_at_block: bool) {
    let mut open = 0usize;
    while !p.at_end() {
        if p.at_any(OPENERS) {
            open += 1;
        } else if p.at_any(CLOSERS) {
            if open == 0 {
                return;
            }
            open -= 1;
        } else if open == 0 && p.at(K::Semicolon) {
            p.bump_any();
            return;
        }
        let closes_block = open == 0 && p.at(K::RBrace);
        p.bump_any();
        if closes_block && ends_at_block {
            return;
        }
    }
}

fn statement_inner(p: &mut Parser<'_>) {
    match p.current() {
        K::LBrace => block(p),
        K::Semicolon => {
            let m = p.start();
            p.bump(K::Semicolon);
            m.complete(p, K::EmptyStmt);
        }
        K::IfKw => if_stmt(p),
        K::WhileKw => while_stmt(p),
        K::DoKw => do_while_stmt(p),
        K::ForKw => for_stmt(p),
        K::ForeachKw => foreach_stmt(p),
        K::BreakKw => keyword_stmt(p, K::BreakKw, K::BreakStmt),
        K::ContinueKw => keyword_stmt(p, K::ContinueKw, K::ContinueStmt),
        K::ReturnKw => return_stmt(p),
        // Before local functions: `match (x) { ... }` is a statement (§14.4).
        K::Ident if exprs::at_match(p) => {
            let m = p.start();
            exprs::match_head(p);
            m.complete(p, K::MatchStmt);
        }
        K::UnsafeKw if p.nth(1) == K::LBrace => {
            let m = p.start();
            p.bump(K::UnsafeKw);
            block(p);
            m.complete(p, K::UnsafeStmt);
        }
        K::At => {
            let m = p.start();
            while p.at(K::At) {
                items::attribute(p);
            }
            statement(p);
            m.complete(p, K::AttributedStmt);
        }
        K::LetKw | K::VarKw if p.nth(1) == K::LParen => deconstruct_stmt(p),
        K::LetKw | K::VarKw => local_decl_stmt(p),
        _ if at_local_function(p) => {
            let m = p.start();
            items::local_function(p);
            m.complete(p, K::LocalFunctionStmt);
        }
        _ if at_expr_start(p) => {
            let m = p.start();
            exprs::expr(p);
            p.expect(K::Semicolon);
            m.complete(p, K::ExprStmt);
        }
        _ => {}
    }
}

/// `Add(a: int, b: int): int => a + b;` or `static Max<T>(...)` in a
/// body.
fn at_local_function(p: &Parser<'_>) -> bool {
    let input = p.input();
    let mut at = p.pos();
    while matches!(input.kind(at), K::StaticKw | K::AsyncKw | K::ExternKw) {
        at += 1;
    }
    items::at_function(input, at)
}

/// `let x = 1;`, `var x: int = 1;`, `let x: int;` (§7.10).
fn local_decl_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    local_decl(p);
    p.expect(K::Semicolon);
    m.complete(p, K::LocalDeclStmt);
}

/// At `let` or `var`.
fn local_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_any();
    items::variable_declarator(p, Declarators::Local);
    m.complete(p, K::LocalDecl);
}

/// `let (id, name) = pair;`.
fn deconstruct_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_any();
    patterns::tuple_designation(p, true);
    p.expect(K::Eq);
    exprs::expr(p);
    p.expect(K::Semicolon);
    m.complete(p, K::DeconstructStmt);
}

fn keyword_stmt(p: &mut Parser<'_>, keyword: SyntaxKind, kind: SyntaxKind) {
    let m = p.start();
    p.bump(keyword);
    p.expect(K::Semicolon);
    m.complete(p, kind);
}

fn return_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::ReturnKw);
    if !p.at(K::Semicolon) && at_expr_start(p) {
        exprs::expr(p);
    }
    p.expect(K::Semicolon);
    m.complete(p, K::ReturnStmt);
}

/// `( condition )` after `if`, `while` and `match`.
pub(crate) fn parenthesized(p: &mut Parser<'_>) {
    p.expect(K::LParen);
    p.lambdas(true, exprs::expr);
    p.expect(K::RParen);
}

fn if_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::IfKw);
    parenthesized(p);
    statement(p);
    if p.at(K::ElseKw) {
        let e = p.start();
        p.bump(K::ElseKw);
        statement(p);
        e.complete(p, K::ElseClause);
    }
    m.complete(p, K::IfStmt);
}

fn while_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::WhileKw);
    parenthesized(p);
    statement(p);
    m.complete(p, K::WhileStmt);
}

fn do_while_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::DoKw);
    statement(p);
    p.expect(K::WhileKw);
    parenthesized(p);
    p.expect(K::Semicolon);
    m.complete(p, K::DoWhileStmt);
}

/// `for (init; condition; update) body`.
fn for_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::ForKw);
    p.expect(K::LParen);
    if !p.at(K::Semicolon) {
        if p.at(K::LetKw) || p.at(K::VarKw) {
            local_decl(p);
        } else {
            let init = p.start();
            expr_list(p);
            init.complete(p, K::ForInitExprs);
        }
    }
    p.expect(K::Semicolon);
    if !p.at(K::Semicolon) {
        exprs::expr(p);
    }
    p.expect(K::Semicolon);
    if !p.at(K::RParen) {
        let update = p.start();
        expr_list(p);
        update.complete(p, K::ForUpdate);
    }
    p.expect(K::RParen);
    statement(p);
    m.complete(p, K::ForStmt);
}

fn expr_list(p: &mut Parser<'_>) {
    exprs::expr(p);
    while p.eat(K::Comma) {
        exprs::expr(p);
    }
}

/// `foreach (let x in xs)`, `foreach (var (k, v) in d)`,
/// `foreach (ref var x in span)`, `foreach (let x: int in xs)` (§7.11).
fn foreach_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(K::ForeachKw);
    p.expect(K::LParen);
    p.eat(K::RefKw);
    if p.at(K::LetKw) || p.at(K::VarKw) {
        p.bump_any();
    }
    patterns::designation(p, true);
    p.expect(K::InKw);
    exprs::expr(p);
    p.expect(K::RParen);
    statement(p);
    m.complete(p, K::ForeachStmt);
}
