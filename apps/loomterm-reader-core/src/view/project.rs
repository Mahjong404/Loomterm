use super::model::*;
use crate::{Alignment, Cancellation, Document, ImageReference, NodeId, NodeKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub struct Projection {
    pub document: Arc<Document>,
    pub blocks: Vec<Block>,
    pub headings: Vec<Heading>,
    pub links: Vec<NodeId>,
    pub(super) node_blocks: Vec<Option<usize>>,
    pub(super) resources: BTreeMap<NodeId, ResourceKind>,
    pub(super) owner: Arc<()>,
    pub(super) bytes: usize,
    pub(super) runs: usize,
}
impl Projection {
    pub fn build(
        document: Arc<Document>,
        limits: ViewLimits,
        cancel: &Cancellation,
    ) -> Result<Self, ViewError> {
        limits.validate()?;
        cancel.check()?;
        let count = document.content.nodes.len();
        let mut builder = Builder {
            projection: Self {
                document,
                blocks: Vec::new(),
                headings: Vec::new(),
                links: Vec::new(),
                node_blocks: vec![None; count],
                resources: BTreeMap::new(),
                owner: Arc::new(()),
                bytes: 0,
                runs: 0,
            },
            limits,
            marked_items: BTreeSet::new(),
            ordinals: BTreeMap::new(),
            cancel,
        };
        let mut counters = BTreeMap::<usize, u64>::new();
        for (id, node) in builder.projection.document.content.nodes.iter().enumerate() {
            cancel.check()?;
            if let NodeKind::Item = node.kind {
                if let Some(parent) = node.parent {
                    let counter = counters.entry(parent).or_default();
                    let start = match builder.projection.document.content.nodes[parent].kind {
                        NodeKind::List { start } => start,
                        _ => None,
                    };
                    builder
                        .ordinals
                        .insert(id, start.map(|start| start.saturating_add(*counter)));
                    *counter += 1;
                }
            }
            if matches!(node.kind, NodeKind::Link { .. }) {
                builder.projection.links.push(id);
            }
        }
        let mut id = 1;
        while id < count {
            cancel.check()?;
            let node = &builder.projection.document.content.nodes[id];
            let end = node.subtree_end;
            match node.kind {
                NodeKind::Heading(level) => {
                    builder.inline(id, id + 1..end, BlockRole::Heading(level))?;
                    id = end;
                }
                NodeKind::Paragraph => {
                    builder.inline(id, id + 1..end, BlockRole::Paragraph)?;
                    id = end;
                }
                NodeKind::CodeBlock { .. } => {
                    builder.code(id)?;
                    id = end;
                }
                NodeKind::Table { .. } => {
                    builder.table(id)?;
                    id = end;
                }
                NodeKind::Rule => {
                    let mut block = builder.block(id, BlockRole::Rule);
                    builder.push(
                        &mut block,
                        "─",
                        SourceLocation {
                            bytes: node.span.clone(),
                            exact: false,
                        },
                        Style::default(),
                        None,
                    )?;
                    builder.finish(block)?;
                    id = end;
                }
                NodeKind::Item => {
                    let mut inline_end = id + 1;
                    while inline_end < end
                        && is_inline(&builder.projection.document.content.nodes[inline_end].kind)
                    {
                        inline_end =
                            builder.projection.document.content.nodes[inline_end].subtree_end;
                    }
                    if inline_end > id + 1 {
                        builder.inline(id, id + 1..inline_end, BlockRole::Paragraph)?;
                        id = inline_end;
                    } else {
                        id += 1;
                    }
                }
                NodeKind::HtmlBlock => {
                    builder.inline(id, id + 1..end, BlockRole::Paragraph)?;
                    id = end;
                }
                _ => {
                    id += 1;
                }
            }
        }
        let mut slugs = BTreeMap::<String, usize>::new();
        for (index, block) in builder.projection.blocks.iter().enumerate() {
            if let BlockRole::Heading(level) = block.role {
                let base = slug(&block.text);
                let n = slugs.entry(base.clone()).or_default();
                let slug = if *n == 0 { base } else { format!("{base}-{n}") };
                *n += 1;
                builder.projection.headings.push(Heading {
                    node: block.node,
                    block: index,
                    level,
                    slug,
                });
            }
        }
        Ok(builder.projection)
    }

    pub fn link_target(&self, node: NodeId) -> LinkTarget {
        let Some(NodeKind::Link { destination, .. }) =
            self.document.content.nodes.get(node).map(|n| &n.kind)
        else {
            return LinkTarget::Blocked;
        };
        if let Some(fragment) = destination.strip_prefix('#') {
            return LinkTarget::Fragment(fragment.into());
        }
        let lower = destination.to_ascii_lowercase();
        if ["https://", "http://", "mailto:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
            && !destination.chars().any(char::is_control)
        {
            return LinkTarget::External(destination.clone());
        }
        match ImageReference::resolve(destination, Some(&self.document.path)) {
            ImageReference::Local { path } => LinkTarget::LocalFile {
                path,
                fragment: destination.split_once('#').map(|(_, f)| f.into()),
            },
            _ => LinkTarget::Blocked,
        }
    }

    /// Literal, case-sensitive search of projected text. No I/O; bounded result count.
    pub fn search(
        &self,
        query: &str,
        max: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<SearchHit>, ViewError> {
        if query.is_empty() || query.len() > 256 || max > 1024 {
            return Err(ViewError::InvalidQuery);
        }
        let mut hits = Vec::new();
        if max == 0 {
            return Ok(hits);
        }
        for (id, block) in self.blocks.iter().enumerate() {
            cancel.check()?;
            for (start, _) in block.text.match_indices(query) {
                let mut range = start..start + query.len();
                // Expand matches to whole graphemes, including combining-mark-only queries.
                for (offset, g) in block.text.grapheme_indices(true) {
                    if offset <= range.start && offset + g.len() > range.start {
                        range.start = offset;
                    }
                    if offset < range.end && offset + g.len() >= range.end {
                        range.end = offset + g.len();
                        break;
                    }
                }
                hits.push(SearchHit {
                    block: id,
                    source: block.location(range.clone()),
                    text: range,
                    owner: self.owner.clone(),
                });
                if hits.len() == max {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }
}

impl Block {
    pub(super) fn location(&self, text: Range<usize>) -> SourceLocation {
        let first_index = self.spans.partition_point(|s| s.text.end <= text.start);
        let mut matches = self.spans[first_index..]
            .iter()
            .take_while(|s| s.text.start < text.end);
        let Some(first) = matches.next() else {
            return SourceLocation {
                bytes: self.source.clone(),
                exact: false,
            };
        };
        if text.start >= first.text.start && text.end <= first.text.end && first.source.exact {
            return SourceLocation {
                bytes: first.source.bytes.start + text.start - first.text.start
                    ..first.source.bytes.start + text.end - first.text.start,
                exact: true,
            };
        }
        let mut end = first.source.bytes.end;
        for span in matches {
            end = end.max(span.source.bytes.end);
        }
        SourceLocation {
            bytes: first.source.bytes.start..end,
            exact: false,
        }
    }
}

struct Builder<'a> {
    projection: Projection,
    limits: ViewLimits,
    marked_items: BTreeSet<NodeId>,
    ordinals: BTreeMap<NodeId, Option<u64>>,
    cancel: &'a Cancellation,
}
impl Builder<'_> {
    fn block(&self, id: usize, role: BlockRole) -> Block {
        let mut quote_depth = 0;
        let mut list_depth = 0;
        let mut parent = self.projection.document.content.nodes[id].parent;
        while let Some(p) = parent {
            match self.projection.document.content.nodes[p].kind {
                NodeKind::BlockQuote => quote_depth += 1,
                NodeKind::List { .. } => list_depth += 1,
                _ => {}
            }
            parent = self.projection.document.content.nodes[p].parent;
        }
        Block {
            node: id,
            role,
            quote_depth,
            list_depth,
            source: self.projection.document.content.nodes[id].span.clone(),
            text: String::new(),
            spans: Vec::new(),
            gap_after: true,
            columns: 0,
        }
    }
    fn push(
        &self,
        block: &mut Block,
        text: &str,
        source: SourceLocation,
        style: Style,
        resource: Option<usize>,
    ) -> Result<(), ViewError> {
        self.cancel.check()?;
        if block.text.len().saturating_add(text.len()) > self.limits.block_bytes {
            return Err(ViewError::Budget(ViewResource::BlockBytes));
        }
        if self.projection.runs + block.spans.len() >= self.limits.runs {
            return Err(ViewError::Budget(ViewResource::Runs));
        }
        let start = block.text.len();
        // Neutralize controls at the content-to-view boundary. Never emit active ANSI/HTML.
        for c in text.chars() {
            block
                .text
                .push(if c.is_control() && !matches!(c, '\n' | '\t') {
                    '\u{fffd}'
                } else {
                    c
                });
        }
        if block.text.len() > self.limits.block_bytes {
            return Err(ViewError::Budget(ViewResource::BlockBytes));
        }
        block.spans.push(Span {
            text: start..block.text.len(),
            source: SourceLocation {
                exact: source.exact && &block.text[start..] == text,
                ..source
            },
            style,
            resource,
        });
        Ok(())
    }
    fn finish(&mut self, mut block: Block) -> Result<(), ViewError> {
        if self.projection.blocks.len() >= self.limits.blocks {
            return Err(ViewError::Budget(ViewResource::Blocks));
        }
        if self.projection.bytes.saturating_add(block.text.len()) > self.limits.text_bytes {
            return Err(ViewError::Budget(ViewResource::TextBytes));
        }
        block.columns = block.text.width();
        self.projection.bytes += block.text.len();
        self.projection.runs += block.spans.len();
        self.projection.node_blocks[block.node].get_or_insert(self.projection.blocks.len());
        for span in &block.spans {
            if let Some(link) = span.style.link {
                self.projection.node_blocks[link].get_or_insert(self.projection.blocks.len());
            }
            if let Some(resource) = span.resource {
                self.projection.node_blocks[resource].get_or_insert(self.projection.blocks.len());
            }
        }
        self.projection.blocks.push(block);
        Ok(())
    }
    fn style(&self, id: usize) -> Style {
        let mut style = Style::default();
        let mut p = Some(id);
        while let Some(id) = p {
            match self.projection.document.content.nodes[id].kind {
                NodeKind::Strong => style.strong = true,
                NodeKind::Emphasis => style.emphasis = true,
                NodeKind::Strikethrough => style.strike = true,
                NodeKind::Code(_) => style.code = true,
                NodeKind::Link { .. } => style.link = Some(id),
                _ => {}
            }
            p = self.projection.document.content.nodes[id].parent;
        }
        style
    }
    fn prefix(&mut self, block: &mut Block) -> Result<(), ViewError> {
        let mut item = Some(block.node);
        while let Some(id) = item {
            if matches!(
                self.projection.document.content.nodes[id].kind,
                NodeKind::Item
            ) {
                if self.marked_items.insert(id) {
                    let prefix = match self.ordinals.get(&id).copied().flatten() {
                        Some(n) => format!("{n}. "),
                        None => "• ".into(),
                    };
                    self.push(
                        block,
                        &prefix,
                        SourceLocation {
                            bytes: self.projection.document.content.nodes[id].span.start
                                ..self.projection.document.content.nodes[id].span.start,
                            exact: false,
                        },
                        Style::default(),
                        None,
                    )?;
                }
                break;
            }
            item = self.projection.document.content.nodes[id].parent;
        }
        Ok(())
    }
    fn fill(
        &mut self,
        block: &mut Block,
        range: Range<usize>,
        cell: Option<u16>,
    ) -> Result<(), ViewError> {
        let mut id = range.start;
        while id < range.end {
            self.cancel.check()?;
            let node = &self.projection.document.content.nodes[id];
            let mut style = self.style(id);
            style.cell = cell;
            let location = SourceLocation {
                bytes: node.span.clone(),
                exact: false,
            };
            match &node.kind {
                NodeKind::Text(s) | NodeKind::Code(s) | NodeKind::RawHtml(s) => {
                    let location = SourceLocation {
                        exact: self
                            .projection
                            .document
                            .content
                            .source
                            .get(node.span.clone())
                            == Some(s),
                        ..location
                    };
                    self.push(block, s, location, style, None)?;
                }
                NodeKind::SoftBreak => self.push(block, " ", location, style, None)?,
                NodeKind::HardBreak => self.push(block, "\n", location, style, None)?,
                NodeKind::TaskMarker(value) => self.push(
                    block,
                    if *value { "☑ " } else { "☐ " },
                    location,
                    style,
                    None,
                )?,
                NodeKind::Image { .. } | NodeKind::Math { .. } => {
                    let kind = if matches!(node.kind, NodeKind::Image { .. }) {
                        ResourceKind::Image
                    } else {
                        ResourceKind::Math
                    };
                    if self.projection.resources.len() >= self.limits.resources {
                        return Err(ViewError::Budget(ViewResource::Resources));
                    }
                    self.push(block, "\u{fffc}", location, style, Some(id))?;
                    self.projection.resources.insert(id, kind);
                    id = node.subtree_end;
                    continue;
                }
                _ => {}
            }
            id += 1;
        }
        Ok(())
    }
    fn inline(&mut self, id: usize, range: Range<usize>, role: BlockRole) -> Result<(), ViewError> {
        let mut block = self.block(id, role);
        self.prefix(&mut block)?;
        self.fill(&mut block, range, None)?;
        self.finish(block)
    }
    fn code(&mut self, id: usize) -> Result<(), ViewError> {
        let end = self.projection.document.content.nodes[id].subtree_end;
        let first = self.projection.blocks.len();
        for child in id + 1..end {
            if let NodeKind::Text(text) = &self.projection.document.content.nodes[child].kind {
                // Each code line is a bounded independent no-wrap layout page.
                let text = text.clone();
                let source = self.projection.document.content.nodes[child].span.clone();
                let exact = self.projection.document.content.source.get(source.clone())
                    == Some(text.as_str());
                let mut offset = 0;
                for line in text.split_inclusive('\n') {
                    let mut block = self.block(id, BlockRole::Code);
                    block.gap_after = false;
                    block.source = if exact {
                        source.start + offset..source.start + offset + line.len()
                    } else {
                        source.clone()
                    };
                    let location = SourceLocation {
                        bytes: if exact {
                            source.start + offset
                                ..source.start + offset + line.trim_end_matches(['\r', '\n']).len()
                        } else {
                            source.clone()
                        },
                        exact,
                    };
                    self.push(
                        &mut block,
                        line.trim_end_matches(['\r', '\n']),
                        location,
                        Style {
                            code: true,
                            ..Style::default()
                        },
                        None,
                    )?;
                    self.finish(block)?;
                    offset += line.len();
                }
            }
        }
        if self.projection.blocks.len() == first {
            let block = self.block(id, BlockRole::Code);
            self.finish(block)?;
        }
        if let Some(last) = self.projection.blocks.last_mut() {
            last.gap_after = true;
        }
        Ok(())
    }
    fn table(&mut self, id: usize) -> Result<(), ViewError> {
        let rows: Vec<_> = self
            .projection
            .document
            .content
            .children(id)
            .map(|(id, _)| id)
            .collect();
        let alignments = match &self.projection.document.content.nodes[id].kind {
            NodeKind::Table { alignments } => alignments.clone(),
            _ => unreachable!(),
        };
        if alignments.len() > self.limits.table_columns {
            return Err(ViewError::Budget(ViewResource::TableColumns));
        }
        let mut widths = vec![0; alignments.len()];
        let mut prepared = Vec::new();
        // Temporary prepared text is also bounded by the projection's global payload budget.
        let mut prepared_bytes = 0usize;
        for row in rows {
            self.cancel.check()?;
            let cells: Vec<_> = self
                .projection
                .document
                .content
                .children(row)
                .map(|(id, _)| id)
                .collect();
            if cells.len() > self.limits.table_columns {
                return Err(ViewError::Budget(ViewResource::TableColumns));
            }
            let mut line = Vec::new();
            for (column, cell) in cells.into_iter().enumerate() {
                let mut block = self.block(cell, BlockRole::Paragraph);
                let end = self.projection.document.content.nodes[cell].subtree_end;
                self.fill(&mut block, cell + 1..end, Some(column as u16))?;
                prepared_bytes = prepared_bytes.saturating_add(block.text.len());
                if prepared_bytes > self.limits.text_bytes {
                    return Err(ViewError::Budget(ViewResource::TextBytes));
                }
                if let Some(width) = widths.get_mut(column) {
                    *width = (*width).max(block.text.width());
                }
                line.push(block);
            }
            prepared.push((row, line));
        }
        for (row, cells) in prepared {
            let header = matches!(
                self.projection.document.content.nodes[row].kind,
                NodeKind::TableHead
            );
            let mut block = self.block(row, BlockRole::TableRow { header });
            block.gap_after = false;
            for (column, cell) in cells.into_iter().enumerate() {
                if column != 0 {
                    self.push(
                        &mut block,
                        " │ ",
                        SourceLocation {
                            bytes: cell.source.start..cell.source.start,
                            exact: false,
                        },
                        Style::default(),
                        None,
                    )?;
                }
                let pad = widths
                    .get(column)
                    .copied()
                    .unwrap_or_default()
                    .saturating_sub(cell.text.width());
                let before = match alignments.get(column) {
                    Some(Alignment::Right) => pad,
                    Some(Alignment::Center) => pad / 2,
                    _ => 0,
                };
                for (count, first) in [(before, true), (pad - before, false)] {
                    if !first {
                        for span in &cell.spans {
                            self.push(
                                &mut block,
                                &cell.text[span.text.clone()],
                                span.source.clone(),
                                span.style,
                                span.resource,
                            )?;
                        }
                    }
                    if count > self.limits.block_bytes {
                        return Err(ViewError::Budget(ViewResource::BlockBytes));
                    }
                    self.push(
                        &mut block,
                        &" ".repeat(count),
                        SourceLocation {
                            bytes: cell.source.start..cell.source.start,
                            exact: false,
                        },
                        Style::default(),
                        None,
                    )?;
                }
            }
            self.finish(block)?;
        }
        if let Some(last) = self.projection.blocks.last_mut() {
            last.gap_after = true;
        }
        Ok(())
    }
}
fn is_inline(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Text(_)
            | NodeKind::Code(_)
            | NodeKind::Emphasis
            | NodeKind::Strong
            | NodeKind::Strikethrough
            | NodeKind::Link { .. }
            | NodeKind::Image { .. }
            | NodeKind::Math { .. }
            | NodeKind::SoftBreak
            | NodeKind::HardBreak
            | NodeKind::TaskMarker(_)
            | NodeKind::RawHtml(_)
    )
}
fn slug(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}
