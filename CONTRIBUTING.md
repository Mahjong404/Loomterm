# Contributing to Loomterm

Start with the [project status](README.md), [production workspace guide](apps/loomterm-cli/README.md), [source inventory](apps/loomterm-cli/UPSTREAM.json), [patch ledger](apps/loomterm-cli/PATCHES.md), and [agent rules](AGENTS.md). This published branch contains the independent production CLI/Daemon workspace at apps/loomterm-cli. Work on the fixed source and report which platform and actual OS resources you tested.

## Toolchains

Use Rust/Cargo **1.96.1** with rustfmt and Clippy, Zig **0.16.0**, Python 3, a native C compiler/linker and target SDK. The application pins Rust in its own rust-toolchain.toml and locks Rust dependencies in Cargo.lock. Its clippy.toml is the unmodified upstream lint configuration. Run Cargo from apps/loomterm-cli so it discovers both the toolchain and .cargo/config.toml.

On Windows use x86_64-pc-windows-msvc, Visual Studio C++ Build Tools and Windows SDK, PowerShell 7 and cl.exe. On Linux use x86_64/glibc with the native C compiler/linker; on macOS use Apple Silicon arm64 with Xcode command-line tools. Zig 0.16.0 archive URLs and platform-specific SHA-256 values are in the [native workflow](.github/workflows/cli-native.yml). Do not substitute the older diagnostic Ghostty engine for this workspace's pinned vendor source.

## Build and check

From a Windows Developer PowerShell, with Rust/Zig and SDK available:

~~~powershell
pwsh ./tools/build-cli.ps1 -Check
pwsh ./tools/build-cli.ps1
~~~

The helper selects apps/loomterm-cli and accepts -Test with a focused -Filter. Add -Offline only after dependencies are cached. To run the bounded **real** Windows ConPTY/CLI regression, use a controlled test account/environment with Python 3, PowerShell 7 and cl.exe:

~~~powershell
pwsh ./tools/test-cli.ps1 -Suite core
~~~

That controller starts a synthetic Shell/fixture and private product namespace, writes evidence under ignored docs/local/ and does not run an Agent. Review its parameters before using another evidence directory.

On Linux x86_64/glibc or macOS arm64, enter the application directory for direct Cargo work:

~~~sh
cd apps/loomterm-cli
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cd ../..
python3 -B tools/test-unix-native.py --evidence /path/to/private/test-evidence
~~~

The final command is a native IPC/PTY lifecycle controller, not a cross-compile or pipe echo. Run it only on a disposable test environment with a finite evidence directory. The [workflow](.github/workflows/cli-native.yml) provisions the fixed toolchains and has a 45-minute job limit. It uploads only ci-summary.json; raw VT/session output and caches stay private.

## Change review

Keep the Daemon's TerminalRuntimeRegistry as the only owner of working PTYs, parsers and processes. Preserve product namespace isolation, bounded I/O, the adopted headers/bindings and all source/patch/license records. Make tests exercise the changed behavior, including native resources where relevant. Do not place private design notes, real Agent history, local paths, secrets, terminal captures or build outputs in a contribution. Review explicit file paths and [third-party attribution](THIRD_PARTY.md) before proposing a PR.
