//! Lexer, parser, lossless syntax tree and typed AST views (§3, §14.4).

pub mod ast;
mod generated;
pub mod lexer;
mod parser;
mod tree;

pub use generated::SyntaxKind;
pub use lexer::{Lexed, SyntaxError, SyntaxFix, Token, lex};
pub use parser::{Parse, parse};
pub use tree::{
    Checkpoint, GreenNode, SyntaxElement, SyntaxElementChildren, SyntaxNode, SyntaxNodeChildren,
    SyntaxToken, TreeBuilder,
};
