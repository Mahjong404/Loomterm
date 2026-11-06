# Window host patch ledger

The adapter targets the fixed revision in [UPSTREAM.json](UPSTREAM.json).
It checks the original Git blob hashes before writing either file. Source
acquisition is separate; run it once on a pristine, independent source copy.
The upstream MIT notice and native/font licenses remain with that copy.

| File | Change | Reason |
| --- | --- | --- |
| `config/src/config.rs` | Replace three root resolvers with required absolute `LOOMTERM_WINDOW_ROOT` children | Isolate data/plugins, cache/blobs, runtime/logs/GUI sockets without changing Shell HOME |
| `wezterm-gui/src/main.rs` | Early adapter probe, required root and distinct Windows AppUserModelID | Reject accidental upstream launch; separate taskbar identity |

Preparation also writes a fixed `.tag` version stamp. The build helper verifies
the two resulting source SHA-256 values before generating a binary manifest.
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
third-party asset. Native image/IME/Clipboard and visual blockers remain explicit.
