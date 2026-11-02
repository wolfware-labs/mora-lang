//! The parser (§14.4).
//!
//! Recursive descent over the tokens that are not trivia, with Pratt
//! parsing for expressions. The parser does not build the tree itself: it
//! emits events (open a node, take a token, close a node, report an error),
//! and [`build`] turns them into a tree, putting whitespace and comments back
//! in place. A marker can be preceded by a new node after it is closed, which
//! is how `a + b` wraps the already parsed `a` in a binary expression.
//!
//! The parser never stops at an error. It reports `MR0110`, wraps what it
//! cannot use in an `Error` node, and goes on at the next declaration or
//! member, so every later declaration still gets its tree.
//!
//! Nesting is limited to [`MAX_DEPTH`] levels (§14.4), so that neither the
//! parser nor a later pass that walks the tree recursively runs out of
//! stack. The parser counts the levels it is inside and stops descending
//! at the limit; [`build`] counts the levels of the finished tree, which
//! the parser cannot see while it wraps a left operand, as in a long chain
//! `a + b + c`.

mod build;
mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;

use mora_diag::codes;
use mora_span::TextRange;

use crate::ast::{self, AstNode};
use crate::lexer::{Lexed, lex};
use crate::{GreenNode, SyntaxError, SyntaxKind, SyntaxNode};

use SyntaxKind as K;

/// A parsed file: its tree and every lexical and syntax error in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    green: GreenNode,
    errors: Vec<SyntaxError>,
}

impl Parse {
    /// The root of the tree.
    pub fn syntax(&self) -> SyntaxNode<'_> {
        self.green.root()
    }

    /// The typed view of the root.
    pub fn tree(&self) -> ast::SourceFile<'_> {
        // The root is always a `SourceFile`: `parse` opens it first.
        ast::SourceFile::cast(self.syntax()).unwrap_or_else(|| unreachable!())
    }

    /// The lexical and syntax errors, in the order of the text.
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn green(&self) -> &GreenNode {
        &self.green
    }
}

/// Parses a source file. Every byte of `text` ends up in the tree.
pub fn parse(text: &str) -> Parse {
    let lexed = lex(text);
    let input = Input::new(text, &lexed);
    let mut p = Parser::new(&input);
    items::source_file(&mut p);
    let (green, mut errors) = build::build(text, &lexed, p.events);
    errors.extend(lexed.errors.iter().cloned());
    errors.sort_by_key(|error| error.range.start());
    Parse { green, errors }
}

/// The tokens the parser sees: everything but trivia, with each token's
/// place among all tokens and whether it touches the next one.
pub(crate) struct Input<'t> {
    /// The whole text.
    text: &'t str,
    /// The kind of each token.
    kinds: Vec<SyntaxKind>,
    /// Where each token is.
    ranges: Vec<TextRange>,
    /// The index of each token in [`Lexed::tokens`].
    raw: Vec<usize>,
    /// Whether the token is directly followed by the next one, with no trivia.
    joined: Vec<bool>,
}

impl<'t> Input<'t> {
    fn new(text: &'t str, lexed: &Lexed) -> Self {
        let mut input = Input {
            text,
            kinds: Vec::new(),
            ranges: Vec::new(),
            raw: Vec::new(),
            joined: Vec::new(),
        };
        for (i, token) in lexed.tokens.iter().enumerate() {
            if token.kind.is_trivia() {
                continue;
            }
            input.kinds.push(token.kind);
            input.ranges.push(token.range);
            input.raw.push(i);
            input.joined.push(lexed.is_joined(i));
        }
        input
    }

    /// The kind of token `i`, or `Error` past the end (an end marker that
    /// matches nothing the grammar expects).
    fn kind(&self, i: usize) -> SyntaxKind {
        self.kinds.get(i).copied().unwrap_or(K::Error)
    }

    fn text(&self, i: usize) -> &'t str {
        self.ranges.get(i).map_or("", |range| &self.text[*range])
    }

    fn is_joined(&self, i: usize) -> bool {
        self.joined.get(i).copied().unwrap_or(false)
    }

    fn len(&self) -> usize {
        self.kinds.len()
    }
}

/// What the parser did, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open a node. `forward_parent` points at a later `Start` that wraps
    /// this one, set by [`CompletedMarker::precede`].
    Start {
        kind: SyntaxKind,
        forward_parent: Option<usize>,
    },
    /// Close the innermost open node.
    Finish,
    /// Take `n_raw` adjacent tokens as one token of `kind`: `>>` from two `>`.
    Token { kind: SyntaxKind, n_raw: usize },
    /// Report `MR0110` at the next token. With `fix`, the error offers to
    /// replace the token it wraps: a fix message and the replacement text.
    Error {
        message: String,
        fix: Option<(String, String)>,
    },
    /// Report `MR0018` at the next token: nesting goes past [`MAX_DEPTH`].
    TooDeep,
    /// A `Start` that was abandoned or moved.
    Tombstone,
}

/// How many expressions, statements, types, patterns and member lists can
/// nest inside each other (§14.4).
pub(crate) const MAX_DEPTH: usize = 256;

pub(crate) struct Parser<'t> {
    input: &'t Input<'t>,
    pos: usize,
    events: Vec<Event>,
    no_lambda: bool,
    /// How many levels of [`Parser::nested`] are open.
    depth: usize,
}

/// A node that is open: it must be completed or abandoned.
pub(crate) struct Marker {
    pos: usize,
}

/// A node that is closed and can still be wrapped by a new one.
#[derive(Clone, Copy)]
pub(crate) struct CompletedMarker {
    pos: usize,
}

impl Marker {
    pub(crate) fn complete(self, p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        if let Some(Event::Start { kind: slot, .. }) = p.events.get_mut(self.pos) {
            *slot = kind;
        }
        p.events.push(Event::Finish);
        CompletedMarker { pos: self.pos }
    }

    /// Drops the node; its children go to the parent.
    pub(crate) fn abandon(self, p: &mut Parser<'_>) {
        if self.pos + 1 == p.events.len() {
            p.events.pop();
        } else if let Some(event) = p.events.get_mut(self.pos) {
            *event = Event::Tombstone;
        }
    }
}

impl CompletedMarker {
    /// Opens a node that starts where this one starts and contains it.
    pub(crate) fn precede(self, p: &mut Parser<'_>) -> Marker {
        let m = p.start();
        if let Some(Event::Start { forward_parent, .. }) = p.events.get_mut(self.pos) {
            *forward_parent = Some(m.pos);
        }
        m
    }
}

/// The joined kinds and the pieces they are made of (§14.4).
const JOINED: &[(SyntaxKind, &[SyntaxKind])] = &[
    (K::UShrEq, &[K::Gt, K::Gt, K::Gt, K::Eq]),
    (K::UShr, &[K::Gt, K::Gt, K::Gt]),
    (K::ShrEq, &[K::Gt, K::Gt, K::Eq]),
    (K::Shr, &[K::Gt, K::Gt]),
    (K::GtEq, &[K::Gt, K::Eq]),
    (K::QuestionLBracket, &[K::Question, K::LBracket]),
];

impl<'t> Parser<'t> {
    fn new(input: &'t Input<'t>) -> Self {
        Parser {
            input,
            pos: 0,
            events: Vec::new(),
            no_lambda: false,
            depth: 0,
        }
    }

    /// Runs `parse` with `x => ...` read as something else than a lambda,
    /// or, with `allowed`, as a lambda again. In a match arm `None => 0`
    /// and in a guard `when ok => 1`, the `=>` ends the pattern or the
    /// guard; inside parentheses, brackets and arguments lambdas come back.
    pub(crate) fn lambdas<T>(&mut self, allowed: bool, parse: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.no_lambda;
        self.no_lambda = !allowed;
        let result = parse(self);
        self.no_lambda = saved;
        result
    }

    /// Runs `parse` one level deeper. At [`MAX_DEPTH`] it reports
    /// `MR0018` instead and wraps the rest of the construct in an `Error`
    /// node: the tokens up to a `;`, a `,` or a closing bracket that is
    /// not its own.
    pub(crate) fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, CompletedMarker> {
        self.nested_in(false, parse)
    }

    /// [`Parser::nested`] in a type, where `<` and `>` are brackets too.
    pub(crate) fn nested_type<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, CompletedMarker> {
        self.nested_in(true, parse)
    }

    fn nested_in<T>(
        &mut self,
        angles: bool,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, CompletedMarker> {
        if self.depth == MAX_DEPTH {
            return Err(self.too_deep(angles));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        Ok(result)
    }

    fn too_deep(&mut self, angles: bool) -> CompletedMarker {
        let m = self.start();
        self.events.push(Event::TooDeep);
        let mut open = 0usize;
        while !self.at_end() {
            match self.current() {
                K::Lt if angles => open += 1,
                K::Gt if angles && open == 0 => break,
                K::Gt if angles => open -= 1,
                K::LParen | K::LBracket | K::LBrace | K::HoleOpen => open += 1,
                K::RParen | K::RBracket | K::RBrace | K::HoleClose if open == 0 => break,
                K::RParen | K::RBracket | K::RBrace | K::HoleClose => open -= 1,
                K::Semicolon | K::Comma if open == 0 => break,
                _ => {}
            }
            self.bump_any();
        }
        m.complete(self, K::Error)
    }

    pub(crate) fn lambdas_allowed(&self) -> bool {
        !self.no_lambda
    }

    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start {
            kind: K::Error,
            forward_parent: None,
        });
        Marker { pos }
    }

    /// The kind of the `n`th token ahead, without joining.
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        self.input.kind(self.pos + n)
    }

    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn nth_text(&self, n: usize) -> &'t str {
        self.input.text(self.pos + n)
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// The position of the next token, for lookahead with [`types::scan_type`].
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn input(&self) -> &'t Input<'t> {
        self.input
    }

    /// The number of tokens `kind` takes at `pos`, if it is there: joined
    /// kinds take several adjacent pieces.
    fn pieces(&self, at: usize, kind: SyntaxKind) -> Option<usize> {
        if let Some((_, pieces)) = JOINED.iter().find(|(joined, _)| *joined == kind) {
            let matches = pieces.iter().enumerate().all(|(i, piece)| {
                self.input.kind(at + i) == *piece
                    && (i + 1 == pieces.len() || self.input.is_joined(at + i))
            });
            return matches.then_some(pieces.len());
        }
        (self.input.kind(at) == kind).then_some(1)
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.pieces(self.pos, kind).is_some()
    }

    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        self.pieces(self.pos + n, kind).is_some()
    }

    pub(crate) fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.iter().any(|&kind| self.at(kind))
    }

    /// Whether the next token is the identifier `word`, which the grammar
    /// uses as a contextual keyword here.
    pub(crate) fn at_contextual(&self, word: &str) -> bool {
        self.current() == K::Ident && self.nth_text(0) == word
    }

    /// Takes the next token as `kind`, which must be there.
    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        let n_raw = self.pieces(self.pos, kind).unwrap_or(1);
        self.events.push(Event::Token { kind, n_raw });
        self.pos += n_raw;
    }

    /// Takes the next token as it was lexed.
    pub(crate) fn bump_any(&mut self) {
        if !self.at_end() {
            let kind = self.current();
            self.bump(kind);
        }
    }

    /// Takes an identifier as the contextual keyword `kind`.
    pub(crate) fn bump_remap(&mut self, kind: SyntaxKind) {
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.pos += 1;
    }

    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump(kind);
            true
        } else {
            false
        }
    }

    /// Takes `kind`, or reports that it is missing and takes nothing.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        self.error(format!("expected {}", describe(kind)));
        false
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        self.events.push(Event::Error {
            message: message.into(),
            fix: None,
        });
    }

    /// Reports `MR0110` on the next token, which the caller then takes,
    /// with a fix that replaces that token by `replacement`.
    pub(crate) fn error_with_fix(
        &mut self,
        message: impl Into<String>,
        fix: impl Into<String>,
        replacement: impl Into<String>,
    ) {
        self.events.push(Event::Error {
            message: message.into(),
            fix: Some((fix.into(), replacement.into())),
        });
    }
}

/// How an error message names a token kind: `` `;` ``, `an identifier`.
pub(crate) fn describe(kind: SyntaxKind) -> String {
    match kind.text() {
        Some(text) => format!("`{text}`"),
        None => match kind {
            K::Ident => "an identifier".into(),
            _ => format!("{kind:?}"),
        },
    }
}

/// The code of every syntax error.
pub(crate) const SYNTAX_ERROR: mora_diag::Code = codes::MR0110;
