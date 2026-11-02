//! Typed views of the syntax tree.
//!
//! Each node of `mora.ungram` is a type here that wraps a [`SyntaxNode`] of
//! its kind and has a method per child. The types and methods are generated
//! by `cargo xtask codegen`; this module holds what they share. A view never
//! fails on a broken tree: a missing child is `None`, so the LSP can work on
//! code that is being typed.

use std::marker::PhantomData;

use crate::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxNodeChildren, SyntaxToken};

pub use crate::generated::nodes::*;

/// A typed view of a node of a tree that lives for `'a`.
pub trait AstNode<'a>: Sized {
    /// Whether a node of this kind can be viewed as `Self`.
    fn can_cast(kind: SyntaxKind) -> bool;

    /// The view of `node`, if its kind fits.
    fn cast(node: SyntaxNode<'a>) -> Option<Self>;

    fn syntax(&self) -> &SyntaxNode<'a>;
}

/// The children of a node that can be viewed as `N`.
#[derive(Debug, Clone)]
pub struct AstChildren<'a, N> {
    inner: SyntaxNodeChildren<'a>,
    _node: PhantomData<N>,
}

impl<'a, N: AstNode<'a>> Iterator for AstChildren<'a, N> {
    type Item = N;

    fn next(&mut self) -> Option<N> {
        self.inner.find_map(N::cast)
    }
}

/// Helpers for the generated accessors.
pub(crate) mod support {
    use super::*;

    pub(crate) fn child<'a, N: AstNode<'a>>(parent: &SyntaxNode<'a>) -> Option<N> {
        parent.children().find_map(N::cast)
    }

    /// The `n`th child that can be viewed as `N`, counting from zero: the
    /// right operand of a binary expression is the second `Expr`.
    #[allow(
        dead_code,
        reason = "generated only when no token separates two children of one type"
    )]
    pub(crate) fn nth_child<'a, N: AstNode<'a>>(parent: &SyntaxNode<'a>, n: usize) -> Option<N> {
        parent.children().filter_map(N::cast).nth(n)
    }

    /// The first child viewable as `N` that comes after a token of one of
    /// `after` (anywhere, when `after` is empty) and before any token of
    /// `before`: the `end` of `start..end` is the `Expr` after `..`.
    pub(crate) fn child_between<'a, N: AstNode<'a>>(
        parent: &SyntaxNode<'a>,
        after: &[SyntaxKind],
        before: &[SyntaxKind],
    ) -> Option<N> {
        let mut started = after.is_empty();
        for element in parent.children_with_tokens() {
            match element {
                SyntaxElement::Token(token) if !started => started = after.contains(&token.kind()),
                SyntaxElement::Token(token) if before.contains(&token.kind()) => return None,
                SyntaxElement::Node(node) if started => {
                    if let Some(found) = N::cast(node) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn children<'a, N: AstNode<'a>>(parent: &SyntaxNode<'a>) -> AstChildren<'a, N> {
        AstChildren {
            inner: parent.children(),
            _node: PhantomData,
        }
    }

    /// The first direct child token whose kind is one of `kinds`.
    pub(crate) fn token<'a>(
        parent: &SyntaxNode<'a>,
        kinds: &[SyntaxKind],
    ) -> Option<SyntaxToken<'a>> {
        parent
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| kinds.contains(&token.kind()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `a[]` with an empty argument after the base, as the parser leaves
    /// `a[, 1]`: the argument is the second child node.
    #[test]
    fn an_empty_child_after_other_nodes_is_found_by_its_position() {
        let parse = crate::parse("let x = a[, 1];\n");
        let index = parse
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::IndexExpr)
            .unwrap();
        let arg: Arg = support::child(&index).unwrap();
        assert!(arg.syntax().text_range().is_empty());
        let after: Arg = support::child_between(&index, &[SyntaxKind::LBracket], &[]).unwrap();
        assert_eq!(after.syntax(), arg.syntax());
        // An empty node has no token at all.
        assert!(arg.syntax().first_token().is_none());
        assert!(arg.syntax().last_token().is_none());
        // No `after` token, no child.
        assert!(support::child_between::<Arg>(&index, &[SyntaxKind::LParen], &[]).is_none());
    }

    #[test]
    fn the_nth_child_of_a_type_counts_only_that_type() {
        let parse = crate::parse("let x = a + b;\n");
        let bin = parse
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::BinExpr)
            .unwrap();
        let rhs: Expr = support::nth_child(&bin, 1).unwrap();
        assert_eq!(rhs.syntax().text(), "b");
        assert!(support::nth_child::<Expr>(&bin, 2).is_none());
    }
}
