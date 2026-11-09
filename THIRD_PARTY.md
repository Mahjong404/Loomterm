# Third-party source and notices

The repository's original [MIT license](LICENSE) remains unchanged. It does not relicense the adopted CLI/Daemon source or assets. This is a source integration candidate, not a release package; any future binary distribution must inventory its exact linked Rust/native dependencies and include applicable notices.

| Component | Fixed source / purpose | License and retained notice |
| --- | --- | --- |
| Original Herdr TUI/server | [UPSTREAM.json](apps/loomterm-cli/UPSTREAM.json), revision ee5bfb5041bb3e4a49d591c72308aae3c81ecdde | [Apache-2.0](apps/loomterm-cli/LICENSE); original copyright and modification record in [PATCHES.md](apps/loomterm-cli/PATCHES.md) |
| Vendored libghostty-vt | Ghostty revision 44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51, plus recorded upstream patches | [MIT](apps/loomterm-cli/vendor/libghostty-vt/LICENSE) and original vendor notices |
| Vendored portable-pty 0.9.0 | Patched dependency retained in the application workspace | [MIT](apps/loomterm-cli/vendor/portable-pty/LICENSE.md) and original source notices |
| Fira Code Regular | [font asset](apps/loomterm-cli/vendor/libghostty-vt/pkg/freetype/res/FiraCode-Regular.ttf), 289,624 bytes, SHA-256 5992ab9640e2df491b2f609467b1de60e8bc39b2c28db184342a0592d98f6117 | [SIL Open Font License 1.1, complete original text](third-party/licenses/fira-code/LICENSE), Copyright (c) 2014 The Fira Code Project Authors |

Fira Code's original license is copied byte-for-byte from FiraCode tag 6.2 (commit eee6db993696aba61ff4eef03698e2987d79910c, Git blob 805e0b38b5cfbf78c7269d42ef7096699eec6559). The font embeds family name Fira Code, version 6.002 and a 2014–2021 author copyright notice. Its original license names no specific Reserved Font Name in the copyright declaration; the OFL's general Reserved Font Name definition and conditions still apply. Preserve the full license with the font in any future source or binary distribution.

The [Cargo.lock](apps/loomterm-cli/Cargo.lock) fixes registry dependency versions; registry crates are downloaded under their own licenses during build. The checked-in native vendor tree retains additional original license/notice files. Zig and platform SDKs are development inputs, not copied into this source tree. Packaging and release require a separate exact artifact and transitive-license check. The Loomterm Window host and its distribution notices are outside this branch.
