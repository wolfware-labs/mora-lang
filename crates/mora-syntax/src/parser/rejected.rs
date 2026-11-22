//! Rejected text, which the parser does not see.
//!
//! The lexer reports each token it rejects (`MR0017`). A quote meant to
//! open a string or a character (one of [`QUOTES`]) is rejected too, and
//! the text up to the quote that closes it goes with it, when that quote
//! comes on the same line (a block comment over lines ends it too) before a
//! `;`, and before any piece of an interpolated string, which a run never
//! splits ([`closing_quotes`]). A quote that none closes is rejected text on
//! its own, so a stray quote takes nothing from the declarations after it.
//! Each such run of rejected text is left out of the parser's [`Input`], so
//! the parser parses the text as if the run were not there and reports
//! only what the text without it would. Trivia between the rejected tokens
//! of a run counts as it would without them. The tokens on both sides of a
//! run with no trivia in or around it stay joined, so `>\>` is still `>>`, and two
//! punctuation tokens that lex as one become one, as `.\.` makes `..`, but
//! never a comment, as `/\/` would ([`Input::merge`]). Nor do the tokens on
//! both sides change where they split: when their texts together lex at
//! another boundary, as `=` and `=>` in `=\=>` make `==` and `>`, the
//! parser keeps the two tokens it has, since a token of the tree cannot
//! start inside a token of the lexer. The tree builder puts each run back
//! where it falls, in an `Error` node of its own or inside the token it
//! joins, so the tree stays lossless. A blank line inside a run splits it
//! into parts with an `Error` node each, and each comment inside a run
//! starts a part, so a comment attaches to the declaration below as it
//! would without the run, with no rejected text from before it
//! ([`record_run`]). A part with no rejected text, only comments, gets no
//! `Error` node: its comments are trivia. A comment between a quote and
//! the quote that closes it is rejected text, not a comment: it neither
//! attaches nor splits a run.
//!
//! Where something is missing, rejected text in its place stands for it:
//! [`Parser::rejected_in_place`] takes the first part of the run into the
//! node of the missing operand or write-scope word ([`Event::Rejected`]),
//! and nothing more is reported there. The parts after it go in as they
//! would anywhere else, as after an operand written there: a comment
//! after the part that stands in place goes to the declaration below, even
//! where the text without the run leaves it in a list still open, as in
//! `G(\/* z */\` above `enum E { A }`. A comment before the run keeps it
//! from standing for what is missing only when the token after the run can
//! start a declaration that takes the comments above it
//! ([`at_item_start`]): the comment, and the run with it, then go to that
//! declaration, and what is missing is reported as it would be without
//! them. Elsewhere the comment stays before the run's `Error` node.

use super::Input;
#[cfg(doc)]
use super::{Event, Parser, items::at_item_start};
use crate::SyntaxKind;
use crate::lexer::Lexed;

use SyntaxKind as K;

/// Rejected text right before a token of [`Input`], by what comes before
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejected {
    /// No comment comes between the token before and the rejected text.
    Bare,
    /// A comment comes before the rejected text: it may belong to the
    /// declaration after the run, which then takes the run with it
    /// ([`at_item_start`]).
    AfterComment,
}

/// The tokens that make up an interpolated string around its holes.
const INTERPOLATION: &[SyntaxKind] = &[
    K::InterpStart,
    K::InterpText,
    K::HoleOpen,
    K::HoleFormat,
    K::HoleClose,
    K::InterpEnd,
];

/// Each quote meant to open a string or a character, with the quotes that
/// close it. The lexer rejects them all, so only an `Unknown`
/// token has one of these texts.
pub(super) const QUOTES: &[(&str, &[&str])] = &[
    ("\u{201c}", &["\u{201d}"]),
    ("\u{201d}", &["\u{201d}"]),
    ("\u{201e}", &["\u{201c}", "\u{201d}"]),
    ("\u{2018}", &["\u{2019}"]),
    ("\u{2019}", &["\u{2019}"]),
    ("\u{201a}", &["\u{2018}", "\u{2019}"]),
];

/// For each quote of [`QUOTES`] in `input`, the index of the quote that
/// closes it, when one of its closers comes before a `;`, the end of the
/// line or a piece of an interpolated string, which a run never splits.
/// `lines_end` tells for each token whether a line ends before it: a line
/// break, or a block comment that holds one.
/// Brackets in between are text. Also returns how many tokens the scans
/// looked at.
///
/// A scan answers for every later quote with the same closers that comes
/// before where it stopped: that quote's scan would stop there too, with
/// the same answer. So each token is scanned once for each set of closers.
pub(super) fn closing_quotes(input: &Input<'_>, lines_end: &[bool]) -> (Vec<Option<usize>>, usize) {
    let mut closing = vec![None; input.len()];
    // For each set of closers, where its last scan stopped and what it found.
    let mut last: Vec<(&[&str], usize, Option<usize>)> = Vec::new();
    let mut looked = 0;
    for (at, slot) in closing.iter_mut().enumerate() {
        if input.kind(at) != K::Unknown {
            continue;
        }
        let Some(&(_, closers)) = QUOTES
            .iter()
            .find(|(opening, _)| *opening == input.text(at))
        else {
            continue;
        };
        let memo = last.iter_mut().find(|(set, ..)| *set == closers);
        if let Some((_, stop, found)) = &memo
            && at < *stop
        {
            *slot = *found;
            continue;
        }
        let mut i = at + 1;
        let found = loop {
            let kind = input.kind(i);
            if i >= input.len()
                || kind == K::Semicolon
                || lines_end[i]
                || INTERPOLATION.contains(&kind)
            {
                break None;
            }
            if kind == K::Unknown && closers.contains(&input.text(i)) {
                break Some(i);
            }
            i += 1;
        };
        looked += i - at;
        *slot = found;
        match memo {
            Some(entry) => *entry = (closers, i, found),
            None => last.push((closers, i, found)),
        }
    }
    (closing, looked)
}

/// Records the run of rejected text over tokens `first..end` of `lexed` in
/// `runs` as the tree builder takes it: for the first token of each part,
/// the index after its last token. A blank line inside the run splits it,
/// a line that holds only rejected text counting as blank, so a comment
/// below the blank line attaches to the declaration under it without the
/// text above the blank line. Each comment starts a part too, so it
/// attaches without the rejected text before it, and a node that takes
/// the part before it in place of something missing leaves it to the
/// declaration below. The trivia between two parts is not in either. A
/// comment `quoted` marks, inside a quoted run, is rejected text. A part
/// that holds only comments, as `/* a */` in `\ /* a */ /* b */ \`, is not
/// recorded: no rejected text is in it, so its comments stay trivia, as
/// they would without the run.
pub(super) fn record_run(
    lexed: &Lexed,
    quoted: &[bool],
    first: usize,
    end: usize,
    runs: &mut [Option<usize>],
) {
    let mut start = first;
    // The last token of the part so far that is not whitespace or a line
    // break.
    let mut last = first;
    // Whether a line break came since the last comment, as rejected text
    // counts as nothing on its line, and whether a blank line did.
    let (mut newline_seen, mut blank) = (false, false);
    // Whether the part so far holds rejected text: the first one starts
    // with it.
    let mut rejected = true;
    let tokens = lexed.tokens[first..end].iter().zip(&quoted[first..end]);
    for (i, (token, &in_quotes)) in (first..end).zip(tokens) {
        let kind = token.kind;
        let comment = kind.is_comment() && !in_quotes;
        match kind {
            K::Newline => {
                blank |= newline_seen;
                newline_seen = true;
                continue;
            }
            K::Whitespace => continue,
            _ if comment => newline_seen = false,
            _ => {}
        }
        if blank || comment {
            if rejected {
                runs[start] = Some(last + 1);
            }
            (start, blank, rejected) = (i, false, false);
        }
        rejected |= !comment;
        last = i;
    }
    // The run ends with rejected text, so its last part holds some.
    runs[start] = Some(last + 1);
}
