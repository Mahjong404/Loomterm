# Loomterm Reader Core

M4-A supplies bounded local Markdown content; M4-B adds logical document layout,
a private viewport, navigation and a tested Mock Host adapter. The separate M3
Window Draft PR #1 pins a fixed Reader revision for a native text/image surface;
this library itself owns no native window. It produces draw data and bounded
local PNG/JPEG/WebP RGBA8 decode results without a GPU, terminal renderer or
working Terminal Owner. Native image painting and the new semantic paint roles
have scoped Windows HWND fixture Pass. M3's Window-owned style adapter is now
in its Draft PR #1 on fixed Reader `7e11dcca…`; formula painting and the full
M4 visual/lifecycle gates remain Pending. The optional `math` feature now
typesets a restricted offline TeX subset into real RGBA pixels; its native
HWND integration and process-cache release acceptance remain Pending.

The crate directory is the Reader physical development/build boundary. Its own
manifest, lockfile, source, tests and output do not change either concurrent
application workspace. See the [boundary ADR](ADR.md), [dependency notices](THIRD_PARTY.md)
and [Mock Host regression](tests/view_host.rs).

## Open a selected file

The host creates one `Reader` for each Window Client's one temporary preview
slot. It passes the file the user selected; the library has no directory scanner,
terminal input, Agent adapter, IPC or working terminal owner.

```rust
use loomterm_reader_core::{Apply, Limits, Reader};
use std::path::Path;

fn load_selected_file(path: &Path) -> Result<Reader, loomterm_reader_core::ReaderError> {
    let mut reader = Reader::new(Limits::default())?;
    let request = reader.open(path)?;
    // In a GUI, move execute() to the bounded content worker.
    let result = request.execute();
    match reader.apply(result) {
        Apply::Ready(_) => assert!(reader.document().is_some()),
        Apply::Failed(error) => return Err(error),
        Apply::Cancelled | Apply::Stale => {}
    }
    Ok(reader)
}
```

`open` captures an absolute target without reading the file. Malformed/nonlocal
path arguments return an error and leave the current slot untouched. A valid new
target cancels the older request and clears its old content. I/O/encoding/parse
failures become `ReaderState::Failed`. On a same-target reload failure the last
good document remains available, alongside the explicit error state.

## Consume the content model

`Document` supplies the path, observed `SourceStamp` and `Arc<Content>`.
`Content` owns the exact UTF-8 source plus a flat preorder `Vec<Node>`.
Each node has a byte `span`, optional parent ID and exclusive `subtree_end`.
`children(id)` skips subtrees to enumerate immediate children. IDs and spans are
revision-local; no recursive tree drop or UI/parser/IPC types cross this API.
An optional UTF-8 BOM stays in the source and spans account for it. UTF-16 and
malformed UTF-8 fail explicitly with the first invalid byte position.

Supported nodes cover headings, paragraphs, ordered/unordered/nested lists,
task markers, table alignment/head/rows/cells, fenced and indented code, quotes,
emphasis/strong/strike, code spans, links/reference links, image references,
rules, hard/soft breaks and inline/display math. CommonMark parsing and enabled
table/task/strike/math extensions come from fixed `pulldown-cmark 0.13.6`.
Unsupported optional extensions are not enabled. HTML is preserved as inert
literal nodes, without executing markup or extracting embedded resources.

Math is `NodeKind::Math { display, source }`, using `$...$` or `$$...$$`.
Renderers receive the delimiter-free syntax and original byte span. Code remains
literal. No math engine, command/macro execution, external includes, link opener
or syntax highlighter is provided. Every future text sink must display content
literally/escape its control syntax, including ANSI controls; raw strings must
not be streamed as trusted terminal output or interpreted as active HTML.

## Refresh and cancel

`following()` defaults to true. The host calls `poll()` from a debounced change
notification or bounded polling schedule; it returns no request while paused or
busy. `set_following(false)` pauses future automatic requests; an already running
one may finish, or the host can call `cancel()`. `reload()` is an explicit refresh
even while paused. There is no hidden watcher/thread or automatic background I/O.

Every refresh reads bounded bytes rather than relying only on size/mtime, so
same-size and same-timestamp saves are detectable. Identical source reuses the
existing `Arc<Content>` with refreshed metadata. Changed input is fully reparsed;
`Update::Replaced { old, new }` reports common-prefix/suffix-trimmed UTF-8 byte
ranges. These are presentation invalidation hints, **not fragment-parse bounds**:
a reference definition changed at the end can alter an earlier node. Source copy
uses `Content::source`; the view maps these ranges to a reading anchor. Final
pixel geometry belongs to the host.

`Cancellation` is a shared per-request token. Checks occur before work, between
16 KiB reads, while diffing and between parser events. `Reader::cancel`, `clear`,
drop, or a newer target cancel the old token. `apply` checks target, generation
and token identity, rejecting stale or accidentally misrouted client results.
Cancelled/error paths drop partial buffers and ASTs; no file handle is retained
by a document. Caller-owned document Arcs remain alive until the caller drops them.

## Enforce budgets and file boundaries

| Budget | Default | Hard ceiling |
| --- | --- | --- |
| Source bytes, including BOM | 4 MiB | 16 MiB |
| AST nodes, including Document root | 100,000 | 250,000 |
| AST depth, excluding root and including leaves | 64 | 128 |
| Image references, including blocked/repeated refs | 256 | 4,096 |
| Retained node string/path/table payload | 8 MiB | 32 MiB |
| Encoded document/image path bytes | 32 KiB | 32 KiB |
| Local image encoded file bytes per decode | 8 MiB | 16 MiB |
| Local image output pixels per decode | 4 million | 8 million |
| Local image width or height | 8,192 | 16,384 |

Budgets can be lowered to zero. Exceeding a budget fails the entire candidate;
partial content is never installed. Reads use a 16 KiB stack buffer and at most
one overflow-probe byte. File metadata is only an early size check; stream growth
is also bounded. `ParseStats` exposes charged payload/count/depth, and
`retained_bytes()` estimates retained source/arena/payload allocation. It excludes
allocator overhead, document wrappers and parser scratch; it is not peak RSS.
Vec capacity and temporary allocations are distinct from logical node counts.

Explicit source opens allow regular files with local path syntax and ordinary
OS user read permission. URL/UNC/device/alternate-stream paths, DOS device names,
and symlink/reparse components are rejected before reading; Windows additionally
opens the final component with `FILE_FLAG_OPEN_REPARSE_POINT` and rechecks its
metadata. No writes, elevation, Source Folder containment ACL, directory grant
or Daemon file broker are introduced. Non-UTF-8 path names are unsupported.

Metadata before/after a read detects ordinary in-place size/mtime changes and
returns `ChangedDuringRead`. An atomic replacement may yield the coherent old
file version; the next poll reads the current path. Metadata is advisory, not a
cryptographic revision ID or an atomic filesystem snapshot. Concurrent hostile
path swaps, OS-mapped drives and slow filesystem calls are not contained by this
library. Cooperative cancellation cannot interrupt a blocking OS call or a
single parser construction/iteration. Parser scratch is bounded indirectly by
the source ceiling, not by a hard allocator or execution-time quota. Run on a
bounded worker; no UI responsiveness or sandbox claim follows from these tests.

Markdown image parsing performs **zero filesystem/network I/O**. References
resolve as follows:

- Relative image paths use the source document's directory, never Shell cwd or
  Workspace Source Folder. Absolute local paths and `..` are allowed by OS
  permissions; Source Folder is not a sandbox.
- Percent escapes decode once as UTF-8. A literal `#fragment` is removed before
  decoding; queries, invalid escapes/control bytes and nonlocal namespaces are
  blocked with a reason. `file:`, `data:`, remote URLs and protocol-relative paths
  are not loaded. No filesystem existence check occurs while parsing.
- `ImageReference::Local` is a candidate path, not an open handle, decoded image
  or authorization grant. The explicit loader revalidates it at open time and
  applies the source reader's regular-file/reparse checks. Broken images return
  an image error without losing the Markdown document.

`load_local_image(reference, ImageLimits, &Cancellation)` runs synchronously on
the host's bounded worker. It sniffs file contents and accepts only PNG, JPEG
and WebP; it never loads a URL. The result is owned, top-down, tightly packed
RGBA8 with `width`, `height`, `stride_bytes()` and `rgba`. The host pairs that
result with the requesting `ResourceTicket`, rejects stale tickets before paint,
and drops buffers when the per-client window closes. For a visible image request,
`frame.projection.document.content.nodes[ticket.node].kind` supplies its
`ImageReference`; the host calls the decoder on a bounded worker, then converts
pixel dimensions to `LogicalSize` for `resource_size` on the UI state owner.
`ImageLoadError` distinguishes blocked, missing/open/read, encoded-byte, pixel/
dimension, decoder-budget, unsupported-format and corrupt-decode failures.
Image files are not held
open after return. Encoded bytes, dimensions and RGBA output pixels have checked
per-decode ceilings; decoder `max_alloc` is a best-effort setting, **not** a hard
peak-RSS or CPU quota. Cancellation is checked during file reading and before/
after codec work, not inside a codec. The host must additionally cap aggregate
decoded buffers and worker concurrency. Local file changes require re-requesting
that image resource. M3's Window-owned adapter in Draft PR #1 pins Reader
`7e11dcca3c59deb87545c75ec4f387f95e09004e` and has a scoped native
PNG color/alpha/refresh/error/budget/close fixture; Window ownership and the
remaining visual/lifecycle acceptance stay with M3.

## Display a document

`view::ReaderView` wraps the content slot and owns only its private reading state.
The layers are `Document/Node` (source semantics), `Projection/Block/Span`
(logical display text and source mappings), and `Frame/DrawLine/DrawCluster`
(visible draw data). All types use Rust data and logical units. No parser,
emulator, GUI or IPC type crosses the view API.

```rust
use loomterm_reader_core::{view::*, Cancellation, Limits};
# fn main() -> Result<(), ViewError> {
let mut view = ReaderView::new(
    Limits::default(), ViewLimits::default(), Viewport::new(80, 30)?,
)?;
let request = view.open(std::path::Path::new("selected.md"))?;
// Execute on a bounded content worker; deliver the result back to this view.
let result = request.execute();
if view.apply(result) == ViewApply::Ready {
    let frame = view.frame(&Cancellation::default())?;
    // A pending frame is partial: schedule another bounded frame() call.
    // Validate again at paint time after draining resize/input/load events.
    if view.is_current_frame(&frame) {
        for line in &frame.lines {
            for cluster in &line.clusters {
                let literal_text = frame.text(cluster);
                let paint = frame.paint_style(line, cluster);
                // Host clips and paints literal_text using paint and its theme.
                let _ = (literal_text, paint);
            }
        }
    }
}
view.close();
# Ok(())
# }
```

Heading levels, block roles, quote/list depth, emphasis/strong/strike/code/link
styles and table cell identities are semantic theme inputs. The host chooses
fonts, palette and pixel scale. `Viewport` is 1..1024 logical columns by 1..256
rows; `Metrics` configures indentation, block/heading gaps, tab stops, ambiguous CJK widths
and image/math placeholders. These units do not allocate a terminal grid.

`Frame::paint_style(line, cluster)` resolves heading level, weight, italic,
strikethrough, inline/code-block/quote/table surface, table column, link underline
and focus, and Reader-only selection into a value without colors or pixel sizes.
The host paints the full code-block line surface, including empty lines; table
separators remain literal bounded draw clusters. `Metrics::heading_gap` defaults
to two logical rows (ordinary blocks one) and triggers anchor-preserving reflow
through `set_metrics`. `DrawLine::list_depth` and `quote_depth` keep the host's
indent guides separate from the literal projected text. Heading font size and
actual glyph advances must be reconciled by the native adapter; the logical
column model does not certify proportional-font wrapping or pixel-perfect layout.

Wrapping uses whole extended graphemes, Unicode line-break opportunities and
Unicode display widths, including CJK and emoji sequences. A word exceeding the
line width can break between graphemes. Combining marks across inline styles
stay with the leading grapheme's style. This is logical column layout; native
font shaping, BiDi, kerning and proportional pixel measurement remain host work.

Code is split into bounded no-wrap logical lines. Table rows use preferred cell
widths and Markdown alignment; they also remain unwrapped. `Input::Horizontal`
scrolls overflowing code/table rows without moving wrapped paragraphs or fitting
rows. Each row clamps its shift to its own overflow. The input offset is bounded
by the latest visible frame's horizontal extent, so reversing at the right edge
responds immediately; a host obtains a frame before forwarding horizontal input.
Extremely
wide rows or code lines fail the block budget explicitly. Images and math are
object placeholders in the frame; the image decoder is a separate resource API.
Their AST nodes retain alt text/reference or formula source.
Raw HTML stays literal and C0 controls other than tabs/newlines become U+FFFD.
The host must paint literal text and clip every draw operation to the viewport,
including partly visible wide graphemes and resource boxes with negative origins.

`SourceLocation::exact` distinguishes direct source byte maps from coarse ranges
for entities, generated markers, normalized text and placeholders. Node IDs,
block indices, selection and search hits belong to a projection revision.
`Frame::text` accepts clusters from that same frame. `Frame::hit_test` includes
the interior of a wide grapheme and returns no hit outside the viewport.
For native drag selection, `Frame::source_point(column, row)` clamps margins and
blank rows to visible text and chooses a grapheme's source start/end by its
logical half-cell. Its `exact` flag distinguishes direct source text from
generated markers. `Frame::is_selected(cluster)` reports source-byte overlap for
painting. The host must check `is_current_frame` before using pointer results;
pixel-to-column conversion remains the host's font/DPI responsibility.
`Frame::source_point_at(f64, f64)` preserves fractional logical coordinates so
the right half of a one-column glyph maps to its source end. Pass pointer
pixels divided by the current font cell metrics without integer truncation.
Non-finite, negative and outside-viewport coordinates return `None`; clamp
captured drags at the host boundary before calling. The integer API remains
compatible. Source selection copies original Markdown bytes, including markup
between the endpoints; it is not formatted-text copying.

## Bound layout and keep a reading position

| View budget | Default | Hard ceiling |
| --- | --- | --- |
| Projected blocks | 100,000 | 250,000 |
| Style/source runs | 200,000 | 500,000 |
| Projected UTF-8 text | 16 MiB | 32 MiB |
| One paragraph, code line or padded table row | 16 KiB | 64 KiB |
| Table columns | 64 | 128 |
| Image/math placeholders | 1,024 | 4,096 |
| Cached layout allocation charge | 8 MiB | 64 MiB |
| Cached blocks | 64 | 512 |
| Visible frame clusters | 32,768 | 262,144 |
| New layout blocks per frame call | 8 | 256 |
| New projected bytes shaped per frame call | 128 KiB | 4 MiB |

The block, frame-cluster and work-block budgets must be nonzero; work bytes must
cover at least one allowed block. Other limits can be zero for explicit refusal.
The core source/AST budgets apply independently. Projection budget failures do
not install partial view content. On refresh, the last good view may remain
alongside `error()`; `source_state()` alone cannot report a projection failure.
Navigation/configuration errors return `Result` directly; frame errors also set
the visible error slot. A successful new content result clears that slot.

Projection is built once per changed document. Small scrolls and resize never
parse Markdown. A Fenwick index stores mixed estimated/measured block heights;
lookup/update is O(log blocks), and viewport/metric changes rebuild estimates in
O(blocks). The top block is shaped first, followed by visible blocks within the
per-call work budgets. `Frame::pending` requests another call. LRU eviction bounds
cached pages by both count and allocation charge; pages may need rebuilding
after eviction. If cache limits prevent progress across calls, return an explicit
`Budget(CacheBytes)` error. A block is at most `block_bytes`, so even a long code
fence is handled as separate pages rather than one full-document layout.

The retained source, AST, projection and height index stay bounded by their own
limits; they are not evicted with glyph pages. Frame output has a cluster cap.
The cache charge counts internal line/atom Vec capacities, excluding allocator
and map overhead, temporary pages, frames and caller-retained Arcs. It is **not
peak RSS**. Cooperative cancellation and byte/count limits are not hard CPU or
allocator quotas. Inputs, resize and final deallocation can perform bounded
synchronous work; keep them away from latency-sensitive draw callbacks.

Position is a block plus a row within it. Small scrolls step measured local
heights; jumps exceeding four viewport heights use estimates and mark the source
anchor `Approximate`. `Home`, `End`, headings and search explicitly locate their
targets. `End` measures the tail and fills the viewport backwards. The total
extent is labelled estimated until all block heights are known.

Resize preserves the leading projected grapheme and any offset inside a tall
placeholder, then reanchors after reflow. Refresh maps the old source anchor
through the core change range; replacement/deletion at that anchor is marked
approximate. A subsequent unchanged reload reuses the projection/cache. Resource
size changes invalidate affected pages and preserve the same reading anchor.
Pixel-perfect anchoring across font/shape changes is not claimed.

## Read and navigate without opening resources

`Input` supplies row/wheel/page scroll intents, Home/End, horizontal scroll,
zero-based heading navigation, link focus/activation and source-byte selection.
The host maps physical keys/mouse to these intents. `Frame` includes selection
and focused-link identity for painting. `selection()` returns original Markdown,
bounded at grapheme boundaries; selection endpoint validation checks the supplied
offset without scanning from the source start on every drag event. Formatted
text copying is not implemented.

`Projection::search(query, max, cancel)` searches literal, case-sensitive
projected text within individual blocks, with a 256-byte query and at most
1,024 hits. Matches expand to grapheme boundaries. `locate_search` rejects hits
from an older/foreign projection. There is no regex, normalization or cross-block
search. Headings expose levels and basic slugs with duplicate suffixes.

For a native search box, the Window adapter clones the current `Arc<Projection>`
from `ReaderView::projection()`, calls `Projection::search` on its bounded content
worker, then passes a chosen `SearchHit` to `ReaderView::locate_search` and asks
for a new frame. Hits carry revision identity: refresh, target replacement or
another Window Client makes an old hit fail with `InvalidPosition`. The adapter
owns query input, result index and highlight presentation; do not scan a large
document inside WM_PAINT or duplicate the Markdown parser in the host.

M3's isolated Win32 Reader adapter now exercises this fixed `7e11dcca…` API
through one cancellable search worker, limiting native queries to 256 UTF-8
bytes and results to 128. Its posted-message HWND fixture passed UTF-16 text
input, F3/Enter navigation, current-hit highlighting (1,981 highlight pixels),
and Reader close without terminating the same Shell (GDI objects 31 to 15).
The final posted-message rerun also cleared the old hit highlight after F5
(zero old-highlight pixels). M3 published the four Window-owned search files
in Draft PR #1 at `8e0cfeb5be8dace89e8883fdefd08a0be7664354`, still pinned
to fixed Reader `7e11dcca…`. Physical foreground Ctrl+F input and broader
search/selection acceptance remain Pending; posted messages do not certify
physical keyboard input.

Explicit link activation returns `Action::Link(LinkTarget)`; it never opens a
browser or file. Fragment targets navigate matching local heading slugs. Local
file links use the same document-directory/percent-decoding/local-path rules as
image references, including allowed `..` and absolute paths. They do not inherit
a directory grant. Only HTTP/HTTPS/mailto are returned as informational external
targets; executable/unsupported schemes are blocked. Hosts must require an
explicit user action, revalidate local file opens and choose how to handle
external targets. Passive focus, search and layout perform no resource I/O.

Visible `Frame::resources` requests identify image/math nodes via revision-bound
`ResourceTicket`. Blocked images emit no request. The host reads alt/reference
or formula syntax from the frame projection's AST, deduplicates requests and
returns `resource_size(ticket, LogicalSize)` or `resource_failed`. Sizes are
1..4096 columns by 1..256 rows. Old/foreign tickets return false, preventing late
resource results from reflowing a newer document or another client. Pending
requests may recur until resolved; they do not start work themselves.

Inline `$...$` and display `$$...$$` become math syntax nodes; the Markdown parser
does not validate LaTeX commands. The optional `math::render_math` API uses fixed
RaTeX 0.1.15 and embedded KaTeX OFL fonts to return `RenderedMath`: owned top-down
RGBA8 pixels plus a baseline in physical pixels. The host passes `MathOptions`
with a DPI-adjusted pixel em and foreground color, then maps bitmap dimensions
to `LogicalSize` and applies the original revision-bound ticket. Invalid formulas
return `MathError`; paint an error and the retained source rather than hiding the
failure. Do not start a job per frame or install stale pixels after refresh.

The default feature set stays unchanged. Enable `--features math` explicitly.
This prototype supports ASCII TeX notation for fractions, radicals, scripts,
sums/integrals, common Greek commands, matrices and limited text/font commands.
It deliberately rejects macros, arbitrary commands/dimensions, HTML, URLs, file
commands, direct non-ASCII math input and system-font fallback. See the finite
allowlist in [math.rs](src/math.rs). Both debug and release embed fonts; no JS,
browser, process, runtime font directory or network resource is used.

| Formula budget | Default | Hard ceiling |
| --- | --- | --- |
| Source bytes | 2,048 | 4,096 |
| Lexical tokens | 512 | 1,024 |
| Brace depth | 24 | 32 |
| Drawing items / path commands | 4,096 each | 8,192 each |
| Output pixels | 524,288 | 1,048,576 |
| Output dimension | 2,048 | 4,096 |
| Physical pixels per em | 24 | 128 |

Cancellation is checked between synchronous phases, not inside RaTeX or PNG.
Input limits and upstream depth/expansion/layout checks bound supported work;
they are not hard CPU or allocator quotas. Upstream process-global font,
outline and glyph-mask caches outlive individual Reader windows. In particular,
the glyph-mask cache permits 8,192 entries / 64 MiB and exposes no host purge
API. These caches are additional to the Window's 16 MiB BGRX image cache.
They must be included in process-memory observation and a production adoption
review; this opt-in prototype does not claim full resource-close acceptance.
Native formula painting, inline baseline alignment and Resize/scroll acceptance
remain Pending until M3 consumes the raster API in its existing image surface.

## M3 adapter and M4-C entry conditions

The small adapter contract is `ReaderView::{open,refresh,poll,apply,resize,frame,
input,resource_size,resource_failed,cancel,close}`, plus `source_state`, `error`,
`Frame::budget` and `is_current_frame`. The lower-level content API remains
available independently. No GUI or serialization dependency is required. M3's
owner pins a Reader revision in its own host workspace; final shared dependency
and lockfile integration still requires owner coordination. This workspace makes
no host manifest change.

M3 must provide the following before real M4-C acceptance:

1. Own at most one private temporary Reader window per Window Client. Explicit
   open replaces its target; close cancels work and releases view/render assets.
   Confirmed parent-client death closes it; temporary Daemon loss alone does not.
2. Run one active content job and at most one replaceable pending target. Drain
   stale results and retain at most the necessary frame/projection snapshots.
   Cancellation alone does not bound a task queue. Move final large Arc drops
   for replaced targets/results/frames off the event loop when needed.
3. Forward resize and normalized keyboard/mouse intents; paint current clipped
   frames with host fonts/theme. Revalidate `is_current_frame` before painting:
   new frames, input, resize, resource reflow, replacement and close invalidate
   old frames. Continue pending frames through the bounded scheduler.
4. Load only explicitly permitted local image candidates, with open-time path
   checks and byte/pixel/decoder/cache budgets. Supply real image/formula sizes
   and release textures/render objects with the window/revision. No automatic
   network image access, active HTML, formula scripts or directory broker.
5. Validate real Windows window ownership/focus/close/resize, reading and source
   selection, anchor behavior, CJK/emoji drawing, wide code/table scrolling,
   image failures/cleanup, readable formulas and bounded responsiveness. Mock
   Host checks do not certify these visual or native lifecycle behaviors.

The adapter never creates, resizes or terminates working PTYs, Shells, Agents or
Workspace resources. Reader positions are never shared with terminal scrollback
or other clients. M3's shared terminal-scroll work is independent of this view.

## Reproduce core checks

Enter this independent workspace so Cargo uses its compiler pin. On Windows,
use Rust 1.96.1 with the matching MSVC linker/SDK, rustfmt and Clippy. No Zig,
Ghostty, Window host build or PTY watchdog is needed for these content-only files.

```powershell
cd apps/loomterm-reader-core
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked --target x86_64-pc-windows-msvc
cargo test --locked --target x86_64-pc-windows-msvc
cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo bench --bench bounded --locked --target x86_64-pc-windows-msvc
cargo bench --bench image --locked --target x86_64-pc-windows-msvc
cargo bench --bench view --locked --target x86_64-pc-windows-msvc
cargo test --all-features --locked --target x86_64-pc-windows-msvc
cargo bench --bench math --features math --locked --target x86_64-pc-windows-msvc
cargo run --example math_raster --features math --locked --target x86_64-pc-windows-msvc -- target/math-preview
```

Unit and real-file tests cover syntax, Unicode/complex text, strict encoding,
all budgets, local/remote image rules, missing/directory/locked/read-error files,
refresh/pause/replacement/delete/recreate, stale/cross-client results, cancellation
during reading/event conversion and Arc/file-handle release. Tests create only
owned synthetic directories in ignored `target/test-files`, removed on Drop;
the finite benchmark similarly removes `target/bench-files/<pid>`.
The core benchmark measures five parses of fixed 376,832-byte input (81,921 nodes),
4 MiB plain text, unchanged and changed reloads, and a pre-cancelled request.
The view benchmark measures read/parse/projection, first visible layout,
200 bidirectional scrolls, 20 resizes and unchanged/changed reloads of 10,000
paragraphs. Its owned `target/view-bench-files/<pid>` directory is removed on
Drop. Mock Host fixtures use `target/view-fixtures`. Core/view benchmarks print JSON
and set no machine-dependent timing gate or peak-RSS claim. The separate image
benchmark decodes generated local 256² and 1024² PNG files 20 times each,
reports median and nearest-rank P95 elapsed microseconds, and removes its
per-process fixture files. Its numbers are codec/I/O timings, not Window frame
timings or resident-memory measurements.

Windows 11 x64/MSVC validation on 2026-11-09 passed fixed Rust 1.96.1 metadata,
fmt, check, strict Clippy, 35 unit/integration tests and two compiled API examples.
Mock Host coverage includes a visible image ticket through local decode and
resource-size reflow, semantic/grapheme layout, 10,000 paragraphs, long
code, wide tables, resource reflow, refresh anchors, search/headings/links,
selection, rapid scroll/resize, cancel/stale jobs, private-state isolation,
budget/error paths and resource release. Linux/macOS runtime behavior, Unix
symlink tests, hostile filesystem races, formula readability,
broader native image behavior and full window lifecycle/reading interactions
remain unverified. These checks do not close M3, M4 UI or M6 performance gates.

Windows 11/MSVC follow-up on 2026-11-10 passed fixed Rust 1.96.1 locked/offline
metadata, fmt, all-target/all-feature check, strict Clippy, 37 default tests,
41 math-enabled tests and two compiled API examples. New coverage includes
fractional/cross-line grapheme selection and refresh invalidation, real offline
formula RGBA, invalid/over-budget/cancelled expressions, DPI-scaled dimensions,
revision-ticket reflow/close rejection, and horizontal edge/reverse/Resize
behavior that keeps narrow tables and short code rows visible. The raster example is executable
evidence, not a native Reader window.

The finite math benchmark on the initial offline-raster revision measured a
173×59 quadratic formula: first call
1,959 µs, then 20 warm calls with median/P95 153/168 µs. A separate fresh-process
Windows memory observation reported 6,217,728 bytes peak working set and
1,875,968 bytes peak PagefileUsage for that standalone benchmark. These are
actual process counters for this small case, not multi-Reader peak memory,
worst-case cache residency or native Paint time. M3 must measure those separately.
