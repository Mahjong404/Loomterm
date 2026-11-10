# Window host patch ledger

The separate Loomterm-owned launcher now has an optional native Reader HWND
behind `--reader-file`. This adds no WezTerm adapter patch and keeps the fixed
Reader Core dependency at `f9264e6501541a863e6fcefc36e1d19a8f8027ed`.
GDI draws text clusters and bounded local image pixels from Core frames; DPI,
resize, wheel, key navigation,
selection copy and SC_CLOSE stay in the Window process. A native fixture proved
paint/resize/synthetic wheel, error page, and Reader close with the terminal,
Shell and Daemon still alive. The measured launcher GDI count fell 48 to 47
after Reader close. The image adapter has one decode worker, 16 pending visible
requests and a 16 MiB decoded buffer cap per HWND. RGBA is composited to white
BGRX before a top-down DIB paint. Native red/blue/alpha pixels, unchanged-text
image refresh, truncated/over-limit placeholders and close without Shell loss
passed. The host maps Reader Core PaintStyle to bounded GDI font/color/surface
roles; Tab/Shift+Tab focuses links without opening external targets. Fixed 7e11
native style and image HWND cases pass; a private-cache ownership test proves
one Reader close does not clear another Reader's decoded pixels. Formula
resources, physical-device wheel, exact selection/copy and terminal GPU
release remain unaccepted. Formal M3 Gate is still Pending.

The adapter targets the fixed revision in [UPSTREAM.json](UPSTREAM.json).
It checks the original Git blob hashes before writing any of four files. Source
acquisition is separate; run it once on a pristine, independent source copy.
The upstream MIT notice and native/font licenses remain with that copy.

| File | Change | Reason |
| --- | --- | --- |
| `config/src/config.rs` | Replace three root resolvers with required absolute `LOOMTERM_WINDOW_ROOT` children | Isolate data/plugins, cache/blobs, runtime/logs/GUI sockets without changing Shell HOME |
| `wezterm-gui/src/main.rs` | Early adapter probe, required root and distinct Windows AppUserModelID | Reject accidental upstream launch; separate taskbar identity |
| `window/src/os/windows/window.rs` | Checked WM_CHAR UTF-16 pair state; clear it on focus loss; remove invalid one-unit char casts | Prevent surrogate input from aborting the host; preserve valid BMP and scalar events |
| `wezterm-input-types/src/lib.rs` | Encode Win32 Unicode as UTF-16 records; normalize synthetic WM_CHAR codes outside the VK range | Preserve supplementary text at the outer PTY instead of clamping to U+FFFF |

Preparation also writes a fixed `.tag` version stamp. The build helper verifies
the four resulting source SHA-256 values before generating a binary manifest.
SHA-256 uses the already adopted CLI dependency family `sha2` 0.10 (MIT OR
Apache-2.0); it is needed before any host execution.

No renderer, font, terminal parser or multiplexer source changes. The host
retains its outer PTY and TUI child; the existing Daemon owns work terminals.
An exit-code probe is compatibility detection, not binary authentication.
Installer identity, icon/version resources, complete binary/font redistribution
inventory and native Window acceptance remain release gates.

The public diff includes the reviewable host patch alongside the preparation tool.
Window-only Win32/IPC observers derive from the repository M1 fixture and do not
change its shared source. PNG is a deterministic generated test input, not a
third-party asset. Remaining input/visual blockers stay explicit. A maintainer
manual receipt passes Chinese IME preedit/cancel/commit, host Clipboard copy/paste,
CJK/Emoji glyphs and mouse titlebar X; same Shell reattach is verified. This
does not certify bracketed-paste bytes, native shortcuts or independent scrolling.

Adapter v2 has three review patches: original path/identity adapter, checked
Windows character decoding, and Win32 UTF-16 serialization. The latter's unit
test and real outer-PTY byte capture pass. BMP/combining WM_CHAR reaches the
work process. With M2's separately owned input-state fix, supplementary input
also reaches an actual PowerShell string as exact UTF-8. The original fixed
M2 binary still drops it. The M3 Draft PR now integrates the fixed M2
`9d97c1673cc54aac499d5f7680e8467d7ffb197f` base. M5-owned input
and viewport changes remain a separately tested source dependency, not part
of the published Window branch. A later focused physical test on the exact M5 CLI candidate
below delivered Shift+X, Ctrl+K, Alt+X and F6 through the actual Window to the
raw work process as `58 0b 1b 78 1b 5b 31 37 7e`, with the same Shell retained.
This supersedes earlier failed/foreground-skipped chord probes for that limited
sequence. Arrow/navigation keys, other function keys, physical mouse input and
raw-console non-BMP `ReadFile` remain separate; the latter still yields four
replacement characters at input CP 65001 and mode 0x280. This is not complete
native Unicode acceptance.
Window-crate metadata, fmt, all-target check, 15 Cargo tests and strict
all-target Clippy pass on fixed Rust 1.96.1 MSVC using the existing offline
dependency/toolchain cache and an isolated Window target directory.
This supersedes the earlier default-cache sha2 resolution failure and
direct-only quality scope. The Window Python suite passes nine tests with the
fixed-package case skipped when no package is supplied.
The extracted exact UTF-16 decoder and host input-types tests also passed.

## App-local ConPTY staging

[conpty.json](conpty.json) pins Microsoft.Windows.Console.ConPTY
1.24.260710001, release v1.24.11911.0, package SHA-256
9382ad7becb7e4d84e300578d8e4f4df28f43d979d9055d978c42913c47e0e9d.
The existing CLI loader already accepts this exact bundle. No CLI, PTY actor,
loader, wire or engine production source patch is needed. The stage helper
verifies the three native file hashes and original legal notices before writing
an adjacent strict bundle; differing existing bundles/notices are rejected.
No system directory is changed. The existing marker filename is retained
solely for dependency compatibility, with no product-name implication.

The system ConPTY drops Kitty APC in the controlled comparison. The fixed
bundle preserves the 186-byte APC, and the unchanged CLI plus native host
renders the checker in the complete TUI. Resize, output scroll-out, redraw
and delete pass for this one PNG. Same-ID replacement/repeat, bounded truncated
replacement rejection and a decoded image over 64 MiB have now passed real
Window bitmap checks. The launcher refuses a missing adjacent bundle and clears
the inherited system-ConPTY override; the existing CLI loader still owns exact
hash validation. Visible error feedback and bounded resource reclamation remain
Pending assertions. A finite five-cycle real-Window display/delete run sampled
host/backend private bytes and handle counts. After initial host allocation,
one run held 1,040–1,043 host handles and 188 backend handles across the cycles;
the three-second idle sample remained at 1,040/188. This is bounded process-level
evidence, not an image-placement or GPU texture count and not proof that deletion
reclaims all graphics resources. Retain the original
[license](../../third-party/licenses/microsoft-conpty/LICENSE.txt) and
[NOTICE](../../third-party/licenses/microsoft-conpty/NOTICE.md).

No fourth host patch was added for large paste. An M3-owned 2 MiB-capped raw
work-process fixture records paste delimiters, bytes and FNV-1a. In a real Window
using candidate 463df14d, 301/1,141/2,261-byte pastes pass in 0.39/2.25/8.39
seconds; 4,501 bytes produce no complete event within 15 seconds. The same
4,501-byte raw prewrapped input also stalls. The shared client framer already
uses an incremental terminator search; the bottleneck is not yet localized.
M3 sent its exact case to M2 and does not
patch shared CLI source concurrently. With separately staged M5 ee75bec4
(CLI SHA-256 1bdef492e5a0b900718693ffdf2e123baa35a4244d5dc2f99bdde9920c64945),
actual Window-to-work-process cases of 301, 4,501, 16,379, 29,771 and 262,521
wire bytes pass exact markers/digest using a same-root prestarted Daemon. The
262,521-byte case takes 15.84 seconds. A 1,225,021-byte attempt leaves the
work-process reader at zero bytes until its 180-second watchdog, consistent
with the client's 1 MiB retained-input admission bound. The later M5 0c6b4160
binary (SHA-256 4f93fd0cc2815a12d939c50ec835a65d33a688b997bfa5bce355b0248fd1233a)
visibly rejected a 1,048,608-byte input at the 1,048,576-byte limit; the same
TUI child/Shell then delivered a 301-byte paste with exact markers/digest and
no oversized work-process bytes. The card appeared only about 56 seconds after
the end marker during outer-console drain, so latency remains a performance
issue. This is chunked outer-PTY protocol input, not an OS Clipboard test.
Cancel/disconnect/backpressure remain open. Window crate Rust 1.96.1 metadata,
fmt, all-target check, six tests and strict Clippy passed in an isolated source
combination with exact ee75bec4 changed-file hashes, then again after exact
0c6b4160 changed-file hashes were added. CLI full offline metadata in that
combination lacked cached `async-broadcast` 0.7.2, so M3 does not certify
the CLI source-build closure. M5 independently owns its CLI build.

## Shared-client viewport dependency verification

The host patches above are unchanged. A separately staged CLI built from
M5 commit 232dd043eeeba78cac9e630f16c010d0b4318e4f has SHA-256
e93e5516d230c3ea4cdbbd4b2d88a87ef47cd4ffbe6bf1fcf107858c5eb95d50.
It repairs client-local reading offsets and the smaller Window's live-tail
projection when another client leads a taller inner PTY. The intermediate
candidate passed return-to-tail but lost subsequent output in the cropped
client. A retained-row geometry guard fixes that native red/green case.
M3 verified Resize, return-tail and subsequent output, mixed a real CLI with
a Window, and tested finite 16 KiB history eviction; all passed with the same
working Shell. Two-client image drawing, close-one retention and Delete
also passed. Exact-HWND synthetic wheel events are not physical-wheel or
full cursor/selection acceptance. This Window branch has not merged or
rebased onto that M5 source; keep both PRs Draft.

On the preceding candidate e3759e37, serialized Win32 record replay reaches the raw work
process for Shift/Ctrl/Alt/F6 and Chinese, and posted F6 WM_KEYDOWN succeeds.
The foreground native-key test was skipped when focus was unavailable;
the prior native-key failures remain unresolved. Non-BMP raw-reader
replacement characters and the 4,501-byte paste failure persist, while
a 301-byte paste control passes. These results add no host patch and do
not claim the shared input boundary or M3 Gate is complete.

## Private Reader search host seam

The Window-owned native Reader HWND uses the pinned Reader Core projection
search and revision-checked locate API. Ctrl+F/F3, UTF-16 WM_CHAR, a bounded
worker, source-span highlight and cancellation on refresh/close add no WezTerm
patch and no second document parser or working Terminal owner. Fixed Rust
1.96.1 Window tests, check, strict Clippy and build passed. A real HWND test
located and highlighted one synthetic source match via posted Win32 events;
foreground SendInput was blocked, so physical search input remains Pending.

## Current Window-owned input seams and outer ConPTY boundary

The Reader HWND handles WM_MOUSEHWHEEL and Shift+WM_MOUSEWHEEL with separate
partial-delta accumulators and forwards signed columns to Reader Core. It
also accepts standard WM_COPY for its existing selection. These are Window
source changes, not a fourth WezTerm adapter patch or a second Reader model.
Rust 1.96.1 MSVC fmt/check/15 tests/strict Clippy pass. Posted HWND wheel
behavior passes; physical-device input and exact Reader Clipboard copy remain
Pending. The frozen Reader Core precision API is now the public Cargo pin:
the Window maps fractional pixel/cell positions to source bytes using
`Frame::source_point_at`, checks the current paint-frame revision and clamps
dragging outside the visible viewport. The exact f926 Git dependency passed
Rust 1.96.1 Window fmt/check/15 tests/strict Clippy/build. Physical selection
and copied-source accuracy are still Pending; this is not a new WezTerm patch.

The CLI's app-local ConPTY 1.24 bundle preserves inner Kitty APC. The outer
WezTerm host retains ConPTY 1.22. With only the outer DLL pair changed, a
301-byte paste reaches the shared TUI as a 301-byte Paste event and the
work child as 314 bytes under 1.22. Outer 1.24 instead emits a mixed input
record stream: one LF is represented by four 66-byte Ctrl/Enter key records,
the old decoder leaves them opaque, and the work child receives 313 bytes.
The 1,048,608-byte overlimit run rejects exactly that size and recovers with
an exact 301-byte follow-up under 1.22; 1.24 counts 1,048,673 and the
follow-up is 300 bytes. This is a decoder/outer-input compatibility gap,
not evidence that inner image transport is broken. No default switch or
host source patch is made; M5 owns shared decoder semantics.
