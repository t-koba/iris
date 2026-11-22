# paste 1.0.15 (pastey 0.1.1)

Source: the crates.io `pastey` 0.1.1 package, SHA-256 checksum
`35fb2e5f958ec131621fdd531e9fc186ed768cbe395337403ae56c17a74c68ec`,
upstream repository <https://github.com/as1100k/pastey>.
License: MIT OR Apache-2.0; see `LICENSE-MIT` and `LICENSE-APACHE`.

This is a temporary vendor patch: the sources and README are byte-for-byte
upstream pastey 0.1.1, and the manifest differs only in the package/lib name
(`paste`) and version (`1.0.15`), needed to satisfy rav1e's `paste = "1.0"`
requirement through a `[patch.crates-io]` entry.

RUSTSEC-2026-0543 marks `paste` as unmaintained with no fixed upstream
release. rav1e (via image/iced) uses only `paste::item!`-style token pasting,
which the maintained fork pastey provides; av-scenechange already uses pastey
elsewhere in this dependency tree. A registry-to-registry `[patch.crates-io]`
rename is not accepted by Cargo, so the fork is vendored under the `paste`
name instead of adding an audit ignore.

Remove `vendor/paste` and the root `[patch.crates-io]` entry when upstream
rav1e moves to pastey or `paste` is maintained again.
