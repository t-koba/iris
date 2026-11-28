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

The portable ZIP and the clean installer now carry the following flat files
beside `Iris.exe`, the versioned FFmpeg DLL set, `pdfium.dll` and this notice.
They are produced offline by
`scripts/windows-release-source-bundle.py build` from the already verified
runner inputs, hashed in `ffmpeg-source-bundle.json`, and inventoried together
with the binaries in `product-inventory.json`; the same list feeds the ZIP,
the installer source file and the reviewed native evidence.

- `ffmpeg-8.1.3.tar.xz`: the byte-exact, signature-verified upstream source
  archive, including its configure script, build system and complete source.
- `ffmpeg-legal-notices.tar.gz`: upstream `COPYING.LGPLv2.1`, `COPYING.GPLv2`,
  `COPYRIGHT`/`CREDITS`, this notice and a `REPLACEMENT.md` excerpt of the
  DLL replacement instructions below.
  It also retains `openjpeg2-pure-rs-license.txt`, the verbatim BSD-2-Clause
  notice for the pure-Rust JPEG 2000 decoder compiled into Iris,
  `noto-sans-cjk-jp-OFL.txt`, the verbatim SIL OFL 1.1 notice for the Noto Sans
  CJK JP subset compiled into Iris, and `material-symbols-apache-2.0.txt`, the
  verbatim Apache-2.0 notice for the 48-glyph Material Symbols Outlined subset
  compiled into Iris.
- `ffmpeg-build-materials.tar.gz`: the derived vcpkg portfile and its evidence,
  retained baseline-port patches, manifest, registry configuration, triplet,
  source-acquisition record, detached signature, signing key and GPG status.
  The pinned baseline port also contributes `FindFFMPEG.cmake.in` (CMake,
  BSD-3-Clause), plus `build.sh.in` and `vcpkg-cmake-wrapper.cmake` (vcpkg ports
  tree, MIT); the legal archive retains their upstream notices as
  `cmake-copyright.txt` and `vcpkg-license.txt`.
  The portfile's runner-local source path is replaced by
  `$IRIS_VALIDATION_SOURCE/ffmpeg-8.1.3`; the original hash stays in the
  bundle report so the derivation remains checkable without publishing a
  runner path.
- `pdfium-notices.tar.gz`: the attested bundle's `LICENSE`, `VERSION`,
  `args.gn` and complete `licenses/` tree.
- `ffmpeg-source-bundle.json`: SHA-256 and byte counts for every input and
  output of that offline bundling step.

The DLLs remain dynamically linked and replaceable; no Iris object files or
relink step are added. This is a factual source, configuration and notice
record, not legal approval, parity evidence or release readiness.

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
these steps with stock locked bindings, builds locally for validation and
emits bounded JSON evidence. The default path promotes no release and retains
no native bytes: the rehearsal ZIP is assembled and hash-verified inside the
run and removed at cleanup, with only JSON evidence uploaded. A release caller
can opt into exporting the verified ZIP, installer and their hashes as
`windows-release-assets`; `release.yml` re-verifies them before packaging.

These are replaceable shared DLLs, not statically linked into Iris. Under LGPL
section 6(b), users may modify and rebuild FFmpeg, swap the versioned DLL set
in place, and reverse engineer Iris to debug those modifications. No Iris
signature or hash gate blocks a compatible replacement, and no Iris object
files or relink step are required. The official policy gate checks the shipped
build but does not restrict local replacement.

## Scope and unresolved items

The rehearsal's PE import closure classifies file-backed VC/UCRT
redistributable imports, including `vcruntime140.dll` and
`vcruntime140_1.dll`, separately as `vc-redist`, never as Windows OS DLLs. A
runner-installed copy in System32 is recorded but is not proof that a fresh
Windows install provides it, as the checked-in
`packaging/windows/validation/windows-vc-redist-allowlist.txt` requires.
Virtual `api-ms-win-*` and `ext-ms-win-*` API sets resolve as `system`
forwarders; genuine OS DLL imports resolve as `os` only when allowlisted and
physically present in a Windows system directory, staged DLLs resolve as
`stage`, and anything else is `unresolved`.

The x64 installer treats the Visual C++ Redistributable for Visual Studio
2015-2022 (x64) as an external prerequisite: it checks
the native 64-bit system directory for `vcruntime140.dll`,
`vcruntime140_1.dll` and `msvcp140.dll` and aborts before installing when any
is missing. Microsoft documents that the matching Visual C++ redistributable
must be installed on the target machine for apps using the Visual C++ runtime
(https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files?view=msvc-170
and https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170).
No `vc_redist` executable is bundled with, downloaded by, or staged for the
installer, and no app-local VC runtime copies are staged. Runner presence
remains evidence about the runner only, not about a clean target. This notice
makes no clean-target provisioning proof or app-local redistribution claim. It
is runtime, source and notice evidence only: not legal approval, parity
evidence, latency parity, audit approval or release readiness.
