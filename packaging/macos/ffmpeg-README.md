# Temporary macOS FFmpeg candidate

The pinned candidate report inventories an FFmpeg source archive that includes
COPYING.LGPLv2.1, the requested shared/non-GPL/nonfree-disabled build flags, and
each staged libav*/libsw* version, configuration, license-function result and
hash. These are source/configuration/runtime evidence, not a licensing
determination or approval, and they do not state Iris's project license. This
inventory describes only the pinned macOS candidate, not the current release
runtime inventoried separately as audit/macos.json. The manifest identifies the
macOS build and its shared source pin, signature identity and verification,
generated decoder inventory, build tools, source and staged dylib hashes. Its
liblzma record includes the XZ source pin, verified primary/signing fingerprints,
source-license digests, CMake version/flags and static archive hash. It does
not report Linux system-package or SONAME policy as macOS evidence. config.h,
config_components.h and config.mak retain the generated build configuration.
The final candidate JSON resolves all Mach-O load edges, including weak loads
and rpath contexts; system edges require an exact Apple SDK stub install-name
match and are never classified by basename or path prefix alone.

The exact corresponding source, detached signature and public signing key are
adjacent as source.tar.xz, source.tar.xz.asc and signing-key.asc. The source
archive contains upstream copyright notices and the complete build system.
Verify the archive digest and GnuPG signature against the manifest's required
primary fingerprint before extraction. The policy includes the upstream security
reference and its narrow CVE scope; no broader vulnerability review is claimed.
Update the shared source pin when evaluating an FFmpeg update. System-edge
records identify SDK stubs; they do not inventory live OS dyld shared-cache
bytes.

## Rebuilding and replacing

Install Xcode command-line tools, Python 3, GnuPG, pkg-config, CMake, make and
nasm (nasm is used on x86). The selected Apple SDK must supply zlib, bzip2 and
iconv headers and linker stubs; liblzma is **not** taken from the SDK or Homebrew.

`packaging/macos/liblzma.json` pins the official XZ Utils v5.8.4 release asset
`xz-5.8.4.tar.xz`, its SHA-256, detached `.sig`, Lasse Collin's public-key URL
and required primary fingerprint `3690C240CE51B4670D30AD1C38EE757D69184620`.
The driver checks the archive hash, imported primary identity and GnuPG signature
before extraction. `xz-COPYING` records upstream's liblzma-specific 0BSD
statement; `xz-COPYING.0BSD` must match the pinned source-license digest. This is
source licensing evidence for **liblzma**, not a license claim for all XZ tools,
Iris, the current release runtime or the complete candidate. It is not legal
approval. The temporary documents include `xz-source.tar.xz`, its `.sig`,
`xz-signing-key.asc`, `xz-signature.status` and `liblzma-build.json`.

The driver configures XZ in a fresh, separate scratch prefix with every pinned
`cmake_flags` entry: static/PIC, tests/NLS/tools/docs off. It builds with
`cmake --build` and installs only `liblzma_Development` with `cmake --install`.
The SDK and xcrun compiler are selected explicitly. It then sets FFmpeg CFLAGS
to the SDK plus that prefix's include directory, LDFLAGS to the SDK plus that
prefix's lib directory, and both PKG_CONFIG_PATH and PKG_CONFIG_LIBDIR to only
that prefix's lib/pkgconfig. Ambient compiler and pkg-config search overrides
are removed; the pkg-config version and libdir must match the pin/prefix.
The preflight links that exact static archive. FFmpeg receives the unchanged
`policy.source.configure_flags` (including `--enable-lzma`), then make/build and
install run; generated `CONFIG_LZMA=1` remains mandatory. There are no patches.
Shared FFmpeg/PIC are enabled, static FFmpeg/GPL/version3/nonfree and optional
autodetection disabled; built-in decoders are not pruned. The static liblzma
code is linked into FFmpeg, never copied into candidate `/lib` or app Frameworks.
Dynamic liblzma edges (even SDK ones), extra dylibs and static archives in
candidate `/lib` fail closed. The final closure remains the exact FFmpeg
candidate dylibs plus verified Apple SDK edges.

The exact-source Iris checkout automates this with
`python3 scripts/macos-ffmpeg.py validate audit/macos-candidate.json`; it
intentionally requires fresh output and targets and deletes all native/source/app
bytes. The staged helper source documents install-name rewriting: copy the
versioned candidate dylibs to Contents/Frameworks, change their IDs and
inter-library loads to @loader_path/basename and executable loads to
@executable_path/../Frameworks/basename. Use install_name_tool, then ad-hoc
codesign each dylib and helper before codesign of the outer app (timestamp=none).
For engineering tests, ABI-compatible replacements can be staged without
rebuilding Iris; this describes packaging mechanics only, not any license
condition. Re-sign nested libraries then the app with
`codesign --force --deep --sign - --timestamp=none Iris.app` for local use.
The report gate's pin does not itself restrict local replacement, and the
workflow does not validate user-supplied replacements. Re-signing changes the
app signature and does not preserve the original distribution trust claims.

## Evidence boundaries

Only bounded JSON leaves this validation job: the current release runtime as
audit/macos.json and this candidate as audit/macos-candidate.json. Downloaded
sources, signatures, keys, generated and downloaded fixtures, staged apps,
static/shared libraries, Cargo targets and build scratch are temporary and
cleaned on success, failure and interruption. No ZIP, installer, source bundle or
Release dispatch is retained. The tagged release workflow is unchanged and
remains on hold; this candidate is not a release and its inventory is not legal
approval.
Both default-feature runtime smokes use the same once-fetched, digest-pinned
[preview corpus](../../tests/preview-corpus/README.md), reverified before extraction.
Fresh separate Cargo targets prevent reuse of FFmpeg discovery. The candidate
smoke is rewritten/signed into the temporary app; its resolved build and staged
closures must link the pinned libavcodec before execution. Both preview builds
clear ambient compiler/linker/include and pkg-config search overrides. The report
retains correctness and JPEG/CR3/H.264 timings for both runtimes; paired differences require matching input, output, build, source
and runner/CPU identities. Only H.264 timing exercises FFmpeg; JPEG/CR3 use Iris
Rust code. PDF correctness uses Core Graphics. Existing generated MPEG-4/PNG
fallback smokes and native inventory remain mandatory candidate checks even if
the corpus fetch or Homebrew discovery/build/smoke fails. Such failures record a
bounded, path-redacted `preview.status=unavailable` reason and omit paired
comparisons. A failed fetch sets `smoke.corpus_correctness=false`; candidate
corpus correctness (when fetched), closure and generated-smoke failures still fail
validation. Cancellation is not treated as an optional failure. There is no
latency threshold or claim of runtime parity, sensor demosaic, HDR/HEIF coverage
or legal approval.

Policy defaults (all positive, configurable via job variables): IRIS_FFMPEG_JOBS
2, IRIS_FFMPEG_BUILD_SECONDS 3600 per build command, IRIS_FFMPEG_FETCH_SECONDS
300 per fetch, IRIS_FFMPEG_COMMAND_SECONDS 60 per inspection/smoke,
IRIS_FFMPEG_COMMAND_BYTES 32768 per captured diagnostic/inspection,
IRIS_FFMPEG_MAX_NATIVE_FILES 256 traversal contexts per root, and
IRIS_FFMPEG_MAX_REPORT_BYTES 266240. Exceeding a budget fails closed, never
truncates a successful inventory. No candidate prefix or Cargo target is cached.
