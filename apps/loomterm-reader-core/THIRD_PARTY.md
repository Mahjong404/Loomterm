# Reader Core dependencies

Original Loomterm code remains under the repository's MIT [license](../../LICENSE).
This independent library fixes `pulldown-cmark 0.13.6` with default features off.
[Cargo.lock](Cargo.lock) records exact registry versions and checksums; there is
no copied parser implementation, native dependency, UI or network client.
M4-B fixes three Unicode dependencies for its logical view implementation.

| Dependency | Locked version | Purpose | License and retained original texts |
| --- | --- | --- | --- |
| pulldown-cmark | 0.13.6 | CommonMark events, tables/tasks/strike/math | MIT; [license](licenses/pulldown-cmark-0.13.6/LICENSE), copyright 2015 Google Inc. |
| bitflags | 2.13.2 | Parser option flags | MIT OR Apache-2.0; [MIT](licenses/bitflags-2.13.2/LICENSE-MIT), [Apache](licenses/bitflags-2.13.2/LICENSE-APACHE) |
| memchr | 2.8.3 | Parser byte scanning | Unlicense OR MIT; [Unlicense](licenses/memchr-2.8.3/UNLICENSE), [MIT](licenses/memchr-2.8.3/LICENSE-MIT) |
| unicase | 2.10.0 | Parser reference matching | MIT OR Apache-2.0; [MIT](licenses/unicase-2.10.0/LICENSE-MIT), [Apache](licenses/unicase-2.10.0/LICENSE-APACHE) |
| unicode-segmentation | 1.13.3 | Extended grapheme boundaries | MIT OR Apache-2.0; [MIT](licenses/unicode-segmentation-1.13.3/LICENSE-MIT), [Apache](licenses/unicode-segmentation-1.13.3/LICENSE-APACHE), [copyright](licenses/unicode-segmentation-1.13.3/COPYRIGHT) |
| unicode-width | 0.2.2 | Logical CJK/emoji display widths | MIT OR Apache-2.0; [MIT](licenses/unicode-width-0.2.2/LICENSE-MIT), [Apache](licenses/unicode-width-0.2.2/LICENSE-APACHE), [copyright](licenses/unicode-width-0.2.2/COPYRIGHT) |
| unicode-linebreak | 0.1.5 | Unicode line-break opportunities | Apache-2.0; [license](licenses/unicode-linebreak-0.1.5/LICENSE) |

The parser's published Rust minimum is 1.71.1, below this module's 1.96.1 pin.
The selected feature graph has these seven platform-neutral Rust dependencies
only, with no OS-target features or native compiler requirement. A fixed release
plus lockfile avoids silent parser updates. Windows/MSVC builds and tests were
run; Linux/macOS execution is not claimed. Preserve the above original texts
with redistributed artifacts; no installer or release is produced here.

Upstream primary references: [parser API](https://docs.rs/pulldown-cmark/0.13.6/pulldown_cmark/)
and [source repository](https://github.com/raphlinus/pulldown-cmark). Future version
updates must recheck syntax behavior, limits, platform support and legal notices.

Unicode primary references: [segmentation API](https://docs.rs/unicode-segmentation/1.13.3/unicode_segmentation/),
[display-width API](https://docs.rs/unicode-width/0.2.2/unicode_width/) and
[line-break API](https://docs.rs/unicode-linebreak/0.1.5/unicode_linebreak/).
These libraries provide table-driven Rust algorithms without platform bindings;
the selected segmentation release requires Rust 1.85 and width requires 1.66,
both below the validated 1.96.1 pin. Segmentation and width use their default
features (including CJK width support); linebreak has no selected optional
features. Unicode version changes can alter layout and must receive grapheme,
CJK, emoji and source-mapping regression checks. Retained copyright files cover
generated Unicode tables. No upstream implementation files were vendored.
