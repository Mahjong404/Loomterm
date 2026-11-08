use crate::ImageReference;
use std::ops::Range;

pub type NodeId = usize;

/// Flat preorder AST: dropping deep input never recursively drops a tree.
/// `subtree_end` is exclusive. IDs/ranges belong only to this parsed revision.
#[derive(Debug)]
pub struct Content {
    pub source: String,
    pub nodes: Vec<Node>,
    pub stats: ParseStats,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    /// UTF-8 byte range in the original source, including a possible UTF-8 BOM.
    pub span: Range<usize>,
    pub parent: Option<NodeId>,
    pub subtree_end: NodeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Paragraph,
    Heading(u8),
    BlockQuote,
    List {
        start: Option<u64>,
    },
    Item,
    Table {
        alignments: Vec<Alignment>,
    },
    TableHead,
    TableRow,
    TableCell,
    CodeBlock {
        info: String,
    },
    Emphasis,
    Strong,
    Strikethrough,
    Link {
        destination: String,
        title: String,
    },
    Image {
        reference: ImageReference,
        title: String,
    },
    Text(String),
    Code(String),
    /// TeX-like syntax only. Never execute commands, macros, includes or URLs.
    Math {
        display: bool,
        source: String,
    },
    SoftBreak,
    HardBreak,
    Rule,
    TaskMarker(bool),
    HtmlBlock,
    /// Inert literal content; consumers must not interpret it as HTML or ANSI.
    RawHtml(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParseStats {
    pub nodes: usize,
    pub max_depth: usize,
    pub image_references: usize,
    /// Owned strings + table alignment elements (excludes allocator overhead).
    pub payload_bytes: usize,
}

impl Content {
    pub fn children(&self, parent: NodeId) -> impl Iterator<Item = (NodeId, &Node)> {
        let end = self.nodes.get(parent).map_or(0, |n| n.subtree_end);
        Children {
            content: self,
            parent,
            next: parent.saturating_add(1),
            end,
        }
    }

    /// Observable retained allocation size, excluding parser scratch and allocator overhead.
    pub fn retained_bytes(&self) -> usize {
        self.source.capacity()
            + self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.stats.payload_bytes
    }
}

struct Children<'a> {
    content: &'a Content,
    parent: NodeId,
    next: usize,
    end: usize,
}
impl<'a> Iterator for Children<'a> {
    type Item = (NodeId, &'a Node);
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        let node = &self.content.nodes[index];
        debug_assert_eq!(node.parent, Some(self.parent));
        self.next = node.subtree_end;
        Some((index, node))
    }
}
