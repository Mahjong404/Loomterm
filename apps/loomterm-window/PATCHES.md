# Window host patch ledger

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
M2 binary still drops it. The M2 owner has since published its input fix in a
later M2 branch commit, but this Window branch remains on its fixed older base.
M3 has staged and GUI-tested a newer M2-owned CLI candidate (SHA-256 463df14d...),
not a published Window branch dependency. No automatic PR rebase is implied.
A raw ReadFile console-mode test still yields
replacement characters with verified input CP 65001 and mode 0x280; native
SendInput chord tests also have failures.
This is a partial repair, not complete native Unicode input acceptance.
Window-crate tests previously passed 4/4 with Cargo on fixed Rust 1.96.1.
Current fixed 1.96.1 rustfmt, rustc metadata check, direct clippy-driver
with -D warnings and 4/4 direct rustc tests pass against cached dependencies.
The local offline Cargo resolver currently reports no matching sha2 package,
so this source-only direct check is not a fresh cargo check/Clippy pass.
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

No fourth host patch was added for large paste. An M3-owned 256 KiB-capped raw
work-process fixture records paste delimiters, bytes and FNV-1a. In a real Window
using candidate 463df14d, 301/1,141/2,261-byte pastes pass in 0.39/2.25/8.39
seconds; 4,501 bytes produce no complete event within 15 seconds. The same
4,501-byte raw prewrapped input also stalls. The suspected growing-buffer scan
belongs to M2's shared client input framer; M3 sent its exact case and does not
patch shared CLI source concurrently. Cancel/disconnect/backpressure remain open.
