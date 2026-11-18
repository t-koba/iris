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

These are external advisory claims; no advisory proof of concept was reproduced
in Iris. Remove the override when the resolved dependency demonstrably contains
both fixes. Registry metadata, the duplicate manifest, and the crate lockfile
are omitted; the build script, Rust bindings and all other C sources are unchanged.
