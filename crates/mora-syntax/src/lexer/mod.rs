//! The lexer (§3, §14.4).
//!
//! [`lex`] turns source text into tokens that cover every byte, trivia
//! included, so the parser can build a lossless tree. Ordinary tokens come
//! from a `logos` automaton. Identifiers, comments, directives, character
//! literals and every string form are lexed by hand, because interpolated
//! strings need a mode stack: a hole holds code, and that code can hold
//! another interpolated string.
//!
//! An interpolated string becomes several tokens: [`SyntaxKind::InterpStart`],
//! then [`SyntaxKind::InterpText`] pieces and holes, then
//! [`SyntaxKind::InterpEnd`]. A hole is [`SyntaxKind::HoleOpen`], the ordinary
//! tokens of its expression (and of an alignment after a comma), an optional
//! [`SyntaxKind::HoleFormat`], and [`SyntaxKind::HoleClose`].
//!
//! `>` is always its own token, so `List<List<int>>` closes two type
//! argument lists; the parser joins adjacent `>`, `>=` and `>>` pieces in
//! expressions with [`Lexed::is_joined`].

mod escape;
pub mod literal;
pub mod number;

use std::borrow::Cow;

use logos::Logos;
use mora_diag::{Code, Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{FileId, Span, TextRange, to_text_size};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::SyntaxKind;

/// One token: its kind and where it is in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
}

/// A lexical or syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// The diagnostic code (§17.6).
    pub code: Code,
    /// The text the error is about; empty where something is missing.
    pub range: TextRange,
    /// What is wrong.
    pub message: String,
    /// Extra explanation, such as what to write instead.
    pub note: Option<String>,
    /// A change that removes the error.
    pub fix: Option<SyntaxFix>,
}

impl SyntaxError {
    pub fn new(code: Code, range: TextRange, message: impl Into<String>) -> Self {
        Self {
            code,
            range,
            message: message.into(),
            note: None,
            fix: None,
        }
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    #[must_use]
    pub fn with_fix(mut self, fix: SyntaxFix) -> Self {
        self.fix = Some(fix);
        self
    }

    /// The error as a diagnostic in `file`.
    pub fn to_diagnostic(&self, file: FileId) -> Diagnostic {
        let span = |range: TextRange| Span::new(file, range);
        let mut diagnostic =
            Diagnostic::error(self.code, &self.message, Label::unlabeled(span(self.range)));
        if let Some(note) = &self.note {
            diagnostic = diagnostic.with_note(note);
        }
        if let Some(fix) = &self.fix {
            let edits = fix
                .edits
                .iter()
                .map(|(range, text)| TextEdit::replace(span(*range), text))
                .collect();
            diagnostic = diagnostic.with_fix(Fix::new(&fix.message, edits));
        }
        diagnostic
    }
}

/// Edits to the text that fix a [`SyntaxError`]: each replaces a range,
/// and an empty range inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxFix {
    pub message: String,
    pub edits: Vec<(TextRange, String)>,
}

impl SyntaxFix {
    /// `text` with the edits applied. The ranges must not overlap.
    pub fn apply(&self, text: &str) -> String {
        let mut edits: Vec<_> = self.edits.iter().collect();
        edits.sort_by_key(|(range, _)| range.start());
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (range, replacement) in edits {
            out.push_str(&text[at..usize::from(range.start())]);
            out.push_str(replacement);
            at = usize::from(range.end());
        }
        out.push_str(&text[at..]);
        out
    }
}

/// The tokens of a text and the errors found while lexing it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<SyntaxError>,
}

impl Lexed {
    /// Whether token `i` is directly followed by a token that is not trivia,
    /// so that the parser may join them, as in `>>` or `?[`.
    pub fn is_joined(&self, i: usize) -> bool {
        self.tokens
            .get(i + 1)
            .is_some_and(|next| !next.kind.is_trivia())
    }
}

/// The name an identifier token declares or refers to, in Unicode
/// normalization form C, so that two spellings that differ only in
/// normalization name the same thing (§3.1, §3.3).
pub fn ident_name(text: &str) -> Cow<'_, str> {
    if is_nfc(text) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.nfc().collect())
    }
}

/// Lexes `text`. Every byte belongs to exactly one token, in order. The
/// text must be smaller than 4 GiB; the driver rejects larger files
/// (`MR0003`, §3.1).
pub fn lex(text: &str) -> Lexed {
    let mut lexer = Lexer {
        text,
        pos: 0,
        stack: Vec::new(),
        holes: Vec::new(),
        out: Lexed::default(),
    };
    while lexer.pos < text.len() {
        match lexer.stack.last().copied() {
            Some(Frame::Text(s)) => lexer.interp_text(s),
            Some(Frame::Hole(s, depth, hole)) => lexer.hole_token(s, depth, hole),
            None => lexer.code_token(),
        }
    }
    if let Some(Frame::Text(s) | Frame::Hole(s, _, _)) = lexer.stack.first().copied() {
        lexer.error(
            codes::MR0012,
            s.start..s.start + s.prefix,
            "this interpolated string is not closed",
        );
    }
    lexer.out
}

/// An interpolated string being lexed.
#[derive(Debug, Clone, Copy)]
struct Interp {
    /// Where its prefix starts.
    start: usize,
    /// The length of `$"`, `` $` ``, `` $$` `` and so on.
    prefix: usize,
    /// The number of backticks around a raw string, zero otherwise.
    delim_len: usize,
    /// How many braces open or close a hole: the number of `$` of a raw
    /// string, one otherwise.
    dollars: usize,
    /// The index in `Lexer::holes` of its first hole.
    first_hole: usize,
}

#[derive(Debug, Clone, Copy)]
enum Frame {
    /// Inside the text of an interpolated string.
    Text(Interp),
    /// Inside a hole: the depth of open brackets in it and its index in
    /// `Lexer::holes`.
    Hole(Interp, u32, usize),
}

struct Lexer<'a> {
    /// The whole text.
    text: &'a str,
    /// Where the next token starts.
    pos: usize,
    /// The interpolated strings and holes `pos` is inside, innermost last.
    stack: Vec<Frame>,
    /// The byte ranges of holes, for the indentation rules of raw strings.
    holes: Vec<(usize, usize)>,
    /// The tokens and errors so far.
    out: Lexed,
}

/// Ordinary tokens. Everything that needs more than a regular expression,
/// or that starts with a non-ASCII character, is lexed by hand.
#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
enum Raw {
    #[regex(r"[ \t\x0C]+")]
    #[token("\r")]
    Whitespace,
    #[token("\n")]
    #[token("\r\n")]
    Newline,
    #[regex(r"//[^\n\r]*", allow_greedy = true)]
    LineComment,
    #[regex(r"///([^/\n\r][^\n\r]*)?", priority = 10, allow_greedy = true)]
    DocComment,
    #[regex(r"[0-9][0-9A-Za-z_]*")]
    #[regex(r"[0-9][0-9_]*\.[0-9][0-9A-Za-z_]*")]
    #[regex(r"[0-9][0-9_]*(\.[0-9][0-9_]*)?[eE][+-][0-9A-Za-z_]*")]
    Number,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(".")]
    Dot,
    #[token("..")]
    DotDot,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,
    #[token("?")]
    Question,
    #[token("??")]
    QuestionQuestion,
    #[token("??=")]
    QuestionQuestionEq,
    #[token("?.")]
    QuestionDot,
    #[token("=>")]
    FatArrow,
    #[token("->")]
    ThinArrow,
    #[token("@")]
    At,
    #[token("$")]
    Dollar,
    #[token("#")]
    Hash,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("&")]
    Amp,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("!")]
    Bang,
    #[token("~")]
    Tilde,
    #[token("<<")]
    Shl,
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("<=")]
    LtEq,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,
    #[token("=")]
    Eq,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("&=")]
    AmpEq,
    #[token("|=")]
    PipeEq,
    #[token("^=")]
    CaretEq,
    #[token("<<=")]
    ShlEq,
    #[token("++")]
    PlusPlus,
    #[token("--")]
    MinusMinus,
    #[token("+%")]
    PlusPercent,
    #[token("-%")]
    MinusPercent,
    #[token("*%")]
    StarPercent,
    #[token("+%=")]
    PlusPercentEq,
    #[token("-%=")]
    MinusPercentEq,
    #[token("*%=")]
    StarPercentEq,
}

impl Raw {
    fn kind(self) -> SyntaxKind {
        match self {
            Raw::Whitespace => SyntaxKind::Whitespace,
            Raw::Newline => SyntaxKind::Newline,
            Raw::LineComment => SyntaxKind::LineComment,
            Raw::DocComment => SyntaxKind::DocComment,
            // Classified by `number::check`.
            Raw::Number => SyntaxKind::IntNumber,
            Raw::LBrace => SyntaxKind::LBrace,
            Raw::RBrace => SyntaxKind::RBrace,
            Raw::LBracket => SyntaxKind::LBracket,
            Raw::RBracket => SyntaxKind::RBracket,
            Raw::LParen => SyntaxKind::LParen,
            Raw::RParen => SyntaxKind::RParen,
            Raw::Dot => SyntaxKind::Dot,
            Raw::DotDot => SyntaxKind::DotDot,
            Raw::Comma => SyntaxKind::Comma,
            Raw::Colon => SyntaxKind::Colon,
            Raw::Semicolon => SyntaxKind::Semicolon,
            Raw::Question => SyntaxKind::Question,
            Raw::QuestionQuestion => SyntaxKind::QuestionQuestion,
            Raw::QuestionQuestionEq => SyntaxKind::QuestionQuestionEq,
            Raw::QuestionDot => SyntaxKind::QuestionDot,
            Raw::FatArrow => SyntaxKind::FatArrow,
            Raw::ThinArrow => SyntaxKind::ThinArrow,
            Raw::At => SyntaxKind::At,
            Raw::Dollar => SyntaxKind::Dollar,
            Raw::Hash => SyntaxKind::Hash,
            Raw::Plus => SyntaxKind::Plus,
            Raw::Minus => SyntaxKind::Minus,
            Raw::Star => SyntaxKind::Star,
            Raw::Slash => SyntaxKind::Slash,
            Raw::Percent => SyntaxKind::Percent,
            Raw::Amp => SyntaxKind::Amp,
            Raw::Pipe => SyntaxKind::Pipe,
            Raw::Caret => SyntaxKind::Caret,
            Raw::Bang => SyntaxKind::Bang,
            Raw::Tilde => SyntaxKind::Tilde,
            Raw::Shl => SyntaxKind::Shl,
            Raw::EqEq => SyntaxKind::EqEq,
            Raw::BangEq => SyntaxKind::BangEq,
            Raw::Lt => SyntaxKind::Lt,
            Raw::Gt => SyntaxKind::Gt,
            Raw::LtEq => SyntaxKind::LtEq,
            Raw::AmpAmp => SyntaxKind::AmpAmp,
            Raw::PipePipe => SyntaxKind::PipePipe,
            Raw::Eq => SyntaxKind::Eq,
            Raw::PlusEq => SyntaxKind::PlusEq,
            Raw::MinusEq => SyntaxKind::MinusEq,
            Raw::StarEq => SyntaxKind::StarEq,
            Raw::SlashEq => SyntaxKind::SlashEq,
            Raw::PercentEq => SyntaxKind::PercentEq,
            Raw::AmpEq => SyntaxKind::AmpEq,
            Raw::PipeEq => SyntaxKind::PipeEq,
            Raw::CaretEq => SyntaxKind::CaretEq,
            Raw::ShlEq => SyntaxKind::ShlEq,
            Raw::PlusPlus => SyntaxKind::PlusPlus,
            Raw::MinusMinus => SyntaxKind::MinusMinus,
            Raw::PlusPercent => SyntaxKind::PlusPercent,
            Raw::MinusPercent => SyntaxKind::MinusPercent,
            Raw::StarPercent => SyntaxKind::StarPercent,
            Raw::PlusPercentEq => SyntaxKind::PlusPercentEq,
            Raw::MinusPercentEq => SyntaxKind::MinusPercentEq,
            Raw::StarPercentEq => SyntaxKind::StarPercentEq,
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || unicode_ident::is_xid_start(c)
}

fn is_ident_continue(c: char) -> bool {
    unicode_ident::is_xid_continue(c)
}

/// The length of the newline at the start of `text`, if one is there.
fn newline_len(text: &str) -> Option<usize> {
    if text.starts_with('\n') {
        Some(1)
    } else if text.starts_with("\r\n") {
        Some(2)
    } else {
        None
    }
}

/// The number of times `c` repeats at the start of `text`.
fn run(text: &str, c: u8) -> usize {
    text.bytes().take_while(|&b| b == c).count()
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn push(&mut self, kind: SyntaxKind, len: usize) {
        debug_assert!(len > 0, "empty {kind:?} token");
        let end = self.pos + len;
        self.out.tokens.push(Token {
            kind,
            range: TextRange::new(to_text_size(self.pos), to_text_size(end)),
        });
        self.pos = end;
    }

    fn error(&mut self, code: Code, range: std::ops::Range<usize>, message: impl Into<String>) {
        self.out.errors.push(SyntaxError::new(
            code,
            TextRange::new(to_text_size(range.start), to_text_size(range.end)),
            message,
        ));
    }

    /// Lexes one token of code.
    fn code_token(&mut self) {
        let rest = self.rest();
        let Some(c) = rest.chars().next() else {
            return;
        };
        if self.string_or_char() {
            return;
        }
        if rest.starts_with("/*") {
            self.block_comment();
            return;
        }
        if is_ident_start(c) {
            let len = rest
                .char_indices()
                .find(|&(i, c)| i > 0 && !is_ident_continue(c))
                .map_or(rest.len(), |(i, _)| i);
            let kind = SyntaxKind::keyword(&rest[..len]).unwrap_or(SyntaxKind::Ident);
            self.push(kind, len);
            return;
        }
        if !c.is_ascii() {
            let len = c.len_utf8();
            // A byte order mark is accepted only at the start of the file (§3.1).
            if c == '\u{FEFF}' && self.pos == 0 {
                self.push(SyntaxKind::Whitespace, len);
            } else {
                self.error(
                    codes::MR0017,
                    self.pos..self.pos + len,
                    format!("`{c}` (U+{:04X}) cannot start a token", u32::from(c)),
                );
                self.push(SyntaxKind::Unknown, len);
            }
            return;
        }
        let mut raw = Raw::lexer(rest);
        match raw.next() {
            Some(Ok(token)) => {
                let len = raw.span().len();
                let kind = if token == Raw::Number {
                    let (kind, errors) = number::check(&rest[..len]);
                    for (code, message) in errors {
                        self.error(code, self.pos..self.pos + len, message);
                    }
                    kind
                } else {
                    token.kind()
                };
                self.push(kind, len);
            }
            _ => {
                self.error(
                    codes::MR0017,
                    self.pos..self.pos + c.len_utf8(),
                    format!("`{}` cannot start a token", c.escape_default()),
                );
                self.push(SyntaxKind::Unknown, c.len_utf8());
            }
        }
    }

    /// Lexes one token inside a hole: the closing braces, a format, or code.
    fn hole_token(&mut self, s: Interp, depth: u32, hole: usize) {
        let rest = self.rest();
        if depth == 0 {
            if run(rest, b'}') >= s.dollars {
                self.push(SyntaxKind::HoleClose, s.dollars);
                if let Some(range) = self.holes.get_mut(hole) {
                    range.1 = self.pos;
                }
                self.replace_top(Frame::Text(s));
                return;
            }
            if rest.starts_with(':') {
                let plain = s.delim_len == 0;
                let stop = |c: char| c == '}' || (plain && (c == '"' || c == '\n'));
                let len = rest.find(stop).unwrap_or(rest.len());
                self.push(SyntaxKind::HoleFormat, len);
                return;
            }
        }
        let frames = self.stack.len();
        let before = self.out.tokens.len();
        self.code_token();
        if self.stack.len() != frames || self.out.tokens.len() == before {
            return;
        }
        let depth = match self.out.tokens[before].kind {
            SyntaxKind::LBrace | SyntaxKind::LParen | SyntaxKind::LBracket => depth + 1,
            SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket => {
                depth.saturating_sub(1)
            }
            _ => depth,
        };
        self.replace_top(Frame::Hole(s, depth, hole));
    }

    fn replace_top(&mut self, frame: Frame) {
        if let Some(top) = self.stack.last_mut() {
            *top = frame;
        }
    }

    /// Lexes a character literal or any string form starting at `pos`, if
    /// one starts there.
    fn string_or_char(&mut self) -> bool {
        let rest = self.rest();
        // `$"`, `` $` ``, `` $$` ``.
        let dollars = run(rest, b'$');
        let ticks = run(&rest[dollars..], b'`');
        if dollars > 0 && (ticks > 0 || rest[dollars..].starts_with('"')) {
            let raw = ticks > 0;
            let prefix = dollars + if raw { ticks } else { 1 };
            if dollars > 1 && !raw {
                self.error(
                    codes::MR0016,
                    self.pos..self.pos + prefix,
                    "more than one `$` needs a raw string, written with backticks",
                );
            }
            let s = Interp {
                start: self.pos,
                prefix,
                delim_len: ticks,
                dollars: if raw { dollars } else { 1 },
                first_hole: self.holes.len(),
            };
            self.push(SyntaxKind::InterpStart, prefix);
            self.stack.push(Frame::Text(s));
            return true;
        }
        if rest.starts_with('`') {
            self.raw_string();
            return true;
        }
        if rest.starts_with('"') {
            self.regular_string();
            return true;
        }
        if rest.starts_with('\'') {
            self.char_literal();
            return true;
        }
        false
    }

    /// The length of a `u8` suffix at `pos + len`, if there is one.
    fn u8_suffix(&self, len: usize) -> usize {
        let after = &self.rest()[len..];
        match after.strip_prefix("u8") {
            Some(next) if !next.chars().next().is_some_and(is_ident_continue) => 2,
            _ => 0,
        }
    }

    /// Checks the escape at `pos + at` and returns its length.
    fn check_escape(&mut self, at: usize) -> usize {
        let start = self.pos + at;
        match escape::escape(&self.text[start..]) {
            Ok((_, len)) => len,
            Err(err) => {
                let len = err.len;
                self.error(err.code, start..start + len, err.message);
                len
            }
        }
    }

    fn regular_string(&mut self) {
        let mut i = 1;
        loop {
            let rest = &self.text[self.pos + i..];
            match rest.bytes().next() {
                Some(b'"') => {
                    i += 1;
                    break;
                }
                Some(b'\\') => i += self.check_escape(i),
                Some(b'\n') | None => {
                    self.unterminated(i, "this string is not closed before the end of the line");
                    break;
                }
                Some(b'\r') if rest.starts_with("\r\n") => {
                    self.unterminated(i, "this string is not closed before the end of the line");
                    break;
                }
                Some(_) => i += rest.chars().next().map_or(1, char::len_utf8),
            }
        }
        let len = i + self.u8_suffix(i);
        self.push(SyntaxKind::String, len);
    }

    /// Lexes a raw string: between runs of the same number of backticks.
    fn raw_string(&mut self) {
        let delim_len = run(self.rest(), b'`');
        let mut i = delim_len;
        loop {
            let rest = &self.text[self.pos + i..];
            let n = run(rest, b'`');
            if n == delim_len {
                self.check_raw(self.pos, delim_len, self.pos + i, &[]);
                i += n;
                break;
            } else if n > 0 {
                i += n;
            } else if let Some(c) = rest.chars().next() {
                i += c.len_utf8();
            } else {
                self.unterminated(i, "this raw string is not closed");
                break;
            }
        }
        let len = i + self.u8_suffix(i);
        self.push(SyntaxKind::RawString, len);
    }

    fn char_literal(&mut self) {
        let rest = self.rest();
        let ends_line = |t: &str| t.is_empty() || newline_len(t).is_some();
        let mut i = 1;
        match rest[1..].chars().next() {
            Some('\\') => i += self.check_escape(1),
            Some('\'') => {
                self.error(
                    codes::MR0015,
                    self.pos..self.pos + 2,
                    "a character literal holds one character; `''` is empty",
                );
                self.push(SyntaxKind::Char, 2);
                return;
            }
            Some(c) if !ends_line(&rest[1..]) => i += c.len_utf8(),
            _ => {
                self.unterminated(1, "this character literal is not closed");
                self.push(SyntaxKind::Char, 1);
                return;
            }
        }
        let rest = self.rest();
        if rest[i..].starts_with('\'') {
            self.push(SyntaxKind::Char, i + 1);
            return;
        }
        // More than one character: take the rest up to a quote on this line.
        // `lines` ends a line at `\n` or `\r\n`, never at a lone `\r` (§3.1).
        let line = rest[i..].lines().next().unwrap_or_default();
        match line.find('\'') {
            Some(close) => {
                let len = i + close + 1;
                self.error(
                    codes::MR0015,
                    self.pos..self.pos + len,
                    "a character literal holds one character; use a string for more",
                );
                self.push(SyntaxKind::Char, len);
            }
            None => {
                self.unterminated(i, "this character literal is not closed");
                self.push(SyntaxKind::Char, i);
            }
        }
    }

    fn block_comment(&mut self) {
        let rest = self.rest();
        let len = match rest[2..].find("*/") {
            Some(end) => end + 4,
            None => {
                self.error(
                    codes::MR0012,
                    self.pos..self.pos + 2,
                    "this block comment is not closed",
                );
                rest.len()
            }
        };
        self.push(SyntaxKind::BlockComment, len);
    }

    /// Reports a literal that starts at `pos` and is not closed by `pos + len`.
    fn unterminated(&mut self, len: usize, message: &str) {
        let start = self.pos;
        self.error(codes::MR0012, start..start + len, message);
    }

    /// Lexes text inside an interpolated string up to a hole or the end.
    fn interp_text(&mut self, s: Interp) {
        let start = self.pos;
        let mut i = 0;
        let raw = s.delim_len > 0;
        // What ends the text: a hole, the closing delimiter, or a problem.
        enum Stop {
            Hole(usize),
            End(usize),
            Unterminated,
        }
        let stop = loop {
            let rest = &self.text[start + i..];
            let Some(c) = rest.chars().next() else {
                break Stop::Unterminated;
            };
            match c {
                // A plain string has no delimiter run: its backticks are text.
                '`' => {
                    let n = run(rest, b'`');
                    if n == s.delim_len {
                        break Stop::End(n);
                    }
                    i += n;
                }
                '"' if !raw => break Stop::End(1),
                '\\' if !raw => i += self.check_escape(i),
                '\n' | '\r' if !raw && newline_len(rest).is_some() => {
                    break Stop::Unterminated;
                }
                '{' => {
                    let n = run(rest, b'{');
                    if raw {
                        if n < s.dollars {
                            i += n;
                        } else {
                            if n - s.dollars >= s.dollars {
                                self.error(
                                    codes::MR0016,
                                    start + i..start + i + n,
                                    format!(
                                        "with {} `$`, a run of {n} `{{` is ambiguous; use more `$`",
                                        s.dollars
                                    ),
                                );
                            }
                            i += n - s.dollars;
                            break Stop::Hole(s.dollars);
                        }
                    } else if n >= 2 {
                        i += 2;
                    } else {
                        break Stop::Hole(1);
                    }
                }
                '}' => {
                    let n = run(rest, b'}');
                    if raw {
                        if n >= s.dollars {
                            self.error(
                                codes::MR0016,
                                start + i..start + i + n,
                                "a `}` that closes no hole; use more `$` to make it text",
                            );
                        }
                        i += n;
                    } else if n >= 2 {
                        i += 2;
                    } else {
                        self.error(
                            codes::MR0016,
                            start + i..start + i + 1,
                            "a `}` in an interpolated string is written `}}`",
                        );
                        i += 1;
                    }
                }
                c => i += c.len_utf8(),
            }
        };
        if i > 0 {
            self.push(SyntaxKind::InterpText, i);
        }
        match stop {
            Stop::Hole(n) => {
                self.holes.push((self.pos, usize::MAX));
                self.push(SyntaxKind::HoleOpen, n);
                self.replace_top(Frame::Hole(s, 0, self.holes.len() - 1));
            }
            Stop::End(n) => {
                if raw {
                    // Taken out for the check, which reports through `self`,
                    // and put back: no copy of the ranges.
                    let holes = std::mem::take(&mut self.holes);
                    self.check_raw(
                        s.start + s.prefix - s.delim_len,
                        s.delim_len,
                        self.pos,
                        &holes[s.first_hole..],
                    );
                    self.holes = holes;
                }
                self.push(SyntaxKind::InterpEnd, n);
                self.leave_interp();
            }
            Stop::Unterminated => {
                self.error(
                    codes::MR0012,
                    s.start..s.start + s.prefix,
                    "this interpolated string is not closed",
                );
                self.leave_interp();
            }
        }
    }

    /// Leaves the innermost interpolated string; the hole ranges are kept
    /// until the outermost one ends, since raw strings around it read them.
    fn leave_interp(&mut self) {
        self.stack.pop();
        if self.stack.is_empty() {
            self.holes.clear();
        }
    }

    /// Checks the line rules of a raw string (§3.5): `open` is where its
    /// opening delimiter starts and `close` where its closing one starts.
    /// Lines that start inside a hole are not checked.
    fn check_raw(&mut self, open: usize, delim_len: usize, close: usize, holes: &[(usize, usize)]) {
        let content_start = open + delim_len;
        let content = &self.text[content_start..close];
        let Some(first_newline) = content.find('\n') else {
            return;
        };
        // Multi-line: the opening line holds nothing but whitespace.
        let first_line = content[..first_newline].trim_end_matches('\r');
        if !first_line.chars().all(char::is_whitespace) {
            self.error(
                codes::MR0016,
                open..content_start,
                "a multi-line raw string starts its content on the line after the opening delimiter",
            );
            return;
        }
        let last_newline = content.rfind('\n').unwrap_or(first_newline);
        let indent = &content[last_newline + 1..];
        if !indent.chars().all(char::is_whitespace) {
            self.error(
                codes::MR0016,
                close..close + delim_len,
                "the closing delimiter of a multi-line raw string goes on its own line",
            );
            return;
        }
        let mut line_start = content_start + first_newline + 1;
        while line_start < content_start + last_newline + 1 {
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(close, |end| line_start + end);
            let line = self.text[line_start..line_end].trim_end_matches('\r');
            let in_hole = holes
                .iter()
                .any(|&(a, b)| a < line_start && line_start <= b);
            if !in_hole && !line.chars().all(char::is_whitespace) && !line.starts_with(indent) {
                self.error(
                    codes::MR0016,
                    line_start..line_start + (line.len() - line.trim_start().len()).max(1),
                    "this line does not start with the indentation of the closing delimiter",
                );
            }
            line_start = line_end + 1;
        }
    }
}

#[cfg(test)]
mod tests;
