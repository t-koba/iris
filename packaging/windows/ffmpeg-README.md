# Windows FFmpeg runtime notice

This file accompanies the dynamically linked FFmpeg DLLs and the PDFium DLL in
the Windows product rehearsal ZIP as `FFMPEG-NOTICE.md`. Iris remains under its
own project license; FFmpeg is a separate work under LGPL 2.1 or later, and
PDFium has its own notices. This is a factual runtime/source/notice record, not
a license declaration for Iris and not a legal determination.

## Exact pins

`packaging/windows/ffmpeg.json` records the Windows build contract and
references the shared FFmpeg source pin in `packaging/linux/ffmpeg.json`;
`packaging/windows/validation/` carries the manifest and triplet. Together they record the
build inputs:

- FFmpeg 8.1.3 from https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz, SHA-256
  `7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3`. Its
  detached signature is https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz.asc and
  its signing key is https://ffmpeg.org/ffmpeg-devel.asc; the required signing
  fingerprint is `FCF986EA15E6E293A5644F10B4322F04D67658D8`.
- vcpkg baseline `b8dfe957ec8b5c0ef5c4b31eea594b8486af744f` (manifest
  `packaging/windows/validation/vcpkg.json`, registry pin
  `packaging/windows/validation/vcpkg-configuration.json`) and dynamic triplet
  `x64-windows-iris-dynamic`
  (`packaging/windows/validation/triplets/x64-windows-iris-dynamic.cmake`).
  That baseline's ffmpeg port builds 9.0.2, so the verified 8.1.3 tarball
  overrides the port source. Locked bindings are ffmpeg-next 9.0.0 and
  ffmpeg-sys-next 9.0.1, whose ffmpeg_8_1 ABI matches this build.
- PDFium release `chromium/8138` (159.0.8138.0),
  `pdfium-win-x64.tgz` from
  https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8138/pdfium-win-x64.tgz,
  asset SHA-256 `8d48973e3eb472dda0f5f2435e595ac043f9f2219dd71883392f549445afa8a5`
  and attestation `pdfium-attestation.json` SHA-256
  `7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7`.
  Provenance is bblanchon/pdfium-binaries `build-all.yml` at digest
  `0eb94b28c2252d2baae3dfe15dc6b237131d8cd8`, source ref `refs/heads/master`.
  The validation workflow verifies asset hash and release attestation before
  extracting the DLL.

## Notices and corresponding source

A product payload carrying these DLLs is accompanied by the FFmpeg
`COPYING.LGPLv2.1` and `COPYING.GPLv2` texts and the upstream copyright notices
from the exact archive, and by the attested PDFium `LICENSE`, `VERSION` and
`licenses/` notices with the provenance above. The archive also contains the
upstream configure script, build system and complete source. The exact
corresponding source can be supplied as the verified `ffmpeg-8.1.3.tar.xz` with
its detached signature and signing key; this notice is the written-offer
record with that archive's exact URL, SHA-256 and fingerprint. Configuration,
the derived vcpkg overlay, triplet and any retained baseline-port patches are
described below. The report-only rehearsal ZIP verifies Iris.exe, the
versioned FFmpeg DLL set, pdfium.dll and this notice; staging of the other
listed documents in a product payload is not decided here.

## Rebuilding and LGPL section 6(b) replacement

Check out the pinned vcpkg commit, verify and stage the exact source with
`python3 scripts/windows-ffmpeg-source.py plan` / `stage`, and derive the
overlay port with `python3 scripts/windows-ffmpeg-overlay.py derive`. Baseline
port patches are retained and adapted only from exact pinned-source contexts.
Run vcpkg install from `packaging/windows/validation` with `VCPKG_OVERLAY_PORTS`
set to the derived overlay and `VCPKG_OVERLAY_TRIPLETS` to
`packaging/windows/validation/triplets`, using triplet
`x64-windows-iris-dynamic`. Build Iris with
`cargo build --release -p iris-ui-shell --locked` and stage the installed DLLs
beside `Iris.exe`. `.github/workflows/windows-ffmpeg-validation.yml` performs
these steps with stock locked bindings and emits bounded JSON evidence; it
builds and publishes nothing by itself.

These are replaceable shared DLLs, not statically linked into Iris. Under LGPL
section 6(b), users may modify and rebuild FFmpeg, swap the versioned DLL set
in place, and reverse engineer Iris to debug those modifications. No Iris
signature or hash gate blocks a compatible replacement, and no Iris object
files or relink step are required. The official policy gate checks the shipped
build but does not restrict local replacement.

## Scope and unresolved items

`vcruntime140.dll` and `vcruntime140_1.dll` currently resolve from the Windows
system in the rehearsal's PE import closure; no app-local VC runtime copies are
staged. The app-local VC runtime redistribution question is unresolved, and
this notice makes no app-local redistribution claim. This is runtime, source
and notice evidence only: not legal approval, parity evidence, latency parity,
audit approval or release readiness.
