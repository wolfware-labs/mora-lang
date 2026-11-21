//! Rejected text, which the parser does not see.
//!
//! The lexer reports each token it rejects (`MR0017`). A quote meant to
//! open a string or a character (one of [`QUOTES`]) is rejected too, and
//! the text up to the quote that closes it goes with it, when that quote
//! comes before the statement ends ([`closing_quotes`]). Each such run of
//! rejected text is left out of the parser's [`Input`], so the parser
//! parses the text as if the run were not there and reports only what the
//! text without it would. The tree builder puts each run back where it
//! falls, in an `Error` node of its own, so the tree stays lossless.
//!
//! Where something is missing, rejected text in its place stands for it:
//! [`Parser::rejected_in_place`] takes the run into the node of the missing
//! operand or write-scope word ([`Event::Rejected`]), and nothing more is
//! reported there.

use super::Input;
#[cfg(doc)]
use super::{Event, Parser};
use crate::SyntaxKind;

use SyntaxKind as K;

/// The keywords that start a statement, which no expression holds: a line
/// that starts with one ends the scan for a closing quote
/// ([`closing_quotes`]).
const STATEMENT_KEYWORDS: &[SyntaxKind] = &[
    K::LetKw,
    K::VarKw,
    K::IfKw,
    K::WhileKw,
    K::DoKw,
    K::ForKw,
    K::ForeachKw,
    K::ReturnKw,
    K::BreakKw,
    K::ContinueKw,
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
/// closes it, when one of its closers comes before the statement ends:
/// before a `;`, or a line that starts with a statement keyword or a `}`.
/// Brackets in between are text. Also returns how many tokens the scans
/// looked at.
///
/// A scan answers for every later quote with the same closers that comes
/// before where it stopped: that quote's scan would stop there too, with
/// the same answer. So each token is scanned once for each set of closers.
pub(super) fn closing_quotes(input: &Input<'_>) -> (Vec<Option<usize>>, usize) {
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
                || (input.line_start[i]
                    && (STATEMENT_KEYWORDS.contains(&kind) || kind == K::RBrace))
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
