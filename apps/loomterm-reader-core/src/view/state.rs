use super::{
    layout::{self, Heights, Page},
    model::*,
    Projection,
};
use crate::{Cancellation, ImageReference, NodeId, NodeKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::Arc,
};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

struct Cached {
    page: Arc<Page>,
    used: u64,
}
#[derive(Clone, Copy, Default)]
struct Position {
    block: usize,
    row: u32,
}
#[derive(Clone, Copy)]
struct DisplayAnchor {
    block: usize,
    byte: usize,
    within: u32,
}

pub(super) struct ViewState {
    pub projection: Arc<Projection>,
    viewport: Viewport,
    metrics: Metrics,
    limits: ViewLimits,
    position: Position,
    pending_anchor: Option<DisplayAnchor>,
    horizontal: u32,
    horizontal_extent: u32,
    heights: Heights,
    cache: BTreeMap<usize, Cached>,
    cache_bytes: usize,
    clock: u64,
    sizes: BTreeMap<NodeId, LogicalSize>,
    builds: u64,
    hits: u64,
    pub focused_link: Option<usize>,
    selection: Option<Range<usize>>,
    failures: BTreeMap<NodeId, ResourceFailure>,
    epoch: Arc<()>,
    approximate: bool,
}
impl ViewState {
    pub fn new(
        projection: Arc<Projection>,
        viewport: Viewport,
        metrics: Metrics,
        limits: ViewLimits,
    ) -> Self {
        let heights = Heights::new(&projection, viewport, metrics);
        Self {
            projection,
            viewport,
            metrics,
            limits,
            position: Position::default(),
            pending_anchor: None,
            horizontal: 0,
            horizontal_extent: 0,
            heights,
            cache: BTreeMap::new(),
            cache_bytes: 0,
            clock: 0,
            sizes: BTreeMap::new(),
            builds: 0,
            hits: 0,
            focused_link: None,
            selection: None,
            failures: BTreeMap::new(),
            epoch: Arc::new(()),
            approximate: false,
        }
    }
    fn page(&mut self, block: usize, cancel: &Cancellation) -> Result<Arc<Page>, ViewError> {
        cancel.check()?;
        self.clock = self.clock.saturating_add(1);
        if let Some(cached) = self.cache.get_mut(&block) {
            self.hits += 1;
            cached.used = self.clock;
            return Ok(cached.page.clone());
        }
        let page = Arc::new(layout::shape(
            &self.projection,
            block,
            self.viewport,
            self.metrics,
            &self.sizes,
            cancel,
        )?);
        self.heights.set(block, u64::from(page.rows));
        self.builds += 1;
        if page.bytes <= self.limits.cache_bytes && self.limits.cache_blocks > 0 {
            while !self.cache.is_empty()
                && (self.cache.len() >= self.limits.cache_blocks
                    || self.cache_bytes + page.bytes > self.limits.cache_bytes)
            {
                let oldest = *self.cache.iter().min_by_key(|(_, c)| c.used).unwrap().0;
                self.cache_bytes -= self.cache.remove(&oldest).unwrap().page.bytes;
            }
            self.cache_bytes += page.bytes;
            self.cache.insert(
                block,
                Cached {
                    page: page.clone(),
                    used: self.clock,
                },
            );
        }
        Ok(page)
    }
    pub fn budget(&self) -> BudgetState {
        BudgetState {
            blocks: self.projection.blocks.len(),
            projected_bytes: self.projection.bytes,
            runs: self.projection.runs,
            cache_bytes: self.cache_bytes,
            cache_blocks: self.cache.len(),
            measured_blocks: self.heights.count,
            layout_builds: self.builds,
            cache_hits: self.hits,
        }
    }
    fn display_anchor(&self) -> DisplayAnchor {
        if let Some(anchor) = self.pending_anchor {
            return anchor;
        }
        if let Some(cached) = self.cache.get(&self.position.block) {
            let line = cached
                .page
                .lines
                .iter()
                .find(|line| line.y + u32::from(line.height) > self.position.row);
            if let Some(line) = line {
                return DisplayAnchor {
                    block: self.position.block,
                    byte: line.atoms.first().map_or(0, |a| a.glyph.text.start),
                    within: self.position.row.saturating_sub(line.y),
                };
            }
        }
        DisplayAnchor {
            block: self.position.block,
            byte: 0,
            within: 0,
        }
    }
    pub fn anchor(&self) -> SourceAnchor {
        let anchor = self.display_anchor();
        let Some(block) = self.projection.blocks.get(anchor.block) else {
            return SourceAnchor {
                byte: 0,
                precision: Precision::Approximate,
            };
        };
        if let Some(grapheme) = block.text[anchor.byte..].graphemes(true).next() {
            let source = block.location(anchor.byte..anchor.byte + grapheme.len());
            SourceAnchor {
                byte: source.bytes.start,
                precision: if source.exact && !self.approximate {
                    Precision::Exact
                } else {
                    Precision::Approximate
                },
            }
        } else {
            SourceAnchor {
                byte: block.source.start,
                precision: Precision::Approximate,
            }
        }
    }
    pub fn restore_source(&mut self, byte: usize, precision: Precision) {
        self.approximate = precision == Precision::Approximate;
        let block = self
            .projection
            .blocks
            .partition_point(|b| b.source.start <= byte)
            .saturating_sub(1);
        let offset = self
            .projection
            .blocks
            .get(block)
            .and_then(|b| b.spans.iter().find(|s| s.source.bytes.end > byte))
            .map_or(0, |s| {
                if s.source.exact {
                    s.text.start + byte.saturating_sub(s.source.bytes.start).min(s.text.len())
                } else {
                    s.text.start
                }
            });
        // A source edit can land inside a grapheme: round down in projected text.
        let offset = self.projection.blocks.get(block).map_or(0, |b| {
            b.text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .take_while(|i| *i <= offset)
                .last()
                .unwrap_or(0)
        });
        self.position = Position { block, row: 0 };
        self.pending_anchor = Some(DisplayAnchor {
            block,
            byte: offset,
            within: 0,
        });
    }
    pub fn resize(&mut self, viewport: Viewport, metrics: Metrics) {
        self.invalidate_frame();
        let anchor = self.display_anchor();
        self.viewport = viewport;
        self.metrics = metrics;
        self.heights = Heights::new(&self.projection, viewport, metrics);
        self.cache.clear();
        self.cache_bytes = 0;
        self.pending_anchor = Some(anchor);
    }
    fn settle(&mut self, cancel: &Cancellation) -> Result<(), ViewError> {
        if self.projection.blocks.is_empty() {
            return Ok(());
        }
        let page = self.page(self.position.block, cancel)?;
        if let Some(anchor) = self.pending_anchor.take() {
            let line = page.lines.iter().rfind(|line| {
                line.atoms
                    .first()
                    .is_some_and(|a| a.glyph.text.start <= anchor.byte)
            });
            self.position.row = line.map_or(0, |line| {
                line.y + anchor.within.min(u32::from(line.height) - 1)
            });
        }
        self.position.row = self.position.row.min(page.rows.saturating_sub(1));
        Ok(())
    }
    pub fn frame(&mut self, cancel: &Cancellation) -> Result<Frame, ViewError> {
        cancel.check()?;
        let before = self.heights.count;
        let mut work = 0;
        let mut work_bytes = 0;
        if !self.projection.blocks.is_empty() {
            let missing = !self.cache.contains_key(&self.position.block);
            self.settle(cancel)?;
            if missing {
                work += 1;
                work_bytes += self.projection.blocks[self.position.block].text.len();
            }
        }
        let mut lines = Vec::new();
        let mut resources = BTreeSet::new();
        let mut clusters = 0;
        let mut horizontal_extent = 0u32;
        let mut block = self.position.block;
        let mut y = -(self.position.row as i32);
        let mut pending = false;
        while block < self.projection.blocks.len() && y < i32::from(self.viewport.rows) {
            cancel.check()?;
            if !self.cache.contains_key(&block) {
                let bytes = self.projection.blocks[block].text.len();
                if work >= self.limits.work_blocks || work_bytes + bytes > self.limits.work_bytes {
                    pending = true;
                    break;
                }
                work += 1;
                work_bytes += bytes;
            }
            let page = self.page(block, cancel)?;
            let semantic = &self.projection.blocks[block];
            for line in &page.lines {
                let top = y + line.y as i32;
                if top + i32::from(line.height) <= 0 || top >= i32::from(self.viewport.rows) {
                    continue;
                }
                let overflow = if semantic.wraps() {
                    0
                } else {
                    line.atoms.last().map_or(0, |a| {
                        (a.glyph.x + a.glyph.width as i32 - i32::from(self.viewport.columns)).max(0)
                            as u32
                    })
                };
                horizontal_extent = horizontal_extent.max(overflow);
                let shift = self.horizontal.min(overflow) as i32;
                let mut visible = Vec::new();
                for atom in &line.atoms {
                    let mut glyph = atom.glyph.clone();
                    glyph.x -= shift;
                    if glyph.x + glyph.width as i32 <= 0
                        || glyph.x >= i32::from(self.viewport.columns)
                    {
                        continue;
                    }
                    clusters += 1;
                    if clusters > self.limits.frame_clusters {
                        return Err(ViewError::Budget(ViewResource::FrameClusters));
                    }
                    if let Some(id) = glyph.resource {
                        resources.insert(id);
                    }
                    visible.push(glyph);
                }
                lines.push(DrawLine {
                    block,
                    role: semantic.role,
                    quote_depth: semantic.quote_depth,
                    list_depth: semantic.list_depth,
                    y: top,
                    height: line.height,
                    clusters: visible,
                    horizontal_overflow: line.atoms.last().is_some_and(|a| {
                        a.glyph.x + a.glyph.width as i32 > i32::from(self.viewport.columns)
                    }),
                });
            }
            y += page.rows as i32;
            block += 1;
        }
        // An undersized cache must fail explicitly instead of returning Pending forever.
        if pending && self.heights.count == before {
            return Err(ViewError::Budget(ViewResource::CacheBytes));
        }
        self.horizontal_extent = horizontal_extent;
        self.horizontal = self.horizontal.min(horizontal_extent);
        let resources = resources
            .into_iter()
            .filter(|id| !self.sizes.contains_key(id) && !self.failures.contains_key(id))
            .filter_map(|id| {
                if matches!(
                    &self.projection.document.content.nodes[id].kind,
                    NodeKind::Image {
                        reference: ImageReference::Blocked { .. },
                        ..
                    }
                ) {
                    return None;
                }
                Some(ResourceRequest {
                    ticket: ResourceTicket {
                        node: id,
                        owner: self.projection.owner.clone(),
                    },
                    kind: self.projection.resources[&id],
                })
            })
            .collect();
        self.invalidate_frame();
        Ok(Frame {
            projection: self.projection.clone(),
            viewport: self.viewport,
            lines,
            resources,
            anchor: self.anchor(),
            epoch: self.epoch.clone(),
            estimated_rows: self.heights.total(),
            extent_exact: self.heights.count == self.projection.blocks.len(),
            at_top: self.position.block == 0 && self.position.row == 0,
            at_bottom: !pending
                && block == self.projection.blocks.len()
                && y <= i32::from(self.viewport.rows),
            pending,
            budget: self.budget(),
            selection: self.selection.clone(),
            focused_link: self.focused_link.map(|index| self.projection.links[index]),
        })
    }
    pub fn resource_size(
        &mut self,
        ticket: &ResourceTicket,
        size: LogicalSize,
    ) -> Result<bool, ViewError> {
        validate_size(size)?;
        if !Arc::ptr_eq(&ticket.owner, &self.projection.owner) {
            return Ok(false);
        }
        if !self.projection.resources.contains_key(&ticket.node) {
            return Err(ViewError::UnknownResource);
        }
        if self.sizes.get(&ticket.node) == Some(&size) {
            return Ok(true);
        }
        let anchor = self.display_anchor();
        self.sizes.insert(ticket.node, size);
        self.failures.remove(&ticket.node);
        self.invalidate_frame();
        if let Some(block) = self.projection.node_blocks[ticket.node] {
            if let Some(cached) = self.cache.remove(&block) {
                self.cache_bytes -= cached.page.bytes;
            }
            self.heights.invalidate(
                block,
                layout::estimate(&self.projection.blocks[block], self.viewport, self.metrics),
            );
            if self.position.block == block {
                self.pending_anchor = Some(anchor);
            }
        }
        Ok(true)
    }
    pub fn resource_failed(
        &mut self,
        ticket: &ResourceTicket,
        failure: ResourceFailure,
    ) -> Result<bool, ViewError> {
        if !Arc::ptr_eq(&ticket.owner, &self.projection.owner) {
            return Ok(false);
        }
        if !self.projection.resources.contains_key(&ticket.node) {
            return Err(ViewError::UnknownResource);
        }
        let anchor = self.display_anchor();
        if self.sizes.remove(&ticket.node).is_some() {
            if let Some(block) = self.projection.node_blocks[ticket.node] {
                if let Some(cached) = self.cache.remove(&block) {
                    self.cache_bytes -= cached.page.bytes;
                }
                self.heights.invalidate(
                    block,
                    layout::estimate(&self.projection.blocks[block], self.viewport, self.metrics),
                );
                if block == self.position.block {
                    self.pending_anchor = Some(anchor);
                }
            }
        }
        self.failures.insert(ticket.node, failure);
        self.invalidate_frame();
        Ok(true)
    }
    pub fn resource_status(&self, node: NodeId) -> Option<ResourceStatus> {
        if !self.projection.resources.contains_key(&node) {
            return None;
        }
        if matches!(
            &self.projection.document.content.nodes[node].kind,
            NodeKind::Image {
                reference: ImageReference::Blocked { .. },
                ..
            }
        ) {
            return Some(ResourceStatus::Failed(ResourceFailure::Unsupported));
        }
        Some(if let Some(size) = self.sizes.get(&node) {
            ResourceStatus::Ready(*size)
        } else if let Some(failure) = self.failures.get(&node) {
            ResourceStatus::Failed(*failure)
        } else {
            ResourceStatus::Pending
        })
    }
    pub fn invalidate_frame(&mut self) {
        self.epoch = Arc::new(());
    }
    pub fn is_current_frame(&self, frame: &Frame) -> bool {
        Arc::ptr_eq(&self.epoch, &frame.epoch)
    }
    pub fn scroll(&mut self, rows: i64, cancel: &Cancellation) -> Result<(), ViewError> {
        if self.projection.blocks.is_empty() {
            return Ok(());
        }
        self.settle(cancel)?;
        if rows.unsigned_abs() > u64::from(self.viewport.rows) * 4 {
            self.approximate = true;
            let total = self.heights.total();
            let current = self.heights.prefix(self.position.block) + u64::from(self.position.row);
            let target = current
                .saturating_add_signed(rows)
                .min(total.saturating_sub(1));
            let (block, row) = self.heights.locate(target);
            self.position = Position { block, row };
            return Ok(());
        }
        let mut remaining = rows;
        while remaining != 0 {
            let page = self.page(self.position.block, cancel)?;
            if remaining > 0 {
                let advance = remaining.min(i64::from(page.rows - 1 - self.position.row));
                self.position.row += advance as u32;
                remaining -= advance;
                if remaining > 0 {
                    if self.position.block + 1 == self.projection.blocks.len() {
                        break;
                    }
                    self.position.block += 1;
                    self.position.row = 0;
                    remaining -= 1;
                }
            } else {
                let advance = (-remaining).min(i64::from(self.position.row));
                self.position.row -= advance as u32;
                remaining += advance;
                if remaining < 0 {
                    if self.position.block == 0 {
                        break;
                    }
                    self.position.block -= 1;
                    self.position.row = self.page(self.position.block, cancel)?.rows - 1;
                    remaining += 1;
                }
            }
        }
        Ok(())
    }
    pub fn input(&mut self, input: Input, cancel: &Cancellation) -> Result<Action, ViewError> {
        cancel.check()?;
        self.invalidate_frame();
        match input {
            Input::ScrollRows(n) => self.scroll(i64::from(n), cancel)?,
            Input::Wheel(n) => self.scroll(-i64::from(n) * 3, cancel)?,
            Input::Page(n) => self.scroll(
                i64::from(n) * i64::from(self.viewport.rows.saturating_sub(1).max(1)),
                cancel,
            )?,
            Input::Home => {
                self.position = Position::default();
                self.pending_anchor = None;
                self.approximate = false;
            }
            Input::End => {
                self.approximate = false;
                if !self.projection.blocks.is_empty() {
                    let block = self.projection.blocks.len() - 1;
                    self.position = Position {
                        block,
                        row: self.page(block, cancel)?.rows - 1,
                    };
                    self.pending_anchor = None;
                    self.scroll(-i64::from(self.viewport.rows - 1), cancel)?;
                }
            }
            Input::Horizontal(n) => {
                self.horizontal = self
                    .horizontal
                    .saturating_add_signed(n)
                    .min(self.horizontal_extent);
            }
            Input::Heading(index) => {
                let heading = self
                    .projection
                    .headings
                    .get(index)
                    .ok_or(ViewError::InvalidPosition)?;
                self.position = Position {
                    block: heading.block,
                    row: 0,
                };
                self.pending_anchor = None;
                self.approximate = false;
            }
            Input::FocusLink { backwards } => {
                let count = self.projection.links.len();
                if count > 0 {
                    let index = match self.focused_link {
                        None => {
                            if backwards {
                                count - 1
                            } else {
                                0
                            }
                        }
                        Some(i) => {
                            if backwards {
                                (i + count - 1) % count
                            } else {
                                (i + 1) % count
                            }
                        }
                    };
                    self.focused_link = Some(index);
                    if let Some(block) = self.projection.node_blocks[self.projection.links[index]] {
                        let byte = self.projection.blocks[block]
                            .spans
                            .iter()
                            .find(|s| s.style.link == Some(self.projection.links[index]))
                            .map_or(0, |s| s.text.start);
                        self.position = Position { block, row: 0 };
                        self.pending_anchor = Some(DisplayAnchor {
                            block,
                            byte,
                            within: 0,
                        });
                        self.approximate = false;
                    }
                }
            }
            Input::ActivateLink => {
                if let Some(index) = self.focused_link {
                    let target = self.projection.link_target(self.projection.links[index]);
                    if let LinkTarget::Fragment(fragment) = &target {
                        if let Some(heading) = self
                            .projection
                            .headings
                            .iter()
                            .find(|h| h.slug == *fragment)
                        {
                            self.position = Position {
                                block: heading.block,
                                row: 0,
                            };
                            self.pending_anchor = None;
                            self.approximate = false;
                        }
                    }
                    return Ok(Action::Link(target));
                }
            }
            Input::Select { start, end } => {
                let source = &self.projection.document.content.source;
                let valid = |offset| {
                    offset == source.len()
                        || (source.is_char_boundary(offset)
                            && GraphemeCursor::new(offset, source.len(), true)
                                .is_boundary(source, 0)
                                == Ok(true))
                };
                if start > source.len() || end > source.len() || !valid(start) || !valid(end) {
                    return Err(ViewError::InvalidPosition);
                }
                self.selection = Some(start.min(end)..start.max(end));
            }
            Input::ClearSelection => self.selection = None,
        }
        Ok(Action::None)
    }
    pub fn selection(&self) -> Option<&str> {
        self.selection
            .as_ref()
            .map(|range| &self.projection.document.content.source[range.clone()])
    }
    pub fn locate_search(
        &mut self,
        hit: &SearchHit,
        cancel: &Cancellation,
    ) -> Result<(), ViewError> {
        if !Arc::ptr_eq(&hit.owner, &self.projection.owner) {
            return Err(ViewError::InvalidPosition);
        }
        self.invalidate_frame();
        self.approximate = false;
        let block = self
            .projection
            .blocks
            .get(hit.block)
            .ok_or(ViewError::InvalidPosition)?;
        if block.text.get(hit.text.clone()).is_none() {
            return Err(ViewError::InvalidPosition);
        }
        self.position = Position {
            block: hit.block,
            row: 0,
        };
        self.pending_anchor = Some(DisplayAnchor {
            block: hit.block,
            byte: hit.text.start,
            within: 0,
        });
        self.settle(cancel)
    }
}
