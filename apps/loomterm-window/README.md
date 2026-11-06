# Loomterm Window

**Development entry; formal M3 Gate Pending.** Loomterm Window runs the complete
terminal TUI in a dedicated native Windows host and connects to the same
Loomterm Daemon as Loomterm CLI. It adds no Workspace or work-terminal owner.

This branch depends on M2 commit `56b0ae5f7b0c2e6a1c9fcc30dc313bbcabc0022b`.
M2's formal quality gate is also Pending. Publication is for source review, not
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

The preparation checks exact original Git blob hashes, applies the two-file
adapter, and writes a fixed version stamp. `--verify` checks the prepared
source. [The review patch](patches/0001-host-adapter.patch) and
[ledger](PATCHES.md) show the seam. Do not apply the patch a second time after
running preparation. No renderer, font system, multiplexer or VT rewrite.

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
./tools/stage-window.ps1 -HostSource $hostSource -HostTarget $hostTarget -CliExe ABS_LOOMTERM_EXE
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
build-produced host SHA manifest and CLI root/session without starting a Window
or Daemon. It initializes product paths through the CLI. The SHA manifest guards
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

The native fixture uses Python 3.12+ and Pillow for optional bitmap captures.
Build a bounded query child and run the exact-PID Windows fixture:

```powershell
cl /nologo /std:c17 /W4 /WX /O2 /MT tools/fixtures/window-query.c /Fe:target/window-query.exe /Fo:target/window-query.obj
python tools/fixtures/window-native.py --launcher target/loomterm-window/dev/loomterm-window.exe --host target/loomterm-window/dev/host/wezterm-gui.exe --control ABS_HOST_TARGET/x86_64-pc-windows-msvc/debug/wezterm.exe --exe ABS_LOOMTERM_EXE --shell ABS_PWSH_EXE --fixture target/window-query.exe --evidence docs/local/window-validation
```

The fixture uses a fresh short owned Root, disabled Shell profiles, explicit
binary identities, deadlines and cleanup. It records raw local evidence; do not
publish that evidence or upload it as CI artifacts. `SetWindowPos` triggers the
real native resize path; system `SC_CLOSE` uses the titlebar/system-menu close
route. `PrintWindow` screenshots are separate visual evidence candidates.
Native input is opt-in with `--input` and is skipped unless the exact owned
HWND receives foreground. Do not use input success to infer IME behavior.

| Scope | Current result |
| --- | --- |
| Full TUI, Shell I/O, Tab/Pane, CLI/Window and two-Window focus coexistence | Live-host tests passed |
| Native HWND/DPI, four physical window sizes including responsive layout, outer grid/TUI/inner PTY propagation | Passed on Windows at DPI 144 |
| Native system close, no-client XTVERSION continuation, same Daemon/Shell/Terminal reattach | Passed |
| Actual bitmap capture | Available through Win32 PrintWindow; not a blanket visual Pass |
| Repeated-resize visual stability | Long PowerShell prompt/reflow residuals observed; unresolved |
| Native key/mouse/OS Clipboard/IME and independent scrolling | Pending; foreground-guard skips and one failed opt-in Unicode round-trip attempt are retained; no native input Pass claimed |
| CJK/emoji/combining/wide-cell rendering | Chinese and composed é visible in a captured sample; Emoji absent there; full width/cursor/IME acceptance remains incomplete |
| Kitty PNG across both terminal layers | Fail: source emits APC, current inner system ConPTY preserves the text marker but drops Kitty APC before the engine/TUI |

The PNG diagnostic used the unchanged adopted PTY library and only answered
its initial CPR handshake. The synthetic child exited successfully; the raw
PTY stream lacked Kitty APC. Investigate the approved Windows ConPTY bundle/
passthrough options at both layers before changing any renderer. No common
CLI/Daemon/native-PTY semantics are changed by this Window branch.

The host GUI socket retains the upstream Windows AF_UNIX path-length limit.
A long controlled Root produced a scoped warning while TUI operation continued;
use a short test Root for host-control measurements. No personal runtime fallback.
Native fonts/DPI combinations, image resize/scroll/delete, sustained slow-client
recovery, performance and release redistribution remain separate gates.

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
`--delete` removes its image ID. A matching host screenshot is required before
claiming end-to-end image rendering. Reader work must not replace this check.

## Licenses and distribution

The launcher/test code follows the repository MIT license. `sha2` 0.10 is MIT OR
Apache-2.0. The separately acquired host is MIT; its original
[notice](../../third-party/licenses/wezterm/LICENSE.md) is retained here for the
adapter. Keep all source/native/font/MS console/ANGLE/Mesa notices with acquired
inputs and local staging. The adopted CLI retains its own Apache-2.0 license.
The local staging helper retains available source notices, but does not certify
a complete transitive binary/font redistribution inventory or installer identity.
