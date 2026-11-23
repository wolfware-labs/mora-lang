//! Turns parser events into a tree, putting trivia and rejected text back
//! in place.
//!
//! Trivia goes into the innermost open node, with one exception: comments
//! directly above a declaration, with no blank line between them, belong to
//! that declaration, so a doc comment is part of the item it documents.
//! Each run of rejected text, which the parser did not see ([`Input`]),
//! goes in as trivia does, in an `Error` node of its own, unless the parser
//! took it in place of something missing ([`Event::Rejected`]) or it sits
//! inside a token it joins, as in `>\>` ([`Event::Token`]).
//!
//! The builder also enforces the nesting limit on the finished tree
//! (§14.4): a node that would sit deeper than [`MAX_DEPTH`] levels gets
//! `MR0018`, and it and everything inside it become one `Error` node that
//! holds their tokens, so the tree stays shallow for the passes after it.

use std::mem;

use mora_diag::{Code, codes};
use mora_span::{TextRange, TextSize};
use rustc_hash::FxHashSet;

#[cfg(doc)]
use super::Input;
use super::{Event, MAX_DEPTH, SYNTAX_ERROR, TokenFix};
use crate::ast::{self, AstNode};
use crate::lexer::Lexed;
use crate::{GreenNode, SyntaxError, SyntaxFix, SyntaxKind, TreeBuilder};

use SyntaxKind as K;

pub(super) fn build(
    text: &str,
    lexed: &Lexed,
    rejected_runs: &[Option<usize>],
    quoted: &[bool],
    mut events: Vec<Event>,
) -> (GreenNode, Vec<SyntaxError>) {
    // Where each string the lexer found unclosed ends. A character literal
    // is left out: it ends after its first character and takes nothing, so
    // what is missing after it is reported.
    let unclosed_strings = lexed
        .errors
        .iter()
        .filter(|error| error.code == codes::MR0012 && !text[error.range].starts_with('\''))
        .map(|error| error.range.end())
        .collect();
    // With no rejected text, no run starts or ends anywhere.
    let mut rejected_run_starts = if rejected_runs.is_empty() {
        Vec::new()
    } else {
        vec![None; lexed.tokens.len()]
    };
    for (start, end) in rejected_runs.iter().enumerate() {
        if let Some(end) = *end {
            rejected_run_starts[end - 1] = Some(start);
        }
    }
    let mut sink = Sink {
        text,
        lexed,
        rejected_runs,
        rejected_run_starts,
        quoted,
        unclosed_strings,
        taken_in_place: FxHashSet::default(),
        raw: 0,
        open: Vec::new(),
        levels: 0,
        flattened: None,
        muted: None,
        suspended: Vec::new(),
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
                // The node takes the rejected text before its first token
                // when the parser took it in place right after opening it,
                // or after opening nodes inside it before any token, as an
                // `Arg` opens around the `Error` that stands for its value.
                // With no rejected text, the parser took none in place.
                let in_place = !sink.rejected_runs.is_empty()
                    && matches!(
                        events[i + 1..]
                            .iter()
                            .find(|event| !matches!(event, Event::Start { .. })),
                        Some(Event::Rejected)
                    );
                // An `Error` node that would hold nothing but what it reports:
                // one past the nesting limit that takes no token (its
                // `TooDeep`), or one the parser opens for an error where
                // nothing is left to take, as `statement` does at the end of
                // the text after attributes and `accessor_list` before a
                // word that is not `get` or `set`. No node is made for it; the
                // nodes that wrap it are, and each of its events, followed by
                // nothing it wraps, reports where it is, as an error with
                // nothing to wrap does. Only an `Error` node is left out: a
                // node of another kind that the parser leaves empty, as the
                // `Name` of a parameter whose name is missing, stays.
                //
                // Left out, the node changes no muting of errors, which reads
                // only where the file or a node `is_container` names is open
                // (a block, a member list, a union or an enum body;
                // `Sink::too_deep`, `Sink::mutes_errors`, `Sink::finish`): it
                // is never directly in one. `nested_in` runs inside the
                // node of the construct that went one level deeper, and every
                // statement opens its own node before it nests (`ExprStmt`,
                // `LocalDeclStmt`, ...). `nested_statement`'s `skip` takes a
                // token unless the text has ended (`skip_statement`), and
                // `statement` is reached at the end only after attributes,
                // inside `AttributedStmt`: `block`, `body` and the file's loop
                // (`source_file`) stop at the end first. `accessor_list`'s is
                // inside `AccessorList`.
                //
                // Rejected text and comments before the token the node would
                // start at go where they would without it: the run is parsed
                // as if absent, and past the limit nothing is missing for it
                // to stand for, since the left-out node is the operand.
                let alone = (kind == K::Error)
                    .then(|| {
                        events[i + 1..]
                            .iter()
                            .take_while(|event| {
                                matches!(event, Event::Error { .. } | Event::TooDeep)
                            })
                            .count()
                    })
                    .filter(|reported| matches!(events.get(i + 1 + reported), Some(Event::Finish)));
                if let Some(reported) = alone {
                    chain.remove(0);
                    events[i + 1 + reported] = Event::Tombstone;
                }
                // The first token of the node, which `MR0018` covers. The
                // node's own `Start` at `i` is no token.
                let first_token = || events[i..].iter().find_map(raw_tokens);
                for kind in chain.drain(..).rev() {
                    sink.start(kind, in_place, first_token);
                }
            }
            Event::Finish => sink.finish(),
            Event::Token { kind, n_raw } => sink.token(kind, n_raw),
            // After `MR0018`, the rest of its statement reports nothing more:
            // what the parser skipped there leaves holes it would report.
            Event::Error { .. } if sink.mutes_errors() => {}
            Event::Error { message, fix } => {
                let wraps = events.get(i + 1).and_then(raw_tokens);
                sink.error(SYNTAX_ERROR, message, fix, wraps);
            }
            // Inside a flattened node, the error that covers it is reported.
            Event::TooDeep => sink.too_deep(events.get(i + 1).and_then(raw_tokens)),
            Event::Rejected => sink.rejected_in_place(),
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
    /// For the first token of each run of rejected text, or of each part of
    /// one a blank line or a comment splits that holds rejected text, the
    /// index after its last token ([`Input`]).
    rejected_runs: &'a [Option<usize>],
    /// For the last token of each run of rejected text, or of each part of
    /// one, its first token.
    rejected_run_starts: Vec<Option<usize>>,
    /// For each token, whether it lies inside a quoted run, where a comment
    /// is rejected text ([`Input`]).
    quoted: &'a [bool],
    /// Where each string the lexer found unclosed (`MR0012`) ends.
    unclosed_strings: FxHashSet<TextSize>,
    /// The last token of the part of each run of rejected text a node took
    /// in place of something missing ([`Event::Rejected`]): the part
    /// stands for it.
    taken_in_place: FxHashSet<usize>,
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
    /// For each block or member list opened while muted, its place in
    /// `open` and the mute to restore when it closes: its statements and
    /// members get errors of their own.
    suspended: Vec<(usize, usize)>,
    /// The tree so far.
    builder: TreeBuilder,
    /// The syntax errors so far.
    errors: Vec<SyntaxError>,
}

impl Sink<'_> {
    fn kind(&self, i: usize) -> Option<SyntaxKind> {
        self.lexed.tokens.get(i).map(|token| token.kind)
    }

    /// The first token of the run of rejected text whose last token is `i`.
    fn rejected_run_ending_at(&self, i: usize) -> Option<usize> {
        self.rejected_run_starts.get(i).copied().flatten()
    }

    /// Whether token `i` lies inside a quoted run, where a comment is
    /// rejected text; never, when the text has no rejected text.
    fn quoted(&self, i: usize) -> bool {
        self.quoted.get(i).copied().unwrap_or(false)
    }

    /// The index after the run of rejected text that starts at token `i`.
    fn rejected_run(&self, i: usize) -> Option<usize> {
        self.rejected_runs.get(i).copied().flatten()
    }

    /// The index of the next token that is neither trivia nor rejected
    /// text, or the end.
    fn next_significant(&self) -> usize {
        let mut i = self.raw;
        loop {
            if let Some(end) = self.rejected_run(i) {
                i = end;
            } else if self.kind(i).is_some_and(SyntaxKind::is_trivia) {
                i += 1;
            } else {
                return i;
            }
        }
    }

    /// The index of the next run of rejected text, past trivia, or of the
    /// next token when none comes first.
    fn next_rejected(&self) -> usize {
        let mut i = self.raw;
        while self.rejected_run(i).is_none() && self.kind(i).is_some_and(SyntaxKind::is_trivia) {
            i += 1;
        }
        i
    }

    /// Adds the tokens up to `until`, each run of rejected text in an
    /// `Error` node.
    fn add_raw(&mut self, until: usize) {
        while self.raw < until {
            if let Some(end) = self.rejected_run(self.raw) {
                self.builder.start_node(K::Error);
                self.add_tokens(end);
                self.builder.finish_node();
            } else {
                self.add_tokens(self.raw + 1);
            }
        }
    }

    fn add_tokens(&mut self, until: usize) {
        while self.raw < until {
            let token = self.lexed.tokens[self.raw];
            self.builder.token(token.kind, &self.text[token.range]);
            self.raw += 1;
        }
    }

    /// Adds the first part of the run of rejected text before the next
    /// token to the node just opened, which stands for what is missing
    /// there. The parts after it, below a blank line or from a comment on,
    /// go in as they would anywhere else, so a comment among them attaches
    /// to the declaration below.
    fn rejected_in_place(&mut self) {
        if self.flattened.is_some() {
            return;
        }
        if let Some(end) = self.rejected_run(self.next_rejected()) {
            self.add_tokens(end);
            self.taken_in_place.insert(end - 1);
        }
    }

    /// Opens a node; `in_place` when it takes the rejected text before the
    /// next token ([`Event::Rejected`]), which then goes inside it.
    /// `first_token` gives how many tokens of [`Lexed::tokens`] the node's
    /// first token spans.
    fn start(
        &mut self,
        kind: SyntaxKind,
        in_place: bool,
        first_token: impl FnOnce() -> Option<usize>,
    ) {
        if let Some(open) = &mut self.flattened {
            *open += 1;
            return;
        }
        let parent = self.open.last().map(|&(parent, _)| parent);
        let nests = nests(kind, parent);
        if nests && self.levels == MAX_DEPTH {
            self.too_deep(first_token());
            self.flattened = Some(0);
            self.open_node(K::Error, false, in_place);
        } else {
            self.open_node(kind, nests, in_place);
        }
    }

    /// Whether a syntax error here goes unreported: it is in the statement
    /// of an `MR0018`, or in the brackets of a block or member list that
    /// statement holds, which are not in any statement of their own.
    fn mutes_errors(&self) -> bool {
        self.muted.is_some()
            || self
                .suspended
                .last()
                .is_some_and(|&(at, _)| at + 1 == self.open.len())
    }

    /// Reports `MR0018` for the next node, unless an error already covers
    /// the statement it is in. `wraps` as in [`Sink::error`].
    fn too_deep(&mut self, wraps: Option<usize>) {
        if self.muted.is_some() {
            return;
        }
        self.error(codes::MR0018, TOO_DEEP.into(), None, wraps);
        // The statement is the node directly inside the innermost block,
        // list of members, cases or enum members, or inside the file: a
        // statement of a block, or a member with its `;`. When nothing is
        // open inside that yet, the node going past the limit is the
        // statement.
        let inside = self
            .open
            .iter()
            .rposition(|&(kind, _)| is_container(kind))
            .map_or(1, |i| i + 1);
        self.muted = Some(inside + 1);
    }

    fn open_node(&mut self, kind: SyntaxKind, nests: bool, in_place: bool) {
        if is_container(kind)
            && let Some(around) = self.muted.take()
        {
            self.suspended.push((self.open.len(), around));
        }
        self.open.push((kind, nests));
        self.levels += usize::from(nests);
        if self.open.len() == 1 {
            self.builder.start_node(kind);
            return;
        }
        let end = if in_place {
            self.next_rejected()
        } else {
            self.next_significant()
        };
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
    /// Rejected text counts as nothing, and the trivia between its tokens
    /// as it would without them: a line that holds only rejected text is a
    /// blank line. A comment among rejected text attaches the part of its
    /// run it is in, which starts at the comment
    /// ([`super::rejected::record_run`]), so no rejected text from before
    /// the comment comes with it; a comment in a part with no rejected text
    /// is in none, and attaches on its own. A comment inside a quoted run is
    /// rejected text.
    fn attached_comments(&self, end: usize) -> usize {
        let mut from = end;
        let mut i = end;
        let mut newline_seen = false;
        // The first token of the run of rejected text `i` is in.
        let mut run = None;
        while i > self.raw {
            i -= 1;
            if run.is_none() {
                run = self.rejected_run_ending_at(i);
            }
            let kind = self.kind(i);
            match (run, kind) {
                (_, Some(kind)) if kind.is_comment() && !self.quoted(i) => {
                    from = run.unwrap_or(i);
                    newline_seen = false;
                }
                (_, Some(K::Newline)) if newline_seen => break,
                (_, Some(K::Newline)) => newline_seen = true,
                (Some(_), _) | (None, Some(K::Whitespace)) => {}
                (None, _) => break,
            }
            if run == Some(i) {
                run = None;
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
        if let Some(&(at, around)) = self.suspended.last()
            && at == self.open.len()
        {
            self.suspended.pop();
            self.muted = Some(around);
        }
        if self.open.is_empty() {
            // The trivia at the end of the file belongs to the root.
            self.add_raw(self.lexed.tokens.len());
        }
        self.builder.finish_node();
    }

    /// Adds `n_raw` tokens, from the next significant one, as one token of
    /// `kind`, with any rejected text among them inside it.
    fn token(&mut self, kind: SyntaxKind, n_raw: usize) {
        let first = self.next_significant();
        self.add_raw(first);
        let Some(range) = self.raw_range(first, n_raw) else {
            return;
        };
        self.builder.token(kind, &self.text[range]);
        self.raw = (first + n_raw).min(self.lexed.tokens.len());
    }

    /// The text of the `n_raw` tokens of [`Lexed::tokens`] from `first`, up
    /// to the end; `None` when that is no token.
    fn raw_range(&self, first: usize, n_raw: usize) -> Option<TextRange> {
        let tokens = self
            .lexed
            .tokens
            .get(first..(first + n_raw).min(self.lexed.tokens.len()))?;
        let (start, end) = (tokens.first()?, tokens.last()?);
        Some(TextRange::new(start.range.start(), end.range.end()))
    }

    /// Whether the lexer already reported the cause of a syntax error at
    /// `range`: it marks something missing at the end of a string the lexer
    /// found unclosed (`MR0012`). An unclosed string runs to the end of the
    /// line and takes the token that was meant to follow it. An unclosed
    /// character literal ends after its first character and takes nothing,
    /// so what is missing after it is reported.
    fn lexer_reported(&self, range: TextRange, wraps: bool) -> bool {
        !wraps && self.unclosed_strings.contains(&range.start())
    }

    /// The last token taken, past trivia and rejected text; rejected text
    /// taken in place of something missing counts as taken.
    fn last_taken(&self) -> Option<usize> {
        let mut i = self.raw;
        loop {
            i = i.checked_sub(1)?;
            if self.taken_in_place.contains(&i) {
                return Some(i);
            }
            if let Some(start) = self.rejected_run_ending_at(i) {
                i = start;
            } else if !self.lexed.tokens[i].kind.is_trivia() {
                return Some(i);
            }
        }
    }

    /// Reports `code`: on the token that follows, when the error wraps it
    /// in an `Error` node (`wraps` is then how many tokens of
    /// [`Lexed::tokens`] it spans, as `>\>` spans three); otherwise just
    /// after the last token taken, where something is missing.
    /// A `fix` replaces the token the error wraps, rejected text inside it
    /// included; without one, it is dropped.
    fn error(&mut self, code: Code, message: String, fix: Option<TokenFix>, wraps: Option<usize>) {
        let next = self.next_significant();
        let wrapped = wraps.and_then(|n_raw| self.raw_range(next, n_raw.max(1)));
        let range = if let Some(range) = wrapped {
            range
        } else {
            let end = self
                .last_taken()
                .map_or(TextSize::new(0), |i| self.lexed.tokens[i].range.end());
            TextRange::empty(end)
        };
        if code == SYNTAX_ERROR && self.lexer_reported(range, wrapped.is_some()) {
            return;
        }
        let mut error = SyntaxError::new(code, range, message);
        if let (Some(range), Some(fix)) = (wrapped, fix) {
            error = error.with_fix(SyntaxFix {
                message: fix.message,
                edits: vec![(range, fix.replacement)],
            });
        }
        self.errors.push(error);
    }
}

/// How many tokens of [`Lexed::tokens`] the token `event` takes, when it
/// takes one.
fn raw_tokens(event: &Event) -> Option<usize> {
    match event {
        Event::Token { n_raw, .. } => Some(*n_raw),
        _ => None,
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

/// Whether a node of this kind holds statements, members, union cases or
/// enum members, each of which gets its own `MR0018`.
fn is_container(kind: SyntaxKind) -> bool {
    matches!(kind, K::Block | K::MemberList | K::UnionBody | K::EnumBody)
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
