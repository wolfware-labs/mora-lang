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
//! into parts with an `Error` node each, so a comment under the blank line
//! attaches to the declaration below as it would without the run
//! ([`record_run`]).
//!
//! Where something is missing, rejected text in its place stands for it:
//! [`Parser::rejected_in_place`] takes the run into the node of the missing
//! operand or write-scope word ([`Event::Rejected`]), and nothing more is
//! reported there.

use super::Input;
#[cfg(doc)]
use super::{Event, Parser};
use crate::SyntaxKind;
use crate::lexer::Lexed;

use SyntaxKind as K;

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
/// text above the blank line. The trivia between two parts is not in
/// either.
pub(super) fn record_run(lexed: &Lexed, first: usize, end: usize, runs: &mut [Option<usize>]) {
    let mut start = first;
    // The last token of the part so far that is not whitespace or a line
    // break.
    let mut last = first;
    let (mut newline_seen, mut blank) = (false, false);
    for i in first..end {
        let kind = lexed.tokens[i].kind;
        match kind {
            K::Newline => {
                blank |= newline_seen;
                newline_seen = true;
                continue;
            }
            K::Whitespace => continue,
            _ if kind.is_comment() => newline_seen = false,
            _ => {}
        }
        if blank {
            runs[start] = Some(last + 1);
            (start, blank) = (i, false);
        }
        last = i;
    }
    runs[start] = Some(last + 1);
}
