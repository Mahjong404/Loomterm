use crate::{NodeId, ReaderError};
use std::{ops::Range, path::PathBuf, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    Core(ReaderError),
    InvalidViewport,
    InvalidLimits,
    Budget(ViewResource),
    InvalidPosition,
    InvalidQuery,
    UnknownResource,
    NoDocument,
}
impl From<ReaderError> for ViewError {
    fn from(error: ReaderError) -> Self {
        Self::Core(error)
    }
}
impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ViewError {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewResource {
    Blocks,
    Runs,
    TextBytes,
    BlockBytes,
    TableColumns,
    Resources,
    FrameClusters,
    CacheBytes,
}

#[derive(Clone, Copy, Debug)]
pub struct ViewLimits {
    pub blocks: usize,
    pub runs: usize,
    pub text_bytes: usize,
    pub block_bytes: usize,
    pub table_columns: usize,
    pub resources: usize,
    pub cache_bytes: usize,
    pub cache_blocks: usize,
    pub frame_clusters: usize,
    pub work_blocks: usize,
    pub work_bytes: usize,
}
impl Default for ViewLimits {
    fn default() -> Self {
        Self {
            blocks: 100_000,
            runs: 200_000,
            text_bytes: 16 * 1024 * 1024,
            block_bytes: 16 * 1024,
            table_columns: 64,
            resources: 1024,
            cache_bytes: 8 * 1024 * 1024,
            cache_blocks: 64,
            frame_clusters: 32768,
            work_blocks: 8,
            work_bytes: 128 * 1024,
        }
    }
}
impl ViewLimits {
    pub(super) fn validate(self) -> Result<(), ViewError> {
        if self.blocks > 250_000
            || self.runs > 500_000
            || self.text_bytes > 32 * 1024 * 1024
            || self.block_bytes == 0
            || self.block_bytes > 64 * 1024
            || self.table_columns > 128
            || self.resources > 4096
            || self.cache_bytes > 64 * 1024 * 1024
            || self.cache_blocks > 512
            || self.frame_clusters == 0
            || self.frame_clusters > 262144
            || self.work_blocks == 0
            || self.work_blocks > 256
            || self.work_bytes < self.block_bytes
            || self.work_bytes > 4 * 1024 * 1024
        {
            return Err(ViewError::InvalidLimits);
        }
        Ok(())
    }
}

/// Logical text columns and rows, not pixels or a terminal grid/owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub columns: u16,
    pub rows: u16,
}
impl Viewport {
    pub fn new(columns: u16, rows: u16) -> Result<Self, ViewError> {
        if columns == 0 || columns > 1024 || rows == 0 || rows > 256 {
            Err(ViewError::InvalidViewport)
        } else {
            Ok(Self { columns, rows })
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalSize {
    pub columns: u16,
    pub rows: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub block_gap: u8,
    pub heading_gap: u8,
    pub indent: u8,
    pub tab_stop: u8,
    pub cjk_ambiguous: bool,
    pub image: LogicalSize,
    pub math: LogicalSize,
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            block_gap: 1,
            heading_gap: 2,
            indent: 2,
            tab_stop: 4,
            cjk_ambiguous: false,
            image: LogicalSize {
                columns: 8,
                rows: 3,
            },
            math: LogicalSize {
                columns: 6,
                rows: 2,
            },
        }
    }
}
impl Metrics {
    pub(super) fn validate(self) -> Result<(), ViewError> {
        if self.block_gap > 8
            || self.heading_gap > 8
            || self.indent > 8
            || self.tab_stop == 0
            || self.tab_stop > 16
        {
            return Err(ViewError::InvalidLimits);
        }
        validate_size(self.image)?;
        validate_size(self.math)
    }
}
pub(super) fn validate_size(size: LogicalSize) -> Result<(), ViewError> {
    if size.columns == 0 || size.columns > 4096 || size.rows == 0 || size.rows > 256 {
        Err(ViewError::InvalidLimits)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRole {
    Heading(u8),
    Paragraph,
    Code,
    TableRow { header: bool },
    Rule,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<NodeId>,
    pub cell: Option<u16>,
}
/// Theme-neutral paint roles. The Window host chooses fonts, colors and DPI sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceRole {
    Plain,
    Quote,
    CodeBlock,
    InlineCode,
    TableHeader,
    TableBody,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintStyle {
    pub heading_level: Option<u8>,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub monospace: bool,
    pub underline: bool,
    pub focused_link: bool,
    pub selected: bool,
    pub surface: SurfaceRole,
    pub table_cell: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub bytes: Range<usize>,
    pub exact: bool,
}
#[derive(Clone, Debug)]
pub struct Span {
    pub text: Range<usize>,
    pub source: SourceLocation,
    pub style: Style,
    pub resource: Option<NodeId>,
}
#[derive(Debug)]
pub struct Block {
    pub node: NodeId,
    pub role: BlockRole,
    pub quote_depth: u16,
    pub list_depth: u16,
    pub source: Range<usize>,
    pub text: String,
    pub spans: Vec<Span>,
    pub gap_after: bool,
    pub(super) columns: usize,
}
impl Block {
    pub fn wraps(&self) -> bool {
        !matches!(self.role, BlockRole::Code | BlockRole::TableRow { .. })
    }
    pub(super) fn gap_rows(&self, metrics: Metrics) -> u32 {
        if !self.gap_after {
            return 0;
        }
        match self.role {
            BlockRole::Heading(_) => u32::from(metrics.heading_gap),
            _ => u32::from(metrics.block_gap),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Heading {
    pub node: NodeId,
    pub block: usize,
    pub level: u8,
    pub slug: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Image,
    Math,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceFailure {
    Missing,
    Decode,
    Budget,
    Unsupported,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceStatus {
    Pending,
    Ready(LogicalSize),
    Failed(ResourceFailure),
}
#[derive(Clone, Debug)]
pub struct ResourceTicket {
    pub node: NodeId,
    pub(super) owner: Arc<()>,
}
#[derive(Clone, Debug)]
pub struct ResourceRequest {
    pub ticket: ResourceTicket,
    pub kind: ResourceKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Fragment(String),
    LocalFile {
        path: PathBuf,
        fragment: Option<String>,
    },
    External(String),
    Blocked,
}

#[derive(Clone, Debug)]
pub struct DrawCluster {
    pub block: usize,
    pub text: Range<usize>,
    pub x: i32,
    pub width: u32,
    pub source: SourceLocation,
    pub style: Style,
    pub resource: Option<NodeId>,
}
#[derive(Clone, Debug)]
pub struct DrawLine {
    pub block: usize,
    pub role: BlockRole,
    pub quote_depth: u16,
    pub list_depth: u16,
    pub y: i32,
    pub height: u16,
    pub clusters: Vec<DrawCluster>,
    pub horizontal_overflow: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    Exact,
    Approximate,
}
#[derive(Clone, Debug)]
pub struct SourceAnchor {
    pub byte: usize,
    pub precision: Precision,
}
/// A pointer-derived source boundary. Inexact locations come from synthesized
/// display text; callers may use them for approximate navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePoint {
    pub byte: usize,
    pub exact: bool,
}
#[derive(Clone, Debug, Default)]
pub struct BudgetState {
    pub blocks: usize,
    pub projected_bytes: usize,
    pub runs: usize,
    pub cache_bytes: usize,
    pub cache_blocks: usize,
    pub measured_blocks: usize,
    pub layout_builds: u64,
    pub cache_hits: u64,
}
pub struct Frame {
    pub projection: Arc<super::Projection>,
    pub viewport: Viewport,
    pub lines: Vec<DrawLine>,
    pub resources: Vec<ResourceRequest>,
    pub anchor: SourceAnchor,
    pub estimated_rows: u64,
    pub extent_exact: bool,
    pub at_top: bool,
    pub at_bottom: bool,
    pub pending: bool,
    pub budget: BudgetState,
    pub selection: Option<Range<usize>>,
    pub focused_link: Option<NodeId>,
    pub(super) epoch: Arc<()>,
}
impl Frame {
    pub fn text<'a>(&'a self, cluster: &DrawCluster) -> &'a str {
        &self.projection.blocks[cluster.block].text[cluster.text.clone()]
    }
    /// Resolve the visible semantic treatment without choosing a host renderer.
    /// Code-block surfaces apply to the whole DrawLine, including empty lines.
    pub fn paint_style(&self, line: &DrawLine, cluster: &DrawCluster) -> PaintStyle {
        let heading_level = match line.role {
            BlockRole::Heading(level) => Some(level),
            _ => None,
        };
        let surface = match line.role {
            BlockRole::Code => SurfaceRole::CodeBlock,
            BlockRole::TableRow { header: true } => SurfaceRole::TableHeader,
            BlockRole::TableRow { header: false } => SurfaceRole::TableBody,
            _ if cluster.style.code => SurfaceRole::InlineCode,
            _ if line.quote_depth > 0 => SurfaceRole::Quote,
            _ => SurfaceRole::Plain,
        };
        PaintStyle {
            heading_level,
            bold: heading_level.is_some()
                || cluster.style.strong
                || matches!(line.role, BlockRole::TableRow { header: true }),
            italic: cluster.style.emphasis,
            strike: cluster.style.strike,
            monospace: cluster.style.code || matches!(line.role, BlockRole::Code),
            underline: cluster.style.link.is_some(),
            focused_link: cluster.style.link.is_some() && cluster.style.link == self.focused_link,
            selected: self.is_selected(cluster),
            surface,
            table_cell: cluster.style.cell,
        }
    }
    pub fn hit_test(&self, column: u16, row: u16) -> Option<&DrawCluster> {
        if column >= self.viewport.columns || row >= self.viewport.rows {
            return None;
        }
        self.lines
            .iter()
            .find(|l| l.y <= i32::from(row) && l.y + i32::from(l.height) > i32::from(row))?
            .clusters
            .iter()
            .find(|c| c.x <= i32::from(column) && c.x + c.width as i32 > i32::from(column))
    }

    /// Maps a pointer cell to the nearest visible source boundary for drag
    /// selection. Blank rows and horizontal margins clamp to visible text.
    /// The Window adapter converts pixels using its current font/DPI metrics.
    pub fn source_point(&self, column: u16, row: u16) -> Option<SourcePoint> {
        self.source_point_at(f64::from(column), f64::from(row))
    }

    /// Fractional logical coordinates retain subcell pointer precision, allowing
    /// the right half of even a one-column grapheme to select its end boundary.
    /// The host must use this frame's font/DPI geometry and check is_current_frame.
    pub fn source_point_at(&self, column: f64, row: f64) -> Option<SourcePoint> {
        if !column.is_finite()
            || !row.is_finite()
            || column < 0.
            || row < 0.
            || column >= f64::from(self.viewport.columns)
            || row >= f64::from(self.viewport.rows)
        {
            return None;
        }
        let y = row.floor() as i32;
        let line = self
            .lines
            .iter()
            .filter(|line| !line.clusters.is_empty())
            .min_by_key(|line| {
                if y < line.y {
                    line.y - y
                } else {
                    (y - (line.y + i32::from(line.height) - 1)).max(0)
                }
            })?;
        let x = column;
        let mut previous: Option<&DrawCluster> = None;
        for cluster in &line.clusters {
            if x < f64::from(cluster.x) {
                return Some(match previous {
                    Some(previous)
                        if x - f64::from(previous.x + previous.width as i32)
                            < f64::from(cluster.x) - x =>
                    {
                        SourcePoint {
                            byte: previous.source.bytes.end,
                            exact: previous.source.exact,
                        }
                    }
                    _ => SourcePoint {
                        byte: cluster.source.bytes.start,
                        exact: cluster.source.exact,
                    },
                });
            }
            if x < f64::from(cluster.x + cluster.width as i32) {
                let at_end = (x - f64::from(cluster.x)) * 2. >= f64::from(cluster.width);
                return Some(SourcePoint {
                    byte: if at_end {
                        cluster.source.bytes.end
                    } else {
                        cluster.source.bytes.start
                    },
                    exact: cluster.source.exact,
                });
            }
            previous = Some(cluster);
        }
        previous.map(|cluster| SourcePoint {
            byte: cluster.source.bytes.end,
            exact: cluster.source.exact,
        })
    }

    /// Whether the source bytes represented by this visible cluster overlap
    /// the current Reader-only selection.
    pub fn is_selected(&self, cluster: &DrawCluster) -> bool {
        self.selection.as_ref().is_some_and(|selection| {
            selection.start < cluster.source.bytes.end && cluster.source.bytes.start < selection.end
        })
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub block: usize,
    pub text: Range<usize>,
    pub source: SourceLocation,
    pub(super) owner: Arc<()>,
}
#[derive(Clone, Debug)]
pub enum Input {
    ScrollRows(i32),
    Wheel(i16),
    Page(i16),
    Home,
    End,
    Horizontal(i32),
    Heading(usize),
    FocusLink { backwards: bool },
    ActivateLink,
    Select { start: usize, end: usize },
    ClearSelection,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Link(LinkTarget),
}
