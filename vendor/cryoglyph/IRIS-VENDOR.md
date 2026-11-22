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
instead. Remove `vendor/cryoglyph` and the root `[patch.crates-io]` entry when
upstream cryoglyph updates its `lru` requirement and publishes a release.
