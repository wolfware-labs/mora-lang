//! The lossless syntax tree (§14.4).
//!
//! `rowan` stores the tree: immutable green nodes, shared between versions
//! of a file, that hold each node's kind, text length and children. It is
//! an alpha release (§14.2), so it stays behind the types of this module
//! and no other crate names it.
//!
//! The views of this module read the green tree in place. A [`SyntaxNode`]
//! is a green node and its offset in the file, two words that borrow the
//! tree and are copied freely, so walking the tree allocates nothing. A
//! view has no link to its parent: the compiler walks the tree only from
//! the root down.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ptr;

use mora_span::{TextRange, TextSize};

use crate::SyntaxKind;

/// The `rowan` language of Mora trees: it maps kinds to and from `rowan`'s
/// raw kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Mora {}

impl rowan::Language for Mora {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::from_raw(raw.0).unwrap_or(SyntaxKind::Error)
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

fn kind(raw: rowan::SyntaxKind) -> SyntaxKind {
    <Mora as rowan::Language>::kind_from_raw(raw)
}

/// A node of the tree: a kind, its children and its place in the file.
#[derive(Clone, Copy)]
pub struct SyntaxNode<'a> {
    green: &'a rowan::GreenNodeData,
    offset: TextSize,
}

/// A token of the tree, trivia included.
#[derive(Clone, Copy)]
pub struct SyntaxToken<'a> {
    green: &'a rowan::GreenTokenData,
    offset: TextSize,
}

/// A child of a node: a node or a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxElement<'a> {
    Node(SyntaxNode<'a>),
    Token(SyntaxToken<'a>),
}

// Two views are equal when they are the same part of the same tree: one
// green node can appear at several offsets, and equal text can be two
// different nodes.
impl PartialEq for SyntaxNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.green, other.green) && self.offset == other.offset
    }
}

impl Eq for SyntaxNode<'_> {}

impl Hash for SyntaxNode<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::hash(self.green, state);
        self.offset.hash(state);
    }
}

impl PartialEq for SyntaxToken<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.green, other.green) && self.offset == other.offset
    }
}

impl Eq for SyntaxToken<'_> {}

impl Hash for SyntaxToken<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::hash(self.green, state);
        self.offset.hash(state);
    }
}

impl<'a> SyntaxNode<'a> {
    pub fn kind(&self) -> SyntaxKind {
        kind(self.green.kind())
    }

    /// Where the node is, trivia inside it included.
    pub fn text_range(&self) -> TextRange {
        TextRange::at(self.offset, self.green.text_len())
    }

    /// The text of the node, exactly as in the source.
    pub fn text(&self) -> String {
        self.green.to_string()
    }

    /// The child nodes, without tokens.
    pub fn children(&self) -> SyntaxNodeChildren<'a> {
        SyntaxNodeChildren(self.children_with_tokens())
    }

    /// The child nodes and tokens, in order.
    pub fn children_with_tokens(&self) -> SyntaxElementChildren<'a> {
        SyntaxElementChildren {
            inner: self.green.children(),
            offset: self.offset,
        }
    }

    /// The node and every node below it, in preorder.
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode<'a>> + use<'a> {
        self.preorder().filter_map(SyntaxElement::into_node)
    }

    /// The tokens of the node and every node below it, in order.
    pub fn tokens(&self) -> impl Iterator<Item = SyntaxToken<'a>> + use<'a> {
        self.preorder().filter_map(SyntaxElement::into_token)
    }

    /// The node and everything below it, in preorder.
    fn preorder(&self) -> Preorder<'a> {
        Preorder {
            first: Some(*self),
            stack: Vec::new(),
        }
    }

    pub fn first_token(&self) -> Option<SyntaxToken<'a>> {
        self.children_with_tokens().find_map(|child| match child {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(node) => node.first_token(),
        })
    }

    pub fn last_token(&self) -> Option<SyntaxToken<'a>> {
        let children: Vec<_> = self.children_with_tokens().collect();
        children.into_iter().rev().find_map(|child| match child {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(node) => node.last_token(),
        })
    }

    /// The tree below this node as indented lines of `Kind@start..end`,
    /// with each token's text: the form snapshot tests compare.
    pub fn debug_tree(&self) -> String {
        fn write(node: SyntaxNode<'_>, depth: usize, out: &mut String) {
            let indent = "  ".repeat(depth);
            out.push_str(&format!("{indent}{node:?}\n"));
            for child in node.children_with_tokens() {
                match child {
                    SyntaxElement::Node(child) => write(child, depth + 1, out),
                    SyntaxElement::Token(token) => {
                        out.push_str(&format!("{indent}  {token:?}\n"));
                    }
                }
            }
        }
        let mut out = String::new();
        write(*self, 0, &mut out);
        out
    }
}

impl fmt::Debug for SyntaxNode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.text_range())
    }
}

impl fmt::Display for SyntaxNode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.green, f)
    }
}

/// The child nodes and tokens of a node, with their offsets.
#[derive(Debug, Clone)]
pub struct SyntaxElementChildren<'a> {
    inner: rowan::Children<'a>,
    offset: TextSize,
}

impl<'a> Iterator for SyntaxElementChildren<'a> {
    type Item = SyntaxElement<'a>;

    fn next(&mut self) -> Option<SyntaxElement<'a>> {
        let child = self.inner.next()?;
        let offset = self.offset;
        self.offset += child.text_len();
        Some(match child {
            rowan::NodeOrToken::Node(green) => SyntaxElement::Node(SyntaxNode { green, offset }),
            rowan::NodeOrToken::Token(green) => SyntaxElement::Token(SyntaxToken { green, offset }),
        })
    }
}

/// The child nodes of a node.
#[derive(Debug, Clone)]
pub struct SyntaxNodeChildren<'a>(SyntaxElementChildren<'a>);

impl<'a> Iterator for SyntaxNodeChildren<'a> {
    type Item = SyntaxNode<'a>;

    fn next(&mut self) -> Option<SyntaxNode<'a>> {
        self.0.find_map(SyntaxElement::into_node)
    }
}

/// A node and everything below it, in preorder.
struct Preorder<'a> {
    /// The node itself, until it is returned.
    first: Option<SyntaxNode<'a>>,
    /// The children still to visit of each node entered.
    stack: Vec<SyntaxElementChildren<'a>>,
}

impl<'a> Iterator for Preorder<'a> {
    type Item = SyntaxElement<'a>;

    fn next(&mut self) -> Option<SyntaxElement<'a>> {
        if let Some(node) = self.first.take() {
            self.stack.push(node.children_with_tokens());
            return Some(SyntaxElement::Node(node));
        }
        loop {
            let children = self.stack.last_mut()?;
            match children.next() {
                Some(SyntaxElement::Node(node)) => {
                    self.stack.push(node.children_with_tokens());
                    return Some(SyntaxElement::Node(node));
                }
                Some(token) => return Some(token),
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

impl<'a> SyntaxToken<'a> {
    pub fn kind(&self) -> SyntaxKind {
        kind(self.green.kind())
    }

    pub fn text(&self) -> &'a str {
        self.green.text()
    }

    pub fn text_range(&self) -> TextRange {
        TextRange::at(self.offset, self.green.text_len())
    }
}

impl fmt::Debug for SyntaxToken<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}@{:?} {:?}",
            self.kind(),
            self.text_range(),
            self.text()
        )
    }
}

impl fmt::Display for SyntaxToken<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl<'a> SyntaxElement<'a> {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            SyntaxElement::Node(node) => node.kind(),
            SyntaxElement::Token(token) => token.kind(),
        }
    }

    pub fn text_range(&self) -> TextRange {
        match self {
            SyntaxElement::Node(node) => node.text_range(),
            SyntaxElement::Token(token) => token.text_range(),
        }
    }

    pub fn into_node(self) -> Option<SyntaxNode<'a>> {
        match self {
            SyntaxElement::Node(node) => Some(node),
            SyntaxElement::Token(_) => None,
        }
    }

    pub fn into_token(self) -> Option<SyntaxToken<'a>> {
        match self {
            SyntaxElement::Node(_) => None,
            SyntaxElement::Token(token) => Some(token),
        }
    }
}

/// An immutable tree without parents or offsets, cheap to clone and to
/// share between versions of a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GreenNode(rowan::GreenNode);

impl GreenNode {
    /// The tree with this node at its root, borrowing this node.
    pub fn root(&self) -> SyntaxNode<'_> {
        SyntaxNode {
            green: &self.0,
            offset: TextSize::new(0),
        }
    }

    pub fn text_len(&self) -> TextSize {
        self.0.text_len()
    }
}

/// Builds a tree from events: open a node, add tokens, close it.
#[derive(Debug, Default)]
pub struct TreeBuilder(rowan::GreenNodeBuilder<'static>);

/// A position in a [`TreeBuilder`] where a node can later be opened, so
/// that it wraps what was added since: the left operand of a binary
/// expression, for example.
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint(rowan::Checkpoint);

impl TreeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.0.start_node(raw(kind));
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.0.checkpoint())
    }

    /// Opens a node that starts at `checkpoint`.
    pub fn start_node_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.0.start_node_at(checkpoint.0, raw(kind));
    }

    pub fn token(&mut self, kind: SyntaxKind, text: &str) {
        self.0.token(raw(kind), text);
    }

    pub fn finish_node(&mut self) {
        self.0.finish_node();
    }

    /// The finished tree.
    ///
    /// # Panics
    ///
    /// Panics if a node is still open, or if not exactly one root node was
    /// built.
    pub fn finish(self) -> GreenNode {
        GreenNode(self.0.finish())
    }
}

fn raw(kind: SyntaxKind) -> rowan::SyntaxKind {
    <Mora as rowan::Language>::kind_to_raw(kind)
}
