# Loomterm

Loomterm is a terminal-first workspace for local Shell and Agent CLI work. The production source in this branch is [Loomterm CLI and its dedicated Daemon](apps/loomterm-cli/README.md): the full TUI runs in an existing terminal, while one local Daemon owns the working PTYs, terminal engines and processes. Disconnecting a client does not by itself close the work.

This is an engineering integration candidate, **not a release**. Windows 11/x64 has prior interactive CLI/Daemon acceptance. Linux x86_64/glibc and macOS arm64 have passed the bounded native L1/L2 build and IPC/PTY suite on [run 42485960769](https://github.com/Mahjong404/Loomterm/actions/runs/42485960769). Distribution, unusual process and signal lifecycles, long-running performance, and full platform coverage remain open. **Loomterm Window**, a native-window entry for the same TUI/Daemon model, is developed separately in Draft PR #1; it is not in this branch.

## Build and verify

Read [CONTRIBUTING](CONTRIBUTING.md) for the fixed Rust 1.96.1 and Zig 0.16.0 inputs, Windows MSVC/SDK and Unix native requirements, and exact build and test commands. Enter the independent application workspace before direct Cargo commands; the repository root is not a Cargo workspace in this published baseline.

## Source and licensing

The adopted Herdr TUI/server source and its native vendor inputs are pinned in [UPSTREAM.json](apps/loomterm-cli/UPSTREAM.json); Loomterm changes are catalogued in [PATCHES.md](apps/loomterm-cli/PATCHES.md). The root [MIT license](LICENSE) covers Loomterm-owned material and does not relicense adopted source. See [third-party notices](THIRD_PARTY.md) and the application [Apache-2.0 license](apps/loomterm-cli/LICENSE). No installer, release archive or migration of existing Herdr/Agent data is provided here.

[Agent/repository rules](AGENTS.md) describe safe contribution boundaries. CI runs the same bounded native controller on Linux and macOS; it retains only a synthetic JSON summary.
