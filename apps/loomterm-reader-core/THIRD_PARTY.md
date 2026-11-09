# Reader Core dependencies

Original Loomterm code remains under the repository's MIT [license](../../LICENSE).
This independent library fixes `pulldown-cmark 0.13.6` with default features off.
[Cargo.lock](Cargo.lock) records exact registry versions and checksums; there is
no copied parser implementation, native dependency, UI or network client.
M4-B fixes three Unicode dependencies for its logical view implementation;
M4-C fixes `image 0.25.10` with default features off and PNG/JPEG/WebP only.

| Dependency | Locked version | Purpose | License and retained original texts |
| --- | --- | --- | --- |
| pulldown-cmark | 0.13.6 | CommonMark events, tables/tasks/strike/math | MIT; [license](licenses/pulldown-cmark-0.13.6/LICENSE), copyright 2015 Google Inc. |
| bitflags | 2.13.2 | Parser option flags | MIT OR Apache-2.0; [MIT](licenses/bitflags-2.13.2/LICENSE-MIT), [Apache](licenses/bitflags-2.13.2/LICENSE-APACHE) |
| memchr | 2.8.3 | Parser byte scanning | Unlicense OR MIT; [Unlicense](licenses/memchr-2.8.3/UNLICENSE), [MIT](licenses/memchr-2.8.3/LICENSE-MIT) |
| unicase | 2.10.0 | Parser reference matching | MIT OR Apache-2.0; [MIT](licenses/unicase-2.10.0/LICENSE-MIT), [Apache](licenses/unicase-2.10.0/LICENSE-APACHE) |
| unicode-segmentation | 1.13.3 | Extended grapheme boundaries | MIT OR Apache-2.0; [MIT](licenses/unicode-segmentation-1.13.3/LICENSE-MIT), [Apache](licenses/unicode-segmentation-1.13.3/LICENSE-APACHE), [copyright](licenses/unicode-segmentation-1.13.3/COPYRIGHT) |
| unicode-width | 0.2.2 | Logical CJK/emoji display widths | MIT OR Apache-2.0; [MIT](licenses/unicode-width-0.2.2/LICENSE-MIT), [Apache](licenses/unicode-width-0.2.2/LICENSE-APACHE), [copyright](licenses/unicode-width-0.2.2/COPYRIGHT) |
| unicode-linebreak | 0.1.5 | Unicode line-break opportunities | Apache-2.0; [license](licenses/unicode-linebreak-0.1.5/LICENSE) |

The image feature graph adds these 19 fixed registry packages. Every original
license variant found in the published package root is retained in its named
`licenses/` directory; the links below identify the package's license folder
through one of those original texts.

| Dependency | Locked version | Purpose | License and retained original text |
| --- | --- | --- | --- |
| image | 0.25.10 | Image format dispatch/RGBA8 | MIT OR Apache-2.0; [MIT](licenses/image-0.25.10/LICENSE-MIT) |
| png | 0.18.1 | PNG codec | MIT OR Apache-2.0; [MIT](licenses/png-0.18.1/LICENSE-MIT) |
| zune-jpeg | 0.5.15 | JPEG codec | MIT OR Apache-2.0 OR Zlib; [MIT](licenses/zune-jpeg-0.5.15/LICENSE-MIT) |
| zune-core | 0.5.3 | JPEG support | MIT OR Apache-2.0 OR Zlib; [MIT](licenses/zune-core-0.5.3/LICENSE-MIT) |
| image-webp | 0.2.4 | WebP codec | MIT OR Apache-2.0; [MIT](licenses/image-webp-0.2.4/LICENSE-MIT) |
| adler2 | 2.0.1 | Deflate checksum | 0BSD OR MIT OR Apache-2.0; [MIT](licenses/adler2-2.0.1/LICENSE-MIT) |
| autocfg | 1.5.1 | Build-time compatibility probe | Apache-2.0 OR MIT; [MIT](licenses/autocfg-1.5.1/LICENSE-MIT) |
| bytemuck | 1.25.2 | Pixel byte conversion | Zlib OR Apache-2.0 OR MIT; [MIT](licenses/bytemuck-1.25.2/LICENSE-MIT) |
| byteorder-lite | 0.1.0 | Byte order | Unlicense OR MIT; [MIT](licenses/byteorder-lite-0.1.0/LICENSE-MIT) |
| cfg-if | 1.0.4 | Conditional compilation | MIT OR Apache-2.0; [MIT](licenses/cfg-if-1.0.4/LICENSE-MIT) |
| crc32fast | 1.5.0 | PNG checksum | MIT OR Apache-2.0; [MIT](licenses/crc32fast-1.5.0/LICENSE-MIT) |
| fdeflate | 0.3.7 | PNG deflate | MIT OR Apache-2.0; [MIT](licenses/fdeflate-0.3.7/LICENSE-MIT) |
| flate2 | 1.1.9 | Deflate | MIT OR Apache-2.0; [MIT](licenses/flate2-1.1.9/LICENSE-MIT) |
| miniz_oxide | 0.8.9 | Deflate implementation | MIT OR Zlib OR Apache-2.0; [MIT](licenses/miniz_oxide-0.8.9/LICENSE-MIT.md) |
| moxcms | 0.8.1 | Color management | BSD-3-Clause OR Apache-2.0; [BSD](licenses/moxcms-0.8.1/LICENSE.md) |
| num-traits | 0.2.19 | Numeric codec support | MIT OR Apache-2.0; [MIT](licenses/num-traits-0.2.19/LICENSE-MIT) |
| pxfm | 0.1.30 | Pixel color transforms | BSD-3-Clause OR Apache-2.0; [BSD](licenses/pxfm-0.1.30/LICENSE.md) |
| quick-error | 2.0.1 | Codec error definitions | MIT/Apache-2.0; [MIT](licenses/quick-error-2.0.1/LICENSE-MIT) |
| simd-adler32 | 0.3.10 | Adler checksum | MIT; [license](licenses/simd-adler32-0.3.10/LICENSE.md) |

The parser's published Rust minimum is 1.71.1, below this module's 1.96.1 pin.
The parser/view base graph has seven platform-neutral registry dependencies;
image decoding adds 19, for 26 in the selected lockfile. The image crate
publishes a Rust minimum of 1.88, below the Reader 1.96.1 pin. No native codec,
OS-target feature or network client is selected. A fixed release plus lockfile
avoids silent updates. Windows/MSVC builds and tests were run; Linux/macOS
execution is not claimed. Preserve the above original texts
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

M4 semantic paint data adds no third-party package. Formula raster candidates
are discussed in the [Reader ADR](ADR.md); none is linked, bundled or distributed
by this crate yet, so its lockfile and retained license set are unchanged.
