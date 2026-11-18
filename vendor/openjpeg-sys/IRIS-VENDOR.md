# openjpeg-sys 1.0.12

Source: the crates.io `openjpeg-sys` 1.0.12 package from
<https://github.com/Enet4/openjpeg-sys>, crates.io archive SHA-256
`92682cb92e7b01e2c020cf8ec12f3374558924bb24621df1112d8a7c93d419ef`.
The package bundles OpenJPEG revision `210a8a5690d0…`, declared as 2.5.3
in `config/opj_config.h`. License: BSD-2-Clause; see `vendor/LICENSE`
and the notices in the C sources.

This temporary Cargo patch backports two decoder fixes without changing the
crate version, Rust API, or JPEG 2000 preview support:

- [CVE-2025-54874](https://www.cve.org/CVERecord?id=CVE-2025-54874):
  [`f809b80c67717c152a5ad30bf06774f00da4fd2d`](https://github.com/uclouvain/openjpeg/commit/f809b80c67717c152a5ad30bf06774f00da4fd2d)
  (OpenJPEG PR #1573, released in 2.5.4) sets JP2 color space only after a
  successful header read. This is hardening for Iris: the `jpeg2k` wrapper
  initializes the output image pointer to null, so Iris's caller does not meet
  the advisory's precondition.
- [OSV-2025-219](https://osv.dev/vulnerability/OSV-2025-219) /
  [GHSA-78g4-xqwv-q3g8](https://github.com/advisories/GHSA-78g4-xqwv-q3g8):
  [`91d08b11a72764f6d199f32fa0c1b1abd4edc2ad`](https://github.com/uclouvain/openjpeg/commit/91d08b11a72764f6d199f32fa0c1b1abd4edc2ad)
  (upstream `master`, not in a release) guards both tile-part index writes
  in `opj_j2k_read_sod` with null and bounds checks.

Guard-presence verification (external evidence, not execution): the raw GitHub
patches for `f809b80c…` and `91d08b11…` were fetched and their SHA-256 digests
are `13d5afb17d56f5cba3644585f08c29b1b2831c9cd54db73d1fc17b8cebdbe921` and
`233bf4c149d7740dd802c36c3e78e80352454b27d5579c1a4ef3260c2f4e90a1`. The
backported guards match those patches: `jp2.c` sets color space only under
`if (ret && p_image && *p_image)`, and the two `end_header`/`end_pos` writes in
`opj_j2k_read_sod` are inside one null-and-bounds guard. The separate guard in
`opj_j2k_add_tlmarker` is already present in bundled OpenJPEG 2.5.3 and was not
modified by this backport.

These checks prove the guards are present in the source, not that a runtime
decoder path reaches them. In particular the `rejects_truncated_jpeg2000_codestream`
test fails during SIZ parsing, before `opj_j2k_read_sod` runs, so it does not
exercise the tile-part guard. `scripts/test-openjpeg-backport.py` binds this
guard-presence evidence to an exact source revision. Targeted decoder success
and clean-rejection tests are bound separately via exact-revision CI.

These are external advisory claims; no advisory proof of concept was reproduced
in Iris. Remove the override when the resolved dependency demonstrably contains
both fixes. Registry metadata, the duplicate manifest, and the crate lockfile
are omitted; the build script, Rust bindings and all other C sources are unchanged.
