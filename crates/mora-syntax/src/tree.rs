//! The lossless syntax tree (§14.4).
//!
//! `rowan` stores the tree: immutable green nodes, shared between versions
//! of a file, that hold each node's kind, text length and children. It is
//! an alpha release (§14.2), so it stays behind the types of this module
//! and no other crate names it.
//!
//! The views of this module read the green tree in place. A [`SyntaxNode`]
//! is a green node and its offset in the file, two words that borrow the
//! tree and are copied freely, so creating a view and iterating the
//! children of a node allocate nothing; a walk over a whole subtree
//! (`descendants`, `tokens`) keeps one stack of the nodes it is inside. A
//! view has no link to its parent: the compiler walks the tree only from
//! the root down.

use std::fmt::{self, Write};
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
// different nodes. `rowan` keeps one green node for equal subtrees, so two
// empty nodes of one kind at one offset, which the parser can leave where
// it recovered, are equal: nothing tells them apart but their position
// among the children.
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
        let mut text = String::with_capacity(usize::from(self.green.text_len()));
        for token in self.tokens() {
            text.push_str(token.text());
        }
        text
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
            end: self.offset + self.green.text_len(),
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
            // One allocation for the whole walk: deep enough for most trees,
            // where growing from empty would allocate several times.
            stack: Vec::with_capacity(32),
        }
    }

    pub fn first_token(&self) -> Option<SyntaxToken<'a>> {
        self.tokens().next()
    }

    pub fn last_token(&self) -> Option<SyntaxToken<'a>> {
        // From the back, into the last child node until a token: a loop
        // with its own stack, so a deep tree cannot overflow the call stack.
        let mut stack = vec![self.children_with_tokens()];
        while let Some(children) = stack.last_mut() {
            match children.next_back() {
                Some(SyntaxElement::Token(token)) => return Some(token),
                Some(SyntaxElement::Node(node)) => stack.push(node.children_with_tokens()),
                None => {
                    stack.pop();
                }
            }
        }
        None
    }

    /// The tree below this node as indented lines of `Kind@start..end`,
    /// with each token's text: the form snapshot tests compare. It walks
    /// with its own stack, so a deep tree cannot overflow the call stack.
    pub fn debug_tree(&self) -> String {
        let mut out = format!("{self:?}\n");
        let mut stack = vec![self.children_with_tokens()];
        loop {
            let indent = "  ".repeat(stack.len());
            let Some(children) = stack.last_mut() else {
                break;
            };
            // Writing to a `String` cannot fail.
            match children.next() {
                Some(SyntaxElement::Node(node)) => {
                    let _ = writeln!(out, "{indent}{node:?}");
                    stack.push(node.children_with_tokens());
                }
                Some(SyntaxElement::Token(token)) => {
                    let _ = writeln!(out, "{indent}{token:?}");
                }
                None => {
                    stack.pop();
                }
            }
        }
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
    /// Where the next child from the front starts.
    offset: TextSize,
    /// Where the next child from the back ends.
    end: TextSize,
}

impl<'a> Iterator for SyntaxElementChildren<'a> {
    type Item = SyntaxElement<'a>;

    fn next(&mut self) -> Option<SyntaxElement<'a>> {
        let child = self.inner.next()?;
        let offset = self.offset;
        self.offset += child.text_len();
        Some(element(child, offset))
    }
}

impl DoubleEndedIterator for SyntaxElementChildren<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let child = self.inner.next_back()?;
        self.end -= child.text_len();
        Some(element(child, self.end))
    }
}

/// The view of a green child that starts at `offset`.
fn element<'a>(
    child: rowan::NodeOrToken<&'a rowan::GreenNodeData, &'a rowan::GreenTokenData>,
    offset: TextSize,
) -> SyntaxElement<'a> {
    match child {
        rowan::NodeOrToken::Node(green) => SyntaxElement::Node(SyntaxNode { green, offset }),
        rowan::NodeOrToken::Token(green) => SyntaxElement::Token(SyntaxToken { green, offset }),
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
///
/// Tokens with the same kind and text are created once and shared. Nodes
/// are not: interning a node costs a hash of its children at every close,
/// and two equal nodes are rare outside the smallest ones.
#[derive(Debug, Default)]
pub struct TreeBuilder {
    /// The open nodes: each one's kind and the index of its first child.
    parents: Vec<(rowan::SyntaxKind, usize)>,
    /// The children of the open nodes, innermost last.
    children: Vec<rowan::NodeOrToken<rowan::GreenNode, rowan::GreenToken>>,
    /// Every distinct token so far.
    tokens: hashbrown::HashTable<rowan::GreenToken>,
}

/// A position in a [`TreeBuilder`] where a node can later be opened, so
/// that it wraps what was added since: the left operand of a binary
/// expression, for example.
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint(usize);

impl TreeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.parents.push((raw(kind), self.children.len()));
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.children.len())
    }

    /// Opens a node that starts at `checkpoint`.
    ///
    /// # Panics
    ///
    /// Panics if the checkpoint is outside the innermost open node.
    pub fn start_node_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        let first = self.parents.last().map_or(0, |&(_, first)| first);
        assert!(
            (first..=self.children.len()).contains(&checkpoint.0),
            "the checkpoint is outside the innermost open node"
        );
        self.parents.push((raw(kind), checkpoint.0));
    }

    pub fn token(&mut self, kind: SyntaxKind, text: &str) {
        let kind = raw(kind);
        let hash = token_hash(kind, text);
        let token = self
            .tokens
            .entry(
                hash,
                |token| token.kind() == kind && token.text() == text,
                |token| token_hash(token.kind(), token.text()),
            )
            .or_insert_with(|| rowan::GreenToken::new(kind, text))
            .get()
            .clone();
        self.children.push(rowan::NodeOrToken::Token(token));
    }

    /// # Panics
    ///
    /// Panics if no node is open.
    pub fn finish_node(&mut self) {
        let Some((kind, first)) = self.parents.pop() else {
            panic!("no node is open");
        };
        let node = rowan::GreenNode::new(kind, self.children.drain(first..));
        self.children.push(rowan::NodeOrToken::Node(node));
    }

    /// The finished tree.
    ///
    /// # Panics
    ///
    /// Panics if a node is still open, or if not exactly one root node was
    /// built.
    pub fn finish(mut self) -> GreenNode {
        assert!(self.parents.is_empty(), "a node is still open");
        match (self.children.pop(), self.children.is_empty()) {
            (Some(rowan::NodeOrToken::Node(root)), true) => GreenNode(root),
            _ => panic!("the tree does not have exactly one root node"),
        }
    }
}

fn token_hash(kind: rowan::SyntaxKind, text: &str) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    kind.hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

fn raw(kind: SyntaxKind) -> rowan::SyntaxKind {
    <Mora as rowan::Language>::kind_to_raw(kind)
}
