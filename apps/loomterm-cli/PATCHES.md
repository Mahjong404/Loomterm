# Loomterm adoption patches

This independent workspace adopts the original TUI/server recorded in
[UPSTREAM.json](UPSTREAM.json). The root workspace remains the diagnostic and
regression foundation. Apache-2.0 applies to the adopted code; the repository's
original MIT license is unchanged. This is a source integration, not a release.

| Patch group | Source locations | Purpose and maintenance surface |
| --- | --- | --- |
| Product identity | `src/product.rs`, `config`, `session`, `ipc`, `logging`, `main.rs`, `client/mod.rs` | Dedicated product directories and endpoint hashes; explicit command/API policy; no original-product config, automatic Agent discovery, updater, plugin execution or remote endpoints |
| Startup and identity | `src/platform/instance_lock.rs`, `server/autodetect`, `server/bootstrap`, API capabilities | Startup lock through both endpoints becoming ready, lifetime owner lock, product/namespace/boot identity before attaching |
| Unix isolation preparation | `src/platform/macos/launchd_spawn.rs` | Use the Loomterm label/temp prefix so stale-job selection cannot target original Herdr. macOS launchd/bootstrap execution and independent-Root cleanup remain native M2 gates |
| M2 Unix isolation guards | `src/platform/{unix_paths,launch_namespace}.rs`, `product.rs`, `ipc.rs`, macOS launchd | Validate native socket byte limits before side effects; reject non-socket/symlink/foreign-owned endpoints; private product directories must be owned real directories; scope stale launchd cleanup by runtime Root. Native socket 0600/directory 0700 checks and dual-endpoint lifecycle passed on Linux x64 and macOS arm64; special launchd execution remains Pending |
| M2 native test entry | `tests/loomterm_native.rs`, repository `tools/test-unix-native.py` and Unix fixtures | Reuse the real outer PTY/TUI host on Unix, ordinary direct-child SIGTERM cleanup, bounded XTVERSION/resize/detach controller; target-native lifecycle/concurrent cold-start and finite exit/Source cases passed on both platforms. No production fixture entry or extra Terminal Owner; remaining signal/descriptor/slow-client/launchd limits stay explicit |
| M2 native compile closure | `src/update.rs`, Unix-only transport test import, endpoint JSON test assets | Reuse the existing selected-product client endpoint resolver in an inherited Unix compile path; updater/handoff remains disabled by entry policy. Include concat-based compile-time JSON fixtures in source delivery. Remove the observed redundant Unix test trait import; native check/build/link and engine tests passed on both targets; the pinned Clippy baseline is tracked in the helper patch below |
| M2 pinned Clippy helper | `crates/ghostty-vt/src/lib.rs` | Group width/height into one size argument in an existing private helper and its two callers. Preserve engine/native ABI, pins, image behavior and enforced Clippy rules |
| M2 lint configuration delivery | `clippy.toml`, repository native test entry | Include the byte-identical pinned upstream configuration (argument threshold 11) in source delivery. Fail before building if absent/changed; check the pinned driver with eleven-argument acceptance and twelve-argument rejection. Keep strict workspace `-D warnings`; no new lint exemptions or function refactors |
| Shared input and paste framing | `src/client/input/windows_vti.rs`, `src/client/{events,mod}.rs`, `src/protocol/wire.rs`, `src/raw_input.rs`, `src/server/{client_transport,pane_input}.rs` | Separate nested Win32 UTF-16 state from ASCII wrappers; completed synthetic surrogate pairs become TextCommit. Scan incomplete bracketed-paste payloads incrementally, including idle checks, so each newly complete delimiter position is checked once. Preserve a completed paste as one client event and one ordered PTY queue item with both markers. Share the 1 MiB payload threshold between client and server; account for event metadata in the bounded Windows handoff. An over-limit single Paste is an ordered local rejected-action notice, followed by usable input on the same client; true queue exhaustion still detaches visibly. Focused 301/4,501/16 KiB framing, 1.225 MiB rejection/recovery, and 16 KiB ordered PTY-queue tests cover UTF-8 splits, newline and control text. Keep exact native Window rejection/recovery and Unix M5 results pending until their gates run. No Window-host, vendor or wire-format change |
| Windows input API control | `src/terminal/runtime/windows_input_tests.rs`, repository `tools/fixtures/cli-input.{c,py}` | Opt-in bounded production-runtime control, no client/Shell/Agent. Verify actual CP65001/mode0x280, loaded ConPTY and process identity. Direct UTF-8 reproduces non-BMP replacement in ConPTY 1.24 ReadFile while wide/record APIs retain the pair. This remains a compatibility failure, not a successful conversion test or a reason to rewrite the PTY actor |
| M5 ClientShell viewport | `src/server/{clients,headless,client_shell,render_stream,pane_input}.rs`, `src/server/headless/{client_views,endpoint_requests,render,retained_surface}.rs`, `src/{pane,kitty_graphics}.rs`, `src/pane/terminal.rs`, `src/terminal/runtime.rs`, `src/app/api/panes.rs`, `src/server/{client_shell_graphics,render_scale_benchmark}.rs`, `src/server/headless/tests/{mod,surface_scroll}.rs`, `src/ui/{panes,tab_surface,scrollbar}.rs` | Store non-tail offsets per ClientShell connection and Pane; project text, cursor, scrollbar and Kitty placements from one shared engine under a serialized viewport swap with restoration on unwind. A smaller non-leading client crops the live tail and adjusts its private scroll range, cursor, links and images without resizing the shared PTY. Client detach/Pane close releases local state; retained dirty rows fall back to full projection for history readers. No wire change, second Terminal Owner or Window-host input change. Scoped native two-Window resize/return-tail and mixed CLI/Window acceptance passed; full M5 interaction remains pending |
| Stable project | `workspace`, `app/creation`, `app/api`, `persist`, `server/headless/notifications` | Stable Source Folder/name/identity, metadata-only cold restoration; centralized single Shell refill when clients exist |
| Bounded I/O | `src/pty`, `pane`, `api/server`, `server/client_transport`, `client/{transport,endpoint/writer,notifications}`, `protocol/{wire,surface_reuse,surface_delta/decode}` | Admission held through actual consumption; separate VT replies; bounded connections, client-frame admission and write deadlines; geometry checked before native/render allocation; explicit failure instead of silent input loss |
| Native reuse | `crates/ghostty-vt/build.rs`, its manifest, `Cargo.lock` | Optional explicit trusted artifact manifest verifies target, revision, library, headers and bindings. Otherwise retains the pinned source/Zig build |
| Product packaging boundary | `Cargo.toml`, `build.rs`, tests | Own package/binary, no publishing; upstream integration tests point at the adopted binary. Existing vendor patches remain unchanged |

The vendor Ghostty revision and its pre-existing patches are inherited intact,
including `vendor/libghostty-vt.vendor.json`. No header or Rust binding is
regenerated by M1. `vendor/portable-pty` remains the original patched 0.9.0
source, rather than replacing it with crates.io's identically numbered package.
Source-level upstream names and historical documentation remain attribution
and maintenance context; they are not additional supported product entries.

Production changes do not adopt B2's fixture-only entry, fixed case Root,
deadline shutdown or permanent Shell-refill suppression. The full client still
uses the original terminal-first interaction and one daemon-owned
TerminalRuntimeRegistry. Experimental fixtures live outside production entry
selection. Native test results are maintained in the repository platform and
lifecycle documents; fixture compilation alone is not interactive acceptance.

For an upstream update, compare against the exact source inventory, retain
licenses/vendor changes, and review these groups individually. Do not switch
to the separate Herdr App/Core architecture or replace the engine implicitly.
