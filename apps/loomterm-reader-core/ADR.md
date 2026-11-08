# Independent Reader content and view boundary

Date: 2026-11-08. Status: Accepted for the standalone M4-A/M4-B implementation;
Proposed for production GUI integration. This module-local ADR is indexed by the
[Reader contract](README.md); it does not change either application workspace.

## Context

A read-only preview needs explicit file identity, a renderer-independent content
model, cancellation and bounded resources. The CLI/Daemon and Window host are
being developed concurrently in separate ownership scopes. Connecting a content
parser to live terminal resources would give it lifecycle responsibilities it
does not need. Building through a native terminal workspace would also make pure
Markdown checks depend on unrelated native engines.

## Decision

Use `apps/loomterm-reader-core` as an independent Rust 1.96.1 library workspace,
with its own Cargo.lock. This is an actual content API/build boundary, not another
application or general document platform. Future consumers depend on its path
only after their owner coordinates the production manifest/lockfile changes.

Use fixed `pulldown-cmark 0.13.6`, with default binary/HTML-output features off,
to map CommonMark and selected extensions into a bounded flat AST. Parser types
stay private. `Reader` owns one temporary content slot and a cancellation token;
the GUI owns scheduling and windows. The Daemon remains the sole working terminal
owner. File reads follow OS user permissions, with local-file/type checks and
resource budgets. There is no directory grant, broker, plugin execution, network
image fetch, document write, active HTML or math rendering.

Changed source is fully parsed before atomic installation. Reuse the AST only
when source bytes match. Return a bounded UTF-8 change range for presentation;
do not fragment-parse Markdown around a local edit because reference definitions
can affect content elsewhere. Tag every result by request identity and generation
so stale work and other clients cannot overwrite this slot.

## Consequences and alternatives

The isolated workspace adds a small lockfile and standalone checks while keeping
concurrent production files untouched. Integration and shared documentation
navigation require a later coordinated patch. No GUI, IPC or native engine build
is pulled into the content crate. Parser and all transitive license texts are
retained in the [dependency inventory](THIRD_PARTY.md).

Handwriting CommonMark would add avoidable syntax/compatibility maintenance.
A fully exposed upstream AST would couple GUI contracts to parser releases.
Putting file parsing in a terminal owner would cross the content/lifecycle
boundary. A standalone editor/plugin platform or filesystem authorization service
has no current requirement. A fully incremental parser or enforced worker-process
memory/time sandbox remains a separate decision if measurements justify it.

## Acceptance

Require pinned metadata/fmt/check/Clippy and behavior tests for Markdown, math,
images, Unicode, encoding, regular-file failures, budget rejection, cancellation,
release, same-target refresh and stale/cross-client result isolation. Keep finite
benchmarks reproducible and distinguish retained allocation estimates from peak
process memory. Record untested platforms. GUI integration must separately prove
one private preview per client, visible error handling, bounded worker/image
resources, source copying, scroll behavior and shutdown cleanup without affecting
Workspace/Shell/Agent resources. Core unit tests do not meet those GUI gates.

## M4-B layout and reading state

Extend this content workspace with a `view` module instead of another crate or
renderer. `Document` remains the source AST. `Projection` owns bounded logical
display blocks/runs and source mappings; `ReaderView` adds instance-private
viewport/anchor/cache/selection state. `Frame` contains visible logical draw
data, semantic styles, resource placeholders and a revision identity. The host
owns pixels, input conversion, scheduling, image/math rendering and the private
native window. No terminal grid/runtime, IPC, GPU or UI dependency is introduced.

Use fixed `unicode-segmentation`, `unicode-width` and `unicode-linebreak` for
graphemes, display widths and break opportunities. Handwritten Unicode rules
would create avoidable CJK/emoji/combining regressions. Logical column metrics
allow deterministic layout without fixed pixels; font shaping/BiDi/proportional
measurement need a later host decision. This logical-metric contract is accepted
for standalone behavior tests and Proposed for actual GUI use.

Build projection once per changed source; unchanged bytes reuse the projection.
Do not extend the Markdown parser for scrolling. Bound single logical blocks,
global projection payload, frame clusters and layout work. Split code fences by
line, preserve code/table horizontal overflow, and reject over-budget candidates
explicitly. Cache glyph pages using count/byte limits and LRU eviction. Keep a
mixed exact/estimated height index to locate distant regions without shaping
the prefix. Resize rebuilds estimates and reflows visible pages, preserving a
grapheme anchor. Refresh maps source ranges with explicit approximate anchors
for replaced/deleted content. Counts/charges are not a peak-RSS measurement.

The adapter holds one active worker and one replaceable pending target. It must
check frame identity before paint, and resource callbacks carry projection-bound
tickets. Selection copies source Markdown; literal block search is bounded.
Navigation returns link information without opening files/browser. Local links
reuse the existing local-reference rules. Network images and active document
content stay disabled. Per-client private window count and shutdown belong to
M3; Reader instances cannot change Shell/Agent/Workspace or terminal lifecycle.

Acceptance adds deterministic Mock Host regressions for layout, budgets, rapid
scroll/resize, reflow, navigation, refresh/stale results and release; finite view
benchmarks measure actual elapsed work. Cache allocations are separately labelled.
Native window and real image/math/interaction acceptance remains an M4-C gate;
passing this isolated workspace does not close full M4.
