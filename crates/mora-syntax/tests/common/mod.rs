//! Checks the integration tests of the parser share.

use mora_syntax::{SyntaxKind, parse};

/// Asserts that every `Error` node in the tree of `text` holds a token that
/// is not trivia: a comment between parts of rejected text is trivia, as
/// it would be without them, rejected text taken in place of something
/// missing is in the node that stands for it, and no `Error` node holds no
/// token at all, as one past the nesting limit that takes nothing, or one
/// for an error at the end of the text, would. It holds for every text
/// [`parse`] parses. A parse past its budget of events (`parse_within`) can
/// still leave one, where it stops.
pub fn assert_errors_hold_tokens(text: &str) {
    for node in parse(text).syntax().descendants() {
        if node.kind() == SyntaxKind::Error {
            assert!(
                node.tokens().any(|token| !token.kind().is_trivia()),
                "{text:?}: {node:?} holds no token but trivia"
            );
        }
    }
}
