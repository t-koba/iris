# cryoglyph 0.1.0

Source: the crates.io `cryoglyph` 0.1.0 package, SHA-256 checksum
`08bc795bdbccdbd461736fb163930a009da6597b226d6f6fce33e7a8eb6ec519`,
upstream repository <https://github.com/iced-rs/cryoglyph>.
License: MIT OR Apache-2.0 OR Zlib; see `LICENSE-MIT`, `LICENSE-APACHE`, and
`LICENSE-ZLIB`.

This temporary patch changes exactly one line of the upstream manifest:
the `lru` requirement under `[dependencies.lru]` moves from 0.16 to 0.18,
with `default-features = false` unchanged. All vendored source files are
byte-for-byte upstream.

The reason is RUSTSEC-2026-0253: `src/text_atlas.rs` calls
`LruCache::pop_lru()`, which reaches the use-after-free in `lru` 0.16.4.
`iced_wgpu` pulls cryoglyph in transitively, and crates.io has no fixed
cryoglyph release (0.1.0 is the latest); the cached `lru` 0.18.5 is used
instead.

This is a scoped waiver, not the general vendor policy. Upstream landed the
`lru` 0.18.2 fix only on the cryoglyph line that requires wgpu 29 or newer,
while Iris resolves iced 0.14 / iced_wgpu 0.14 / wgpu 27. Migrating Iris to
an unreleased iced line, or writing an Iris-owned GPU text atlas, is not a
drop-in change and would risk preview latency and format coverage. Removing
this patch without a replacement would silently restore vulnerable `lru` 0.16.

Revisit when `Cargo.lock` resolves `iced_wgpu` against wgpu 29 or newer and
`cryoglyph` requires `lru >= 0.18.2`. At that point remove this directory and
the root `[patch.crates-io]` entry only after the preview latency and format
coverage checks pass. Until that trigger is observed, keep the patch and the
dependency checks intact.
