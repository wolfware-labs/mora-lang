//! Turns parser events into a tree, putting trivia back in place.
//!
//! Trivia goes into the innermost open node, with one exception: comments
//! directly above a declaration, with no blank line between them, belong to
//! that declaration, so a doc comment is part of the item it documents.
//!
//! The builder also enforces the nesting limit on the finished tree
//! (§14.4): a node that would sit deeper than [`MAX_DEPTH`] levels gets
//! `MR0018`, and it and everything inside it become one `Error` node that
//! holds their tokens, so the tree stays shallow for the passes after it.

use std::mem;

use mora_span::{TextRange, TextSize};

use mora_diag::{Code, codes};

use super::{Event, MAX_DEPTH, SYNTAX_ERROR};
use crate::ast::{self, AstNode};
use crate::lexer::Lexed;
use crate::{GreenNode, SyntaxError, SyntaxFix, SyntaxKind, TreeBuilder};

use SyntaxKind as K;

pub(super) fn build(
    text: &str,
    lexed: &Lexed,
    mut events: Vec<Event>,
) -> (GreenNode, Vec<SyntaxError>) {
    let mut sink = Sink {
        text,
        lexed,
        raw: 0,
        open: Vec::new(),
        levels: 0,
        flattened: None,
        muted: None,
        builder: TreeBuilder::new(),
        errors: Vec::new(),
    };
    let mut chain = Vec::new();
    for i in 0..events.len() {
        match mem::replace(&mut events[i], Event::Tombstone) {
            Event::Start {
                kind,
                forward_parent,
            } => {
                // Open the outermost of the nodes that precede this one first.
                chain.push(kind);
                let mut next = forward_parent;
                while let Some(j) = next {
                    match mem::replace(&mut events[j], Event::Tombstone) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
                            chain.push(kind);
                            next = forward_parent;
                        }
                        _ => break,
                    }
                }
                for kind in chain.drain(..).rev() {
                    sink.start(kind);
                }
            }
            Event::Finish => sink.finish(),
            Event::Token { kind, n_raw } => sink.token(kind, n_raw),
            // After `MR0018`, the rest of its statement reports nothing more:
            // what the parser skipped there leaves holes it would report.
            Event::Error { .. } if sink.muted.is_some() => {}
            Event::Error { message, fix } => {
                let wraps_token = matches!(events.get(i + 1), Some(Event::Token { .. }));
                sink.error(SYNTAX_ERROR, message, fix, wraps_token);
            }
            // Inside a flattened node, the error that covers it is reported.
            Event::TooDeep => {
                let wraps_token = matches!(events.get(i + 1), Some(Event::Token { .. }));
                sink.too_deep(wraps_token);
            }
            Event::Tombstone => {}
        }
    }
    (sink.builder.finish(), sink.errors)
}

struct Sink<'a> {
    /// The whole text.
    text: &'a str,
    /// Every token of the text, trivia included.
    lexed: &'a Lexed,
    /// The next token of `lexed` to add.
    raw: usize,
    /// Each open node and whether it counts toward the nesting limit.
    open: Vec<(SyntaxKind, bool)>,
    /// How many open nodes count toward the nesting limit.
    levels: usize,
    /// Inside a node that went past the limit: how many of the nodes inside
    /// it, which are left out of the tree, are open.
    flattened: Option<usize>,
    /// After a node went past the limit, how many nodes are open up to the
    /// outermost level of the statement around it: one error covers that
    /// statement.
    muted: Option<usize>,
    /// The tree so far.
    builder: TreeBuilder,
    /// The syntax errors so far.
    errors: Vec<SyntaxError>,
}

impl Sink<'_> {
    fn kind(&self, i: usize) -> Option<SyntaxKind> {
        self.lexed.tokens.get(i).map(|token| token.kind)
    }

    /// The index of the next token that is not trivia, or the end.
    fn next_significant(&self) -> usize {
        let mut i = self.raw;
        while self.kind(i).is_some_and(SyntaxKind::is_trivia) {
            i += 1;
        }
        i
    }

    fn add_raw(&mut self, until: usize) {
        while self.raw < until {
            let token = self.lexed.tokens[self.raw];
            self.builder.token(token.kind, &self.text[token.range]);
            self.raw += 1;
        }
    }

    fn start(&mut self, kind: SyntaxKind) {
        if let Some(open) = &mut self.flattened {
            *open += 1;
            return;
        }
        let parent = self.open.last().map(|&(parent, _)| parent);
        let nests = nests(kind, parent);
        if nests && self.levels == MAX_DEPTH {
            self.too_deep(true);
            self.flattened = Some(0);
            self.open_node(K::Error, false);
        } else {
            self.open_node(kind, nests);
        }
    }

    /// Reports `MR0018` for the next node, unless an error already covers
    /// the statement it is in.
    fn too_deep(&mut self, wraps_token: bool) {
        if self.muted.is_some() {
            return;
        }
        self.error(codes::MR0018, TOO_DEEP.into(), None, wraps_token);
        // The statement is the node directly inside the innermost block or
        // member list, or inside the file: a statement of a block, or a
        // member with its `;`. When blocks or member lists are themselves
        // what nests too deeply, it is the innermost level.
        let inside = self
            .open
            .iter()
            .rposition(|&(kind, _)| matches!(kind, K::Block | K::MemberList))
            .map_or(1, |i| i + 1);
        let statement = if inside < self.open.len() {
            Some(inside)
        } else {
            self.open.iter().rposition(|&(_, nests)| nests)
        };
        self.muted = Some(statement.map_or(0, |i| i + 1));
    }

    fn open_node(&mut self, kind: SyntaxKind, nests: bool) {
        self.open.push((kind, nests));
        self.levels += usize::from(nests);
        if self.open.len() == 1 {
            self.builder.start_node(kind);
            return;
        }
        let end = self.next_significant();
        let attach_from = if attaches_comments(kind) {
            self.attached_comments(end)
        } else {
            end
        };
        self.add_raw(attach_from);
        self.builder.start_node(kind);
        self.add_raw(end);
    }

    /// Where the comments directly above the token at `end` start: comments
    /// and the whitespace between them, up to a blank line or a directive.
    fn attached_comments(&self, end: usize) -> usize {
        let mut from = end;
        let mut i = end;
        let mut newline_seen = false;
        while i > self.raw {
            i -= 1;
            match self.kind(i) {
                Some(K::LineComment | K::DocComment | K::BlockComment) => {
                    from = i;
                    newline_seen = false;
                }
                Some(K::Whitespace) => {}
                Some(K::Newline) if newline_seen => break,
                Some(K::Newline) => newline_seen = true,
                _ => break,
            }
        }
        from
    }

    fn finish(&mut self) {
        match &mut self.flattened {
            Some(0) => self.flattened = None,
            Some(open) => {
                *open -= 1;
                return;
            }
            None => {}
        }
        if self.open.pop().is_some_and(|(_, nests)| nests) {
            self.levels -= 1;
        }
        if self.muted.is_some_and(|around| self.open.len() < around) {
            self.muted = None;
        }
        if self.open.is_empty() {
            // The trivia at the end of the file belongs to the root.
            self.add_raw(self.lexed.tokens.len());
        }
        self.builder.finish_node();
    }

    fn token(&mut self, kind: SyntaxKind, n_raw: usize) {
        let first = self.next_significant();
        self.add_raw(first);
        let last = (first + n_raw).min(self.lexed.tokens.len());
        if first >= last {
            return;
        }
        let range = TextRange::new(
            self.lexed.tokens[first].range.start(),
            self.lexed.tokens[last - 1].range.end(),
        );
        self.builder.token(kind, &self.text[range]);
        self.raw = last;
    }

    /// Reports `code`: on the token that follows, when the error wraps it
    /// in an `Error` node; otherwise just after the last token taken, where
    /// something is missing.
    /// A `fix` replaces the token the error wraps; without one, it is
    /// dropped.
    fn error(
        &mut self,
        code: Code,
        message: String,
        fix: Option<(String, String)>,
        wraps_token: bool,
    ) {
        let next = self.next_significant();
        let wrapped = if wraps_token {
            self.lexed.tokens.get(next).map(|token| token.range)
        } else {
            None
        };
        let range = if let Some(range) = wrapped {
            range
        } else {
            let end = self.lexed.tokens[..self.raw]
                .iter()
                .rev()
                .find(|token| !token.kind.is_trivia())
                .map_or(TextSize::new(0), |token| token.range.end());
            TextRange::empty(end)
        };
        let mut error = SyntaxError::new(code, range, message);
        if let (Some(range), Some((message, replacement))) = (wrapped, fix) {
            error = error.with_fix(SyntaxFix {
                message,
                edits: vec![(range, replacement)],
            });
        }
        self.errors.push(error);
    }
}

/// The message of `MR0018`.
const TOO_DEEP: &str = "nesting is too deep";

/// Whether a node of this kind, inside a node of kind `parent`, is a level
/// of nesting (§14.4). The fields of a case pattern, `(let r)` in
/// `Circle(let r)`, are part of that pattern.
fn nests(kind: SyntaxKind, parent: Option<SyntaxKind>) -> bool {
    if kind == K::PositionalPattern && parent == Some(K::CasePattern) {
        return false;
    }
    ast::Expr::can_cast(kind)
        || ast::Stmt::can_cast(kind)
        || ast::Type::can_cast(kind)
        || ast::Pattern::can_cast(kind)
        || kind == K::MemberList
}

/// Whether comments directly above a node of this kind belong to it.
fn attaches_comments(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        K::UsingDirective
            | K::StructDecl
            | K::ClassDecl
            | K::UnionDecl
            | K::EnumDecl
            | K::InterfaceDecl
            | K::MacroDecl
            | K::ExtensionDecl
            | K::FunctionDecl
            | K::FieldDecl
            | K::GetterDecl
            | K::IndexerDecl
            | K::ConstructorDecl
            | K::DestructorDecl
            | K::OperatorDecl
            | K::AssocTypeDecl
            | K::EnumMember
            | K::UnionCase
            | K::Accessor
    )
}
