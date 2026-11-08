# macOS FFmpeg runtime

Product packaging (`scripts/package-macos-app.sh`) builds the pinned shared
FFmpeg runtime with static/PIC liblzma in fresh native and Cargo scratch. It
never selects Homebrew FFmpeg or falls back to it. Default Iris features remain
enabled. ZIP and PKG contain this app, its runtime and the evidence below.

`Contents/Resources/ffmpeg/manifest.json` identifies the exact FFmpeg source pin,
verified signing identity, configure policy, generated decoder inventory, build
tools, source hashes and pre/post-signing dylib hashes. `liblzma-build.json`
records the XZ source pin, signature identity, source-license digests, CMake
flags/version and static archive hash. `config.h`, `config_components.h` and
`config.mak` retain generated configuration, with private build paths redacted.
`Contents/Resources/ffmpeg-runtime.json` records actual libav*/libsw* version,
configuration, license-function result and signed-library hash, the complete
resolved Mach-O closure, and mandatory default-feature preview-corpus results.
The outer app signature is applied afterwards; this inventory does not claim
final main-executable hashes. Packaging rechecks the nested bytes and closure
after signing, before archiving.

These are source/configuration/runtime evidence, not a licensing determination,
legal approval, latency parity or release approval, nor a statement of Iris's
project license. The policy includes an upstream security reference and its
narrow CVE scope; no broader vulnerability review is claimed. System edges
require matching install names in the selected Apple SDK's stubs, not just an
OS path prefix. OS shared-cache bytes and updates are supplied by macOS.
Unknown non-system libraries, dynamic liblzma, changed evidence, GPL/version3/
nonfree configuration, missing required decoders or failed correctness abort
packaging; no Homebrew dependency closure is accepted for the product.

## Source, notices and rebuilding

Adjacent `source.tar.xz`, `source.tar.xz.asc` and `signing-key.asc` are the exact
corresponding FFmpeg source, detached signature and public key. The archive
contains upstream copyrights and the complete build system; COPYING.LGPLv2.1
is also adjacent. Verify the archive hash and signature against the manifest's
pinned primary fingerprint before extraction. There are no source patches.

`packaging/macos/liblzma.json` pins XZ Utils 5.8.4, its archive hash, detached
signature, key URL and required primary fingerprint. `xz-source.tar.xz`, its
`.sig`, `xz-signing-key.asc` and `xz-signature.status` accompany the runtime.
`xz-COPYING` records upstream's liblzma-specific 0BSD declaration;
`xz-COPYING.0BSD` must match the pinned license digest. This is evidence for
liblzma, not a licensing claim for all XZ tools or Iris. The build installs only
`liblzma_Development`, static/PIC with tests, tools, NLS and docs disabled.
FFmpeg links that archive, never Homebrew/SDK liblzma. zlib, bzip2 and iconv
headers/stubs come from the explicitly selected Apple SDK.

Use the exact-source Iris checkout, Xcode command-line tools, Python 3, GnuPG,
pkg-config, CMake, make and nasm (x86). Run `cargo fetch --locked`, then
`./scripts/package-macos-app.sh`. The source policy is shared with
`packaging/linux/ffmpeg.json`, but no Linux ELF mechanics or package evidence is
used for macOS. Shared FFmpeg/PIC are enabled; static FFmpeg, GPL, version3,
nonfree and optional autodetection are disabled; built-in decoders are not
pruned. The builder clears ambient compiler/include/linker/pkg-config discovery
and requires the pinned prefix. Update the source pin when evaluating updates.

For local engineering use, ABI-compatible replacements can be installed in
`Contents/Frameworks` without rebuilding Iris. IDs and inter-library loads use
`@loader_path/basename`; executable loads use
`@executable_path/../Frameworks/basename`. Re-sign nested libraries then the app
(e.g. `codesign --force --deep --sign - --timestamp=none Iris.app`). This changes
the distribution signature and invalidates its recorded hashes, not a license
condition or a claim that user replacements passed the distribution audit.

## Report-only comparison

`python3 scripts/macos-ffmpeg.py validate audit/macos-candidate.json` remains a
separate experiment: temporary Homebrew and pinned default-feature smokes use
fresh targets and the same pinned corpus. The historical Homebrew app inventory
is `audit/macos.json`, explicitly selected with `IRIS_MACOS_REPORT_ONLY=1` and
`IRIS_MACOS_FFMPEG_BASELINE=1`; it cannot use release signing or notarization.
Only bounded JSON leaves that workflow. Apps, archives, sources, fixtures and
native/Cargo scratch are deleted; it never dispatches Release. Optional baseline
or corpus-fetch failure there records unavailable comparison evidence without
suppressing the candidate's generated MPEG-4/PNG checks. In production, corpus
fetch and correctness are mandatory. No comparison claims parity, sensor
RAW demosaic, HDR/HEIF coverage or legal approval.

Both paths use positive configurable budgets: IRIS_FFMPEG_JOBS 2,
IRIS_FFMPEG_BUILD_SECONDS 3600, IRIS_FFMPEG_FETCH_SECONDS 300,
IRIS_FFMPEG_COMMAND_SECONDS 60, IRIS_FFMPEG_COMMAND_BYTES 32768,
IRIS_FFMPEG_MAX_NATIVE_FILES 256 and IRIS_FFMPEG_MAX_REPORT_BYTES 266240.
Budgets fail closed rather than truncate successful evidence. Product scratch
is removed on success, failure and handled interruption; sources/notices remain
inside the distributed app. No native runtime or Cargo discovery target is cached.
