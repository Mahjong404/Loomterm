# Loomterm CLI and Daemon

This independent Cargo workspace contains the full interactive terminal client and dedicated local Daemon. The command binary is loomterm. Run direct Cargo commands from this directory to select the pinned Rust 1.96.1 toolchain, Cargo configuration and lockfile; the repository root is not this workspace.

~~~sh
cargo build --locked
cargo run --locked -- --check
cargo run --locked
~~~

The last command opens the full TUI in an existing interactive terminal and connects to or starts the product's dedicated Daemon. A local Shell Pane can be created in a Workspace. A normal client exit detaches; working terminals remain owned by the Daemon. Do not treat the --check command as a runtime or native PTY acceptance test.

Product config, state, cache, logs and IPC use Loomterm-specific paths. LOOMTERM_ROOT can select an explicit absolute product root and --session selects a namespace; invalid or foreign roots are rejected. Normal Shell HOME, USERPROFILE, APPDATA, XDG and Agent storage are preserved. No existing Herdr/Agent data migration, automatic Agent resume, plugin installation, updater or remote launch is provided in this baseline.

See [CONTRIBUTING](../../CONTRIBUTING.md) for Windows MSVC and native Linux/macOS build and test entry points. The original source revision, vendored Ghostty input and file hashes are in [UPSTREAM.json](UPSTREAM.json); modifications are in [PATCHES.md](PATCHES.md). Retain this workspace's [Apache-2.0 license](LICENSE), vendor notices and the repository [third-party inventory](../../THIRD_PARTY.md). Historical upstream distribution scripts in this tree are not Loomterm installers.
