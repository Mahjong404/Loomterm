use super::{model::*, Projection};
use crate::{Cancellation, NodeId};
use std::{collections::BTreeMap, mem::size_of};
use unicode_linebreak::linebreaks;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub(super) struct Atom {
    pub glyph: DrawCluster,
    pub height: u16,
    allowed: bool,
}
pub(super) struct Line {
    pub y: u32,
    pub height: u16,
    pub atoms: Vec<Atom>,
}
pub(super) struct Page {
    pub lines: Vec<Line>,
    pub rows: u32,
    pub bytes: usize,
}

pub(super) fn estimate(block: &Block, viewport: Viewport, metrics: Metrics) -> u64 {
    let indent = indent(block, viewport, metrics);
    let width = (u32::from(viewport.columns) - indent).max(1) as usize;
    let rows = if block.wraps() {
        block.columns.div_ceil(width).max(1)
    } else {
        1
    };
    rows as u64 + u64::from(block.gap_rows(metrics))
}
fn indent(block: &Block, viewport: Viewport, metrics: Metrics) -> u32 {
    (u32::from(block.quote_depth + block.list_depth) * u32::from(metrics.indent))
        .min(u32::from(viewport.columns) - 1)
}

pub(super) fn shape(
    projection: &Projection,
    id: usize,
    viewport: Viewport,
    metrics: Metrics,
    sizes: &BTreeMap<NodeId, LogicalSize>,
    cancel: &Cancellation,
) -> Result<Page, ViewError> {
    let block = &projection.blocks[id];
    let left = indent(block, viewport, metrics);
    let mut lines = Vec::new();
    let mut atoms: Vec<Atom> = Vec::new();
    let mut x = left;
    let mut y = 0;
    let mut span_index = 0;
    let mut breaks = linebreaks(&block.text).peekable();
    for (n, (start, grapheme)) in block.text.grapheme_indices(true).enumerate() {
        if n % 128 == 0 {
            cancel.check()?;
        }
        let end = start + grapheme.len();
        let mut allowed = false;
        while breaks.peek().is_some_and(|(position, _)| *position <= end) {
            let (position, _) = breaks.next().unwrap();
            allowed |= position == end;
        }
        if grapheme == "\n" {
            finish_line(&mut lines, &mut atoms, &mut y);
            x = left;
            continue;
        }
        while block
            .spans
            .get(span_index)
            .is_some_and(|span| span.text.end <= start)
        {
            span_index += 1;
        }
        let span = block.spans.get(span_index);
        let resource = span.and_then(|s| s.resource);
        let size = resource.map(|node| {
            sizes
                .get(&node)
                .copied()
                .unwrap_or_else(|| match projection.resources[&node] {
                    ResourceKind::Image => metrics.image,
                    ResourceKind::Math => metrics.math,
                })
        });
        let mut width = size.map_or_else(
            || grapheme_width(grapheme, x, metrics),
            |s| u32::from(s.columns),
        );
        if block.wraps() && x + width > u32::from(viewport.columns) && !atoms.is_empty() {
            let split = atoms
                .iter()
                .rposition(|atom| atom.allowed)
                .map(|p| p + 1)
                .unwrap_or(atoms.len());
            let tail = atoms.split_off(split);
            finish_line(&mut lines, &mut atoms, &mut y);
            atoms = tail;
            x = rebase(&mut atoms, left, block, metrics);
            width = size.map_or_else(
                || grapheme_width(grapheme, x, metrics),
                |s| u32::from(s.columns),
            );
            if x + width > u32::from(viewport.columns) && !atoms.is_empty() {
                finish_line(&mut lines, &mut atoms, &mut y);
                x = left;
                width = size.map_or_else(
                    || grapheme_width(grapheme, x, metrics),
                    |s| u32::from(s.columns),
                );
            }
        }
        atoms.push(Atom {
            glyph: DrawCluster {
                block: id,
                text: start..end,
                x: x as i32,
                width,
                source: block.location(start..end),
                style: span.map_or(Style::default(), |s| s.style),
                resource,
            },
            height: size.map_or(1, |s| s.rows),
            allowed,
        });
        x += width;
    }
    if !atoms.is_empty() || lines.is_empty() {
        finish_line(&mut lines, &mut atoms, &mut y);
    }
    cancel.check()?;
    let bytes = lines.capacity() * size_of::<Line>()
        + lines
            .iter()
            .map(|l| l.atoms.capacity() * size_of::<Atom>())
            .sum::<usize>();
    Ok(Page {
        lines,
        rows: y + block.gap_rows(metrics),
        bytes,
    })
}
fn grapheme_width(text: &str, x: u32, metrics: Metrics) -> u32 {
    if text == "\t" {
        let stop = u32::from(metrics.tab_stop);
        stop - x % stop
    } else if metrics.cjk_ambiguous {
        text.width_cjk() as u32
    } else {
        text.width() as u32
    }
}
fn rebase(atoms: &mut [Atom], left: u32, block: &Block, metrics: Metrics) -> u32 {
    let mut x = left;
    for atom in atoms {
        atom.glyph.x = x as i32;
        if atom.glyph.resource.is_none() {
            atom.glyph.width = grapheme_width(&block.text[atom.glyph.text.clone()], x, metrics);
        }
        x += atom.glyph.width;
    }
    x
}
fn finish_line(lines: &mut Vec<Line>, atoms: &mut Vec<Atom>, y: &mut u32) {
    let height = atoms.iter().map(|a| a.height).max().unwrap_or(1);
    lines.push(Line {
        y: *y,
        height,
        atoms: std::mem::take(atoms),
    });
    *y += u32::from(height);
}

/// Mixed exact/estimated block-height index. Lookup, update and prefix sum are O(log n).
pub(super) struct Heights {
    values: Vec<u64>,
    tree: Vec<u64>,
    pub measured: Vec<bool>,
    pub count: usize,
}
impl Heights {
    pub fn new(projection: &Projection, viewport: Viewport, metrics: Metrics) -> Self {
        let values: Vec<_> = projection
            .blocks
            .iter()
            .map(|block| estimate(block, viewport, metrics))
            .collect();
        let mut tree = vec![0; values.len() + 1];
        for (i, value) in values.iter().enumerate() {
            let id = i + 1;
            tree[id] += value;
            let parent = id + (id & id.wrapping_neg());
            if parent < tree.len() {
                tree[parent] += tree[id];
            }
        }
        Self {
            measured: vec![false; values.len()],
            values,
            tree,
            count: 0,
        }
    }
    pub fn set(&mut self, id: usize, value: u64) {
        let old = self.values[id];
        self.values[id] = value;
        let mut index = id + 1;
        while index < self.tree.len() {
            self.tree[index] = self.tree[index] - old + value;
            index += index & index.wrapping_neg();
        }
        if !self.measured[id] {
            self.measured[id] = true;
            self.count += 1;
        }
    }
    pub fn invalidate(&mut self, id: usize, value: u64) {
        self.set(id, value);
        self.measured[id] = false;
        self.count -= 1;
    }
    pub fn prefix(&self, mut end: usize) -> u64 {
        let mut sum = 0;
        while end > 0 {
            sum += self.tree[end];
            end &= end - 1;
        }
        sum
    }
    pub fn total(&self) -> u64 {
        self.prefix(self.values.len())
    }
    pub fn locate(&self, row: u64) -> (usize, u32) {
        if self.values.is_empty() {
            return (0, 0);
        }
        let mut index = 0;
        let mut sum = 0;
        let mut bit = 1;
        while bit < self.tree.len() {
            bit <<= 1;
        }
        while bit > 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= row {
                index = next;
                sum += self.tree[next];
            }
            bit >>= 1;
        }
        let index = index.min(self.values.len() - 1);
        (
            index,
            row.saturating_sub(sum).min(self.values[index] - 1) as u32,
        )
    }
}
