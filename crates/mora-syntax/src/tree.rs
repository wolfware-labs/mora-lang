//! The lossless syntax tree (§14.4).
//!
//! `rowan` stores the tree: immutable green nodes shared between versions
//! of a file, and red nodes that add parents and offsets on demand. It is an
//! alpha release (§14.2), so it stays behind the types of this module and
//! no other crate names it.

use std::fmt;

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

/// A node of the tree: a kind, its children and its place in the file.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SyntaxNode(rowan::SyntaxNode<Mora>);

/// A token of the tree, trivia included.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SyntaxToken(rowan::SyntaxToken<Mora>);

/// What [`SyntaxNode::child_shapes`] tells about a child.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChildShape {
    pub(crate) kind: SyntaxKind,
    pub(crate) is_node: bool,
    pub(crate) range: TextRange,
}

/// A child of a node: a node or a token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

impl SyntaxNode {
    pub fn kind(&self) -> SyntaxKind {
        self.0.kind()
    }

    /// Where the node is, trivia inside it included.
    pub fn text_range(&self) -> TextRange {
        self.0.text_range()
    }

    /// The text of the node, exactly as in the source.
    pub fn text(&self) -> String {
        self.0.text().to_string()
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.0.parent().map(SyntaxNode)
    }

    /// The node and its parents, innermost first.
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        self.0.ancestors().map(SyntaxNode)
    }

    /// The child nodes, without tokens.
    pub fn children(&self) -> SyntaxNodeChildren {
        SyntaxNodeChildren(self.0.children())
    }

    /// The child nodes and tokens, in order.
    pub fn children_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + use<> {
        self.0.children_with_tokens().map(SyntaxElement::from)
    }

    /// The node and every node below it, in preorder.
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        self.0.descendants().map(SyntaxNode)
    }

    /// The tokens of the node and every node below it, in order.
    pub fn tokens(&self) -> impl Iterator<Item = SyntaxToken> + use<> {
        self.0
            .descendants_with_tokens()
            .filter_map(|element| element.into_token().map(SyntaxToken))
    }

    /// The kind, start and length of each direct child, read from the
    /// green tree. Unlike [`SyntaxNode::children_with_tokens`], this creates
    /// no node or token, so the typed views use it to find a child before
    /// they create the one they return.
    pub(crate) fn child_shapes(&self) -> impl Iterator<Item = ChildShape> + '_ {
        let mut offset = self.0.text_range().start();
        self.0.green().children().map(move |child| {
            let start = offset;
            let len = child.text_len();
            offset += len;
            ChildShape {
                kind: <Mora as rowan::Language>::kind_from_raw(child.kind()),
                is_node: matches!(child, rowan::NodeOrToken::Node(_)),
                range: TextRange::at(start, len),
            }
        })
    }

    /// The child node that [`SyntaxNode::child_shapes`] described as
    /// `shape`, the `nodes_before`th child node.
    pub(crate) fn child_node(&self, shape: ChildShape, nodes_before: usize) -> Option<SyntaxNode> {
        // A non-empty child is the only one that covers its range; an empty
        // one may share its offset with others, so it is counted out.
        if !shape.range.is_empty()
            && let Some(rowan::NodeOrToken::Node(node)) =
                self.0.child_or_token_at_range(shape.range)
            && node.text_range() == shape.range
        {
            return Some(SyntaxNode(node));
        }
        self.children().nth(nodes_before)
    }

    /// The child token that [`SyntaxNode::child_shapes`] described as
    /// `shape`. Tokens are never empty, so its range finds it.
    pub(crate) fn child_token(&self, shape: ChildShape) -> Option<SyntaxToken> {
        match self.0.child_or_token_at_range(shape.range)? {
            rowan::NodeOrToken::Token(token) if token.text_range() == shape.range => {
                Some(SyntaxToken(token))
            }
            // Not reached for a well-formed tree; fall back to a scan.
            _ => self
                .0
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.text_range() == shape.range)
                .map(SyntaxToken),
        }
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        self.0.first_token().map(SyntaxToken)
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        self.0.last_token().map(SyntaxToken)
    }

    /// The tree below this node as indented lines of `Kind@start..end`,
    /// with each token's text: the form snapshot tests compare.
    pub fn debug_tree(&self) -> String {
        let mut out = String::new();
        let mut depth = 0;
        for event in self.0.preorder_with_tokens() {
            match event {
                rowan::WalkEvent::Enter(element) => {
                    let range = element.text_range();
                    let indent = "  ".repeat(depth);
                    let span = format!("{:?}@{:?}", element.kind(), range);
                    match element {
                        rowan::NodeOrToken::Node(_) => {
                            out.push_str(&format!("{indent}{span}\n"));
                            depth += 1;
                        }
                        rowan::NodeOrToken::Token(token) => {
                            out.push_str(&format!("{indent}{span} {:?}\n", token.text()));
                        }
                    }
                }
                rowan::WalkEvent::Leave(rowan::NodeOrToken::Node(_)) => depth -= 1,
                rowan::WalkEvent::Leave(rowan::NodeOrToken::Token(_)) => {}
            }
        }
        out
    }
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.text_range())
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.text(), f)
    }
}

/// The child nodes of a node.
#[derive(Debug, Clone)]
pub struct SyntaxNodeChildren(rowan::SyntaxNodeChildren<Mora>);

impl Iterator for SyntaxNodeChildren {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<SyntaxNode> {
        self.0.next().map(SyntaxNode)
    }
}

impl SyntaxToken {
    pub fn kind(&self) -> SyntaxKind {
        self.0.kind()
    }

    pub fn text(&self) -> &str {
        self.0.text()
    }

    pub fn text_range(&self) -> TextRange {
        self.0.text_range()
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.0.parent().map(SyntaxNode)
    }

    /// The next token in the file, possibly trivia.
    pub fn next_token(&self) -> Option<SyntaxToken> {
        self.0.next_token().map(SyntaxToken)
    }

    /// The previous token in the file, possibly trivia.
    pub fn prev_token(&self) -> Option<SyntaxToken> {
        self.0.prev_token().map(SyntaxToken)
    }
}

impl fmt::Debug for SyntaxToken {
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

impl fmt::Display for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl SyntaxElement {
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

    pub fn into_node(self) -> Option<SyntaxNode> {
        match self {
            SyntaxElement::Node(node) => Some(node),
            SyntaxElement::Token(_) => None,
        }
    }

    pub fn into_token(self) -> Option<SyntaxToken> {
        match self {
            SyntaxElement::Node(_) => None,
            SyntaxElement::Token(token) => Some(token),
        }
    }
}

impl From<rowan::SyntaxElement<Mora>> for SyntaxElement {
    fn from(element: rowan::SyntaxElement<Mora>) -> Self {
        match element {
            rowan::NodeOrToken::Node(node) => SyntaxElement::Node(SyntaxNode(node)),
            rowan::NodeOrToken::Token(token) => SyntaxElement::Token(SyntaxToken(token)),
        }
    }
}

/// An immutable tree without parents or offsets, cheap to clone and to
/// share between versions of a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GreenNode(rowan::GreenNode);

impl GreenNode {
    /// The tree with this node at its root.
    pub fn root(&self) -> SyntaxNode {
        SyntaxNode(rowan::SyntaxNode::new_root(self.0.clone()))
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
