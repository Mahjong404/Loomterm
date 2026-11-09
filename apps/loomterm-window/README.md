# Loomterm Window

**Development entry; formal M3 Gate Pending.** Loomterm Window runs the complete
terminal TUI in a dedicated native Windows host and connects to the same
Loomterm Daemon as Loomterm CLI. It adds no Workspace or work-terminal owner.

This branch depends on M2 commit `56b0ae5f7b0c2e6a1c9fcc30dc313bbcabc0022b`.
The M2 branch has since advanced to `9d97c1673cc54aac499d5f7680e8467d7ffb197f`;
this Window branch has not been rebased on that commit. A separate M2-owned
CLI candidate was tested inside the staged Window, but that does not update the
branch dependency or certify a build from the latest M2 HEAD.
The early Linux/macOS M2 native gate passed on 2026-11-08. This Window branch
still depends on its older fixed M2 commit, so it does not inherit that later
source or prove a rebuild against it. Publication is for source review, not
an installer, release, or claim that native GUI acceptance is complete.

## Fixed inputs and prerequisites

Use Windows 11 x64, Rust **1.96.1 MSVC**, MSVC C++ tools and Windows SDK in a
Developer PowerShell. The host's OpenSSL build needs native Windows Perl
(e.g. Strawberry Perl 5.40.2.1 portable). Put its `perl/bin` on this process's
PATH. Preserve ordinary Shell HOME/USERPROFILE/tool storage. Building the CLI
from source additionally needs **Zig 0.16.0** and its documented native SDK/libc
inputs; no private prebuilt engine library is required by the source route.

The host source pin and native gitlinks are in [UPSTREAM.json](UPSTREAM.json).
Acquire them into a separate build-owned directory. For example, from the
repository root (Git commands below create only that independent input copy):

```powershell
$env:RUSTUP_TOOLCHAIN = '1.96.1-x86_64-pc-windows-msvc'
$hostSource = Join-Path $PWD 'target/window-host-source'
$hostTarget = Join-Path $PWD 'target/window-host-build'
git clone --filter=blob:none --no-checkout --config core.autocrlf=false https://github.com/wezterm/wezterm.git $hostSource
git -C $hostSource -c core.autocrlf=false checkout --detach e2a98e1b8af156988ad0ac778658ac27b60a55fb
git -C $hostSource -c core.autocrlf=false submodule update --init --recursive
python tools/prepare-window-host.py $hostSource
./tools/build-window.ps1 -HostSource $hostSource -HostTarget $hostTarget -Controller
```

The preparation checks four exact original Git blob hashes, applies the
Window adapter and writes a fixed version stamp. `--verify` checks a prepared
copy. An exact adapter-v1 copy can use `--upgrade-input` once; a pristine copy
uses the normal command above. Review patches
[1](patches/0001-host-adapter.patch),
[2](patches/0002-windows-utf16-input.patch),
[3](patches/0003-win32-utf16-records.patch) and the [ledger](PATCHES.md)
show every seam. Do not apply the patches again after preparation.
No renderer, font system, multiplexer or VT rewrite.

Build the CLI in its independent workspace so Cargo selects its configuration:

```powershell
Push-Location apps/loomterm-cli
cargo +1.96.1-x86_64-pc-windows-msvc build --locked --target x86_64-pc-windows-msvc
Pop-Location
```

Use the actual CLI output path (including any explicit CARGO_TARGET_DIR) when
staging. No binaries, native source snapshot, build cache or user configuration
are checked into the Window change.

## Run without another terminal

```powershell
$conpty = Get-Content apps/loomterm-window/conpty.json -Raw | ConvertFrom-Json
$package = Join-Path $PWD 'target/window-inputs/Microsoft.Windows.Console.ConPTY.1.24.260710001.nupkg'
New-Item -ItemType Directory -Force (Split-Path $package) | Out-Null
Invoke-WebRequest -Uri $conpty.package.url -OutFile $package
./tools/stage-window.ps1 -HostSource $hostSource -HostTarget $hostTarget -CliExe ABS_LOOMTERM_EXE -ConPtyPackage $package
```

Open `target/loomterm-window/dev/loomterm-window.exe` directly in Explorer. The
adjacent CLI and `host/wezterm-gui.exe` are discovered automatically. The default
Product Root is `%LOCALAPPDATA%/Loomterm` and session is `default`. CLI and Window
must select the same Root/session to share live resources.

For controlled tests or an explicit Source Folder, use a Windows shortcut or:

```powershell
./target/loomterm-window/dev/loomterm-window.exe --root ABS_PRODUCT_ROOT --cwd ABS_SOURCE_FOLDER --session window-test
```

`--host` and `--cli` override adjacent binary paths. `--check` validates the
build-produced host SHA manifest, adjacent ConPTY bundle presence and CLI
root/session without starting a Window or Daemon. It initializes product paths
through the CLI. The SHA manifest guards
accidental binary replacement; it is not an authenticated installer/signature.

The launcher writes per-process internal Lua under `ProductRoot/window`, waits
for the host and removes that Lua on exit. Host data/plugins, cache/blobs and
runtime/logs/socket paths stay under that directory. Inherited `WEZTERM_*`
selectors are removed before startup. No personal WezTerm Lua is loaded.
A pre-GUI startup failure leaves a PID-named `loomterm-window-error` log in OS
temp. This is path separation, not an OS sandbox.

Ctrl+Shift+C/V are configured for host Clipboard copy/paste; other default host
key bindings are disabled. IME is enabled. These settings are not themselves
proof of native input acceptance. Closing the host releases its outer TUI;
the launcher sends no work-Pane close or Daemon stop request.

## Tests and current verdicts

```powershell
Push-Location apps/loomterm-window
cargo test --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
Pop-Location
python -m unittest discover -s tools/fixtures -p 'test_window_*.py' -v
```

Run these from the pinned Rust 1.96.1 workspace and set
`WINDOW_CONPTY_PACKAGE` to the verified fixed package before the Python suite
to exercise its real staging case. With the existing fixed offline cache and
an isolated Window target directory, Cargo metadata, fmt, all-target check,
six crate tests and all-target Clippy with `-D warnings` pass on Rust 1.96.1
MSVC. The Python suite passes 9/9, including the real fixed-package case.
The earlier default-cache `sha2` resolution failure is retained as an
environment result; it is no longer the current Window Cargo check result.

The native fixture uses Python 3.12+ and Pillow for optional bitmap captures.
Build a bounded query child and run the exact-PID Windows fixture:

```powershell
cl /nologo /std:c17 /W4 /WX /O2 /MT tools/fixtures/window-query.c /Fe:target/window-query.exe /Fo:target/window-query.obj
python tools/fixtures/window-native.py --launcher target/loomterm-window/dev/loomterm-window.exe --host target/loomterm-window/dev/host/wezterm-gui.exe --control ABS_HOST_TARGET/x86_64-pc-windows-msvc/debug/wezterm.exe --exe ABS_LOOMTERM_EXE --shell ABS_PWSH_EXE --fixture target/window-query.exe --evidence docs/local/window-validation
python tools/fixtures/window-image-native.py --launcher target/loomterm-window/dev/loomterm-window.exe --host target/loomterm-window/dev/host/wezterm-gui.exe --control ABS_HOST_TARGET/x86_64-pc-windows-msvc/debug/wezterm.exe --exe target/loomterm-window/dev/loomterm.exe --shell ABS_PWSH_EXE --fixture tools/fixtures/window-image.py --evidence docs/local/window-validation
```

The fixture uses a fresh short owned Root, disabled Shell profiles, explicit
binary identities, deadlines and cleanup. It records raw local evidence; do not
publish that evidence or upload it as CI artifacts. `SetWindowPos` triggers the
real native resize path; system `SC_CLOSE` uses the titlebar/system-menu close
route. `PrintWindow` screenshots are separate visual evidence candidates.
Native input is opt-in with `--input` and is skipped unless the exact owned
HWND receives foreground. Do not use input success to infer IME behavior.
The image fixture requires the staged CLI beside its fixed ConPTY bundle,
checks the backend's actually loaded DLL, and captures only its owned Window.
It certifies a single checker PNG's stated assertions. To inspect work-process
UTF-8 output independently of text entry, run `tools/fixtures/window-text.py`
inside the work Pane. `window-input.c` is a separate bounded raw-byte fixture;
its output does not establish candidate-window or font correctness.

| Scope | Current result |
| --- | --- |
| Full TUI, Shell I/O, Tab/Pane, CLI/Window and two-Window focus coexistence | Live-host tests passed |
| Native HWND/DPI, four physical window sizes including responsive layout, outer grid/TUI/inner PTY propagation | Passed on Windows at DPI 144 |
| Native system close, no-client XTVERSION continuation, same Daemon/Shell/Terminal reattach | Passed |
| Client reading isolation with the M5 dependency candidate | Native two-Window Resize/return-tail and subsequent live output, actual CLI/Window mixed reading and 16 KiB history eviction pass on the exact candidate below. Synthetic wheel events target the owned HWND; physical wheel/selection remain Pending. This Window branch has not integrated M5 source |
| Actual bitmap capture | Available through Win32 PrintWindow; not a blanket visual Pass |
| Repeated-resize visual stability | Five native sizes retain the long prompt in inner/outer text; inspected candidate screenshots show no obvious residual. Prior residual is retained; wider visual/cursor acceptance remains Pending |
| Native keyboard and raw-console compatibility | On the exact M5 CLI candidate below, a maintainer physically entered Shift+X, Ctrl+K, Alt+X and F6 through the focused Window; the raw work process received `58 0b 1b 78 1b 5b 31 37 7e`, and the same Shell remained attached. Host/TUI key tracing is opt-in. This passes only that chord sequence; arrows, Home/End, PageUp/PageDown, other function keys and physical mouse input remain Pending. The separate raw-console non-BMP ReadFile fixture still yields four replacement characters at input CP 65001/mode 0x280; PowerShell WM_CHAR Unicode and manual IME/glyph acceptance are separate paths |
| IME, OS Clipboard and mouse titlebar close | Maintainer reports preedit/candidates, Esc cancel, Chinese commit, Shift-drag selection, Ctrl+Shift+C/V copy/paste, and titlebar X normal in the fixed local candidate. Same Shell reattach after X is verified. Real-Window bracketed paste passes exact markers/digest through 2,261 bytes but becomes slow with size; 4,501 bytes did not reach the raw work process within 15 seconds. Cancel/disconnect and physical mouse-wheel/selection remain open |
| CJK/emoji/combining/wide-cell rendering | CJK, combining text, Emoji and ZWJ are visible; engine cell-width probes pass. Maintainer confirms CJK/Emoji glyphs in the manual case. Multi-font/DPI and full cursor/layout acceptance remain incomplete |
| Kitty PNG across both terminal layers | Fixed Microsoft ConPTY 1.24.260710001 preserves the exact APC. The 32x32 checker displays, resizes, scrolls out, redraws, replaces at the same ID/position and repeats; truncated replacement and a decoded image above 64 MiB do not draw. Two Windows each render it; closing one leaves the other and its Shell intact. Five further display/delete cycles kept host handles at 1,040–1,043 after initial allocation and backend handles at 188 in one run; these process-level samples do not count image/GPU resources or prove reclamation. Visible error feedback and exact resource reclamation remain Pending |

The current system ConPTY drops PNG APC even with passthrough; the fixed
package in [conpty.json](conpty.json) preserves it through the CLI's existing
production loader. Staging verifies the package, three native files and legal
notices, keeps the bundle adjacent to the CLI and never replaces system DLLs.
The Window launcher checks that all four adjacent bundle files are present
before opening the host and removes the inherited legacy system-ConPTY override.
The production CLI loader verifies exact file hashes. A missing bundle therefore
fails Window startup explicitly instead of silently losing Kitty images. This
does not alter an already-running Daemon that was started separately with a
different ConPTY selection. Only Windows 11 has been exercised; the minimum OS
for the separately distributed DLL and complete redistribution inventory remain
unverified release checks.
No common CLI/Daemon/native-PTY source is changed by this Window branch.
The standalone PTY probe answers startup CPR only; separate live Window tests
establish actual image rendering. One image does not prove all lifecycle cases.

Earlier input results used M2-owned candidates `40069dd7` and
`463df14d6c4ab776ee05c8bea029c3908aa9aa312e9b34d89e7a900b0032238c`.
The latest shared-client candidate tested in the Window has SHA-256
`e93e5516d230c3ea4cdbbd4b2d88a87ef47cd4ffbe6bf1fcf107858c5eb95d50`
and source commit `232dd043eeeba78cac9e630f16c010d0b4318e4f` in
[M5 Draft PR #3](https://github.com/Mahjong404/Loomterm/pull/3).
It passes native two-Window Resize/return-to-tail and subsequent new output,
mixed CLI/Window reading and finite 16 KiB history eviction. Each test retains one Daemon and
the same working Shell. Two Windows each render the image; closing A leaves
B's image and Shell intact, and Delete clears B's image. Wheel events target
the owned HWND and Pane; these results do not certify physical mouse behavior
or every cursor/selection layout. The candidate binary is not published here,
and this Window branch has not merged or rebased onto the M2/M5 source.

With the preceding M5 candidate e3759e37, replaying serialized Win32 records through the
outer PTY reaches the raw work process for ASCII, Shift, Ctrl, Alt, F6 and
Chinese. Posting F6 WM_KEYDOWN and text WM_CHAR messages to the owned HWND
also reaches it. These controls bypass foreground keyboard injection and
do not establish actual modifier or IME behavior. Supplementary Emoji still
becomes replacement characters at the raw-reader boundary; the previously
accepted PowerShell string and manual IME/glyph scopes remain distinct.

The host GUI socket retains the upstream Windows AF_UNIX path-length limit.
A long controlled Root produced a scoped warning while TUI operation continued;
use a short test Root for host-control measurements. No personal runtime fallback.
Native fonts/DPI combinations, visible malformed/oversized image errors,
bounded render-resource accounting, sustained slow-client recovery, performance
and release redistribution remain separate gates. The earlier shared scroll
failure and smaller-client tail clipping after Resize are fixed within the
tested M5 candidate scope above. The shared CLI/Daemon retains scrollback
ownership while ClientShell reading positions are private; no Window host
patch or second Terminal Owner was added. A small bracketed paste reached a raw work process with
the exact start/end markers and Chinese/control-text payload; newline was
normalized to CRLF by inner ConPTY. A bounded raw-reader fixture now checks
larger paste length and FNV-1a digest through the complete Window path: 301,
1,141 and 2,261 bytes passed, taking 0.39, 2.25 and 8.39 seconds respectively.
A 4,501-byte paste returned success from the host control command but the
reader received zero bytes before its 15-second watchdog; a raw prewrapped
write behaved likewise. Latency rises sharply across these sizes, but the
exact bottleneck and layer remain under M2-owned CLI diagnosis. Cancelled/disconnected
pastes remain Pending.
The preceding M5 candidate e3759e37 repeats the 4,501-byte failure (zero raw-reader bytes
before the watchdog), while a 301-byte control passes exact delimiters and
digest in 0.375 seconds. The newer M5 commit ee75bec4 was separately staged
with exact CLI SHA-256 1bdef492e5a0b900718693ffdf2e123baa35a4244d5dc2f99bdde9920c64945.
In a real Window with a prestarted Daemon under the same isolated Product Root,
301, 4,501, 16,379, 29,771 and 262,521 wire bytes reached a raw work process
with exact bracket markers and FNV-1a digests. The 262,521-byte case took 15.84
seconds. Control input was chunked into the outer PTY between explicit bracket
markers; this verifies the Window/TUI/Daemon path, not OS Clipboard or a single
host paste command. An ee75 1,225,021-byte attempt returned no work-process
bytes before the 180-second watchdog. The subsequent M5 commit 0c6b4160
(CLI SHA-256 4f93fd0cc2815a12d939c50ec835a65d33a688b997bfa5bce355b0248fd1233a)
keeps the same Window TUI child and Shell attached after a 1,048,608-byte
over-limit input. The outer grid displayed an exact 1,048,576-byte-limit
rejection; a following 301-byte paste reached the work process with exact
markers/digest and no oversized bytes. The card appeared about 56 seconds
after the end marker because the outer console was still draining input. This
is bounded functional recovery, not acceptable latency or a total input-memory
budget. The test runner's separate cold-start case exited before a visible
HWND; it does not overturn earlier cold-start acceptance. Cancellation,
disconnect/backpressure and the shared CLI source-build closure remain Pending.
An isolated source combination of M3, exact M5 0c6b4160 changed-file hashes
and M4 Reader 8eaed290 passed Window crate Rust 1.96.1 metadata, fmt,
all-target check, seven tests and strict Clippy at that checkpoint. The full CLI offline metadata
step lacks cached `async-broadcast` 0.7.2; M5 owns its independent CLI build.

## Reader native window (developer entry)

The Window crate pins Reader Core revision
[`7e11dcca`](https://github.com/Mahjong404/Loomterm/tree/7e11dcca3c59deb87545c75ec4f387f95e09004e/apps/loomterm-reader-core)
from M4. `ReaderHost` owns one logical preview slot per Window client, bounds
loading to one active worker plus one replaceable pending request, rejects stale
generations, forwards viewport/input changes and resource-size/failure callbacks,
and requires the renderer to recheck the current-frame lease before painting.
The Windows launcher opens a separate native Reader HWND for an explicit
absolute local source path while retaining the terminal host:

```powershell
./target/loomterm-window/dev/loomterm-window.exe --reader-file C:\absolute\notes.md
```

This developer entry paints Reader Core text clusters and local PNG/JPEG/WebP,
advances bounded pending work, maps DPI/resize to its private viewport, handles
Reader scroll/navigation and Clipboard copy, and closes without stopping the
host, Daemon or work Shell. Image decode uses one worker and at most 16 pending
visible requests per HWND, the Core's 4-million-pixel per-image limit and a
16 MiB aggregate decoded-image cache. The GDI surface composites RGBA onto
white, paints top-down BGRX and releases the cache on Reader close/reopen.
There is one Reader HWND per launcher and one ReaderHost slot. No TUI command
opens it yet. The native GDI surface uses Reader Core's PaintStyle roles for
heading weight/color, emphasis, strike, code/quote/table backgrounds, selection
and focused links. Sixteen fixed font variants are recreated on DPI changes and
released with the HWND; Tab/Shift+Tab traverses Reader links without opening
external targets. Search UI, formulas, wide-character right-half selection and
physical-device wheel remain Pending. F5 explicitly reopens the same Markdown so
a changed local image refreshes even when the text did not change; it resets
Reader's scroll position. The exact fixed Reader commit was fetched once into
an isolated M3 repository and checked by full SHA; no shared branch moved.
The terminal Window and its M3 acceptance remain independent. Reader Core's MIT license and
dependency notices remain with that exact source revision; no Reader binaries
or source snapshot are copied into this branch.

The private native Win32 fixture opens the actual Reader HWND, captures painted
Markdown, resizes and sends a bounded wheel event, closes Reader via SC_CLOSE,
and verifies terminal, Shell and Daemon survival. A missing-source case paints
the Core error. A native image case counted 684 red, 684 blue and 866 blended
gray pixels; same-document F5 replacement showed 684 green and zero old red.
Truncated and over-limit PNGs removed the old colors and showed error/limit
placeholders. On the fixed 7e11 Core, a fresh image case changed launcher GDI
objects from 30 to 14 on Reader close while the terminal and Shell remained.
A style case measured 284 heading-color pixels, 33,601 code-background pixels,
15,710 table-header pixels and 1,291 focused-link pixels after Tab; captured
frames also show emphasis and strike. One foreground-guarded SendInput wheel
changed Reader pixels, while a separate attempt was blocked by OS foreground
protection. Hardware wheel and full selection/cursor visuals remain Pending.

## Short manual completion check

1. Focus the test Window, resize smaller/larger, then check a fresh grid and cursor
   against the measured inner dimensions. Record both geometry and screenshot.
2. Type English and Chinese; observe IME preedit, candidate placement, Esc cancel
   and exactly one commit. Check emoji, combining marks and wide-character cursor.
3. Click/scroll between two Panes; test Ctrl/Alt/Shift/function keys. Select/copy
   `M3_CLIP_TEST`, paste with Ctrl+Shift+V and verify exact content/target.
4. Click the titlebar X and reopen. Check the same live work identity. Retain
   observations as Pass/Fail/Pending; do not infer from logs or upstream features.

The deterministic image input is `tools/fixtures/window-image.py`. Run it inside
an actual work Pane to emit a 32x32 Kitty PNG; `--output` writes the source PNG,
`--replace` repeats the image ID with alternate pixels, `--invalid` emits a truncated
replacement, `--overbudget` emits a compact PNG above the 64 MiB decoded-image
budget, and `--delete` removes its image ID. A matching host screenshot is required before
claiming end-to-end image rendering. Reader work must not replace this check.

## Licenses and distribution

The launcher/test code follows the repository MIT license. `sha2` 0.10 is MIT OR
Apache-2.0. The separately acquired host is MIT; its original
[notice](../../third-party/licenses/wezterm/LICENSE.md) is retained here for the
adapter. Keep all source/native/font/MS console/ANGLE/Mesa notices with acquired
inputs and local staging. The adopted CLI retains its own Apache-2.0 license.
The app-local Microsoft ConPTY package is MIT; retain its
[license](../../third-party/licenses/microsoft-conpty/LICENSE.txt) and
[NOTICE](../../third-party/licenses/microsoft-conpty/NOTICE.md) alongside it.
The local staging helper retains available source notices, but does not certify
a complete transitive binary/font redistribution inventory or installer identity.
