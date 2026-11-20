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

pub use types::PREDEFINED_TYPES;

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
    parse_within(text, None)
}

/// [`parse`], with at most `budget` events instead of the budget the
/// tokens give.
fn parse_within(text: &str, budget: Option<usize>) -> Parse {
    let lexed = lex(text);
    let input = Input::new(text, &lexed);
    let mut p = Parser::new(&input);
    if let Some(budget) = budget {
        p.budget = budget;
    }
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
    /// Whether the token starts where the lexer ended a character or string
    /// literal it found unclosed (`MR0012`), so that it is the rest of what
    /// was meant as the literal.
    cut: Vec<bool>,
    /// Whether a line break comes between the token and the one before it.
    line_start: Vec<bool>,
}

impl<'t> Input<'t> {
    fn new(text: &'t str, lexed: &Lexed) -> Self {
        let mut input = Input {
            text,
            kinds: Vec::new(),
            ranges: Vec::new(),
            raw: Vec::new(),
            joined: Vec::new(),
            cut: Vec::new(),
            line_start: Vec::new(),
        };
        let mut line_break = false;
        for (i, token) in lexed.tokens.iter().enumerate() {
            if token.kind.is_trivia() {
                line_break |= token.kind == K::Newline;
                continue;
            }
            input.line_start.push(line_break);
            line_break = false;
            input.kinds.push(token.kind);
            input.ranges.push(token.range);
            input.raw.push(i);
            input.joined.push(lexed.is_joined(i));
            input.cut.push(lexed.errors.iter().any(|error| {
                error.code == codes::MR0012 && error.range.end() == token.range.start()
            }));
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

    /// The index after the `)` that closes the `(` at `open`, counting only
    /// parentheses, or `None` when no `(` is there or the text ends first.
    fn after_parens(&self, open: usize) -> Option<usize> {
        if self.kind(open) != K::LParen {
            return None;
        }
        let mut depth = 0usize;
        for (i, &kind) in self.kinds.iter().enumerate().skip(open) {
            match kind {
                K::LParen => depth += 1,
                K::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        None
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
    /// replace the token it wraps.
    Error {
        message: String,
        fix: Option<TokenFix>,
    },
    /// Report `MR0018` at the next token: nesting goes past [`MAX_DEPTH`].
    TooDeep,
    /// A `Start` that was abandoned or moved.
    Tombstone,
}

/// A replacement for the token a syntax error wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenFix {
    /// What the fix does, shown with the error.
    message: String,
    /// The text that takes the token's place.
    replacement: String,
}

/// How many expressions, statements, types, patterns and member lists can
/// nest inside each other (§14.4).
pub(crate) const MAX_DEPTH: usize = 256;

/// How many events the parser may emit for each token before it counts as
/// stuck. Every loop in the parser takes a token or ends, so a parse emits
/// a handful of events per token; a loop that stopped taking tokens would
/// otherwise open nodes until memory runs out.
const EVENTS_PER_TOKEN: usize = 64;

/// The error reported where the parser stopped, past its budget of events.
const STUCK: &str = "internal error: the parser stopped making progress here, \
                     and the rest of the file is not checked";

pub(crate) struct Parser<'t> {
    /// The tokens to parse.
    input: &'t Input<'t>,
    /// The index of the next token in `input`.
    pos: usize,
    /// What the parser did so far.
    events: Vec<Event>,
    /// Whether `x => ...` is read as something else than a lambda here.
    no_lambda: bool,
    /// How many levels of [`Parser::nested`] are open.
    depth: usize,
    /// How many events the parser may emit ([`EVENTS_PER_TOKEN`]).
    budget: usize,
    /// The token where the parser stopped, past its budget. From then on it
    /// sees the end of the input and reports nothing, and
    /// [`Parser::finish_stopped`] takes the rest.
    stopped: Option<usize>,
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
            budget: EVENTS_PER_TOKEN * (input.len() + 1),
            stopped: None,
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
    /// node: a bracketed group, when the construct starts with an opening
    /// bracket, and otherwise the tokens up to a `;`, a `,` or a closing
    /// bracket that is not its own.
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

    /// [`Parser::nested`] for a statement. At [`MAX_DEPTH`] it reports
    /// `MR0018` instead and `skip` takes the statement into an `Error`
    /// node. Returns whether the statement was parsed.
    pub(crate) fn nested_statement(
        &mut self,
        parse: impl FnOnce(&mut Self),
        skip: impl FnOnce(&mut Self),
    ) -> bool {
        if self.depth < MAX_DEPTH {
            self.depth += 1;
            parse(self);
            self.depth -= 1;
            return true;
        }
        let m = self.start();
        self.events.push(Event::TooDeep);
        skip(self);
        m.complete(self, K::Error);
        false
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
        let group = matches!(self.current(), K::LParen | K::LBracket | K::LBrace)
            || (angles && self.at(K::Lt));
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
            if group && open == 0 {
                break;
            }
        }
        m.complete(self, K::Error)
    }

    pub(crate) fn lambdas_allowed(&self) -> bool {
        !self.no_lambda
    }

    /// Opens a node. Past the budget of events, the parser stopped taking
    /// tokens, which is a bug in the parser: it stops there instead of
    /// running until memory runs out ([`Parser::stopped`]).
    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        if pos >= self.budget && self.stopped.is_none() {
            self.stopped = Some(self.pos);
            self.pos = self.input.len();
        }
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

    /// Whether the next token starts where the lexer ended a literal it
    /// found unclosed, as `b` in `'ab`.
    pub(crate) fn at_cut(&self) -> bool {
        self.input.cut.get(self.pos).copied().unwrap_or(false)
    }

    /// Whether a line break comes before the next token.
    pub(crate) fn at_line_start(&self) -> bool {
        self.nth_at_line_start(0)
    }

    /// Whether a line break comes before the `n`th token ahead.
    pub(crate) fn nth_at_line_start(&self, n: usize) -> bool {
        self.input
            .line_start
            .get(self.pos + n)
            .copied()
            .unwrap_or(false)
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
        if self.stopped.is_some() {
            return;
        }
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
        if self.stopped.is_some() {
            return;
        }
        self.events.push(Event::Error {
            message: message.into(),
            fix: Some(TokenFix {
                message: fix.into(),
                replacement: replacement.into(),
            }),
        });
    }
}

impl Parser<'_> {
    /// After the parser stopped ([`Parser::stopped`]), takes the tokens it
    /// did not reach into one `Error` node that reports [`STUCK`]. The
    /// caller is the file, whose node is still open.
    pub(crate) fn finish_stopped(&mut self) {
        let Some(at) = self.stopped.take() else {
            return;
        };
        self.pos = at;
        self.budget = usize::MAX;
        let e = self.start();
        self.error(STUCK);
        while !self.at_end() {
            self.bump_any();
        }
        e.complete(self, K::Error);
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

#[cfg(test)]
mod tests {
    use super::{STUCK, parse, parse_within};

    #[test]
    fn a_parser_past_its_budget_takes_the_rest_into_one_error() {
        let text = "let a = 1;\nlet b = (2 + 3) * 4;\n";
        let full = parse(text);
        assert!(full.errors().is_empty());
        let stopped = parse_within(text, Some(12));
        assert_eq!(stopped.syntax().text(), text);
        let errors: Vec<_> = stopped
            .errors()
            .iter()
            .map(|error| (error.code.to_string(), error.message.clone()))
            .collect();
        assert_eq!(errors, [("MR0110".to_string(), STUCK.to_string())]);
        let tree = stopped.syntax().debug_tree();
        let error_nodes = tree
            .lines()
            .filter(|line| line.trim_start().starts_with("Error@"))
            .count();
        assert_eq!(error_nodes, 1, "{tree}");
    }

    #[test]
    fn a_parser_within_its_budget_reports_nothing_more() {
        let text = "let a = 1;\n";
        let events = super::Parser::new(&super::Input::new(text, &super::lex(text))).budget;
        assert_eq!(events, super::EVENTS_PER_TOKEN * 6);
        assert!(parse_within(text, Some(events)).errors().is_empty());
    }
}
