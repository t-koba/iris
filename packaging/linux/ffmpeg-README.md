# Iris Linux FFmpeg runtime

This directory accompanies the dynamically linked FFmpeg libraries in
`usr/lib/iris` (or `/usr/lib/iris` after Debian installation). FFmpeg is a
separate work under LGPL 2.1 or later; see `COPYING.LGPLv2.1`. This is not a
license declaration for Iris. PDFium has its own adjacent notices/provenance.

`manifest.json` records the exact upstream source URL and SHA-256, required
signing-key fingerprint, successful detached-signature status, actual library
ABI/configuration/license strings, enabled codecs, hashes and build tool versions.
FFmpeg system dependencies are limited to an explicit glibc/GCC/zlib/bzip2/liblzma
runtime allowlist; the runtime manifest records each resolved file hash,
binary/source package identity, and distro copyright digest. The bounded product
audit report separately records each non-bundled system library directly resolved
by Iris, including package identity, file hash, configured license reference, and
distro copyright digest. Unknown packages block the audit; this is runtime
inventory evidence, not legal approval. These platform libraries
are not bundled and receive updates through the OS package manager. `config.h`,
`config_components.h` and `config.mak` record generated configuration and the
enabled built-in codecs. GPL, version3 and nonfree are disabled; shared/PIC is
on. Optional external libraries are not autodetected. Basic zlib/bzip2/liblzma
compression support and libc iconv are enabled explicitly so compressed PNG,
TIFF and other built-in codecs are not lost. Built-in decoders are not pruned. The ELF RUNPATH is changed with patchelf to `$ORIGIN`, with no changes
to the upstream source. Iris uses `$ORIGIN/../lib/iris`.

## Source and rebuilding/replacement

The exact, unmodified corresponding source is included as `source.tar.xz`, with
`source.tar.xz.asc` and `signing-key.asc`. The official source is also available
at https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz. Verify the SHA-256 in the
manifest and verify the signature with the fingerprint recorded there before
building. The upstream license, copyright notices, configure script, build
system and source are all in the archive.

Install a C compiler, binutils, make, nasm, pkg-config, patchelf, and the
zlib, bzip2 and liblzma development headers/libraries (on Ubuntu:
`zlib1g-dev libbz2-dev liblzma-dev`). Extract the
archive, enter `ffmpeg-8.1.3`, and run `./configure --prefix=/absolute/prefix
--libdir=/absolute/prefix/lib` followed by every flag from the manifest's
`policy.configure_flags` array. Run `make -j2 && make install`. Set each real
shared library's RUNPATH with `patchelf --set-rpath '$ORIGIN' path/to/library`.
The Iris source tree automates digest/signature verification and these steps
with `scripts/build-linux-ffmpeg.sh`; `IRIS_FFMPEG_PREFIX` chooses an absolute
prefix and `IRIS_FFMPEG_JOBS` chooses parallelism (default 2). A completed prefix
is revalidated on reuse; delete it when intentionally rebuilding or updating.

These are replaceable shared libraries, not statically linked into Iris.
Keep the ABI-compatible SONAME symlinks, replace the libraries in `usr/lib/iris`
and run Iris normally; no Iris object files or relink step is required. You may
modify/rebuild FFmpeg and use it with Iris, and reverse engineer Iris as needed
to debug those modifications. The official packaging gate verifies its pinned
runtime; that gate does not restrict local replacement. The library RUNPATH
also supports a compatible replacement through the dynamic loader's normal
`LD_LIBRARY_PATH` mechanism. No integrity check is imposed by the Iris app.

## Security and system dependencies

The upstream security page (https://ffmpeg.org/security.html) lists fixes for
CVE-2026-8461 and CVE-2026-30999 in 8.1.2; this runtime selects 8.1.3 (libavcodec
62.28.103). This is not a claim of a broader security review. Iris distributors
must update the source/hash/signature pin and rebuild to ship FFmpeg fixes;
Ubuntu does not update these private bundled libraries. Debian's generated
`Depends` declares the system dependency closure, including PDFium and Iris;
unknown/missing package mappings block packaging. The AppDir uses that same
closure on the build's Ubuntu baseline and is not a universal Linux binary.
Its `system-dependencies.txt` records the corresponding Debian requirements.
The runtime's allowed system SONAMEs are explicit in the manifest policy.

## Validation without publication

The registered `.github/workflows/ci.yml` calls the reusable Linux FFmpeg
validation job only on `workflow_dispatch`, at the caller's exact SHA (no source
override). It builds with Rust 1.97.1 and locked/offline Cargo, then packages
locally. It verifies actual Iris ELF resolution to these exact DSOs,
ABI/configuration/license/closure, binary size, PDFium and video thumbnails in
the AppDir and after a clean Debian install. Only bounded JSON audit reports
may leave that runner; no library, AppDir, source bundle, archive or installer
is uploaded. A passing report is runtime evidence, not legal approval or a
cross-platform release approval.
