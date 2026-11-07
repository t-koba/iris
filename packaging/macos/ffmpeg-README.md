# Temporary macOS FFmpeg candidate

The pinned candidate report inventories an FFmpeg source archive that includes
COPYING.LGPLv2.1, the requested shared/non-GPL/nonfree-disabled build flags, and
each staged libav*/libsw* version, configuration, license-function result and
hash. These are source/configuration/runtime evidence, not a licensing
determination or approval, and they do not state Iris's project license. This
inventory describes only the pinned macOS candidate, not the current release
runtime inventoried separately as audit/macos.json. The manifest identifies the
macOS build and its shared source pin, signature identity and verification,
generated decoder inventory, build tools, source and staged dylib hashes. It does
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

Install Xcode command-line tools, Python 3, GnuPG, pkg-config, make and nasm
(nasm is used on x86). Build against the selected Apple SDK, which must provide
zlib, bzip2, liblzma and iconv headers and linker stubs. The final load-closure
check rejects unresolved non-SDK runtime dependencies outside the exact candidate
dylibs. Extract source.tar.xz, enter its ffmpeg-version directory and set
SDKROOT=$(xcrun --sdk macosx --show-sdk-path), CC=$(xcrun -f clang),
CFLAGS="-isysroot $SDKROOT" and LDFLAGS="-isysroot $SDKROOT" in the environment.
Run ./configure --prefix=/absolute/prefix --libdir=/absolute/prefix/lib followed
by every manifest policy.source.configure_flags entry, then make -j2 && make install.
There are no source patches. Shared/PIC are enabled, static/GPL/version3/nonfree
and optional autodetection disabled; built-in decoders are not pruned and
compression/iconv remain explicitly enabled.

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
sources, generated fixtures, staged apps, libraries and build scratch are
temporary and cleaned even on failure. No ZIP, installer, source bundle or
Release dispatch is retained. The tagged release workflow is unchanged and
remains on hold; this candidate is not a release and its inventory is not legal
approval.
PDF/raw-full product features stay enabled but this job's tiny generated
MPEG-4 and PNG FFmpeg fallback smokes do not establish PDF, sensor RAW,
HDR/HEIF correctness, representative corpus coverage or latency parity.

Policy defaults (all positive, configurable via job variables): IRIS_FFMPEG_JOBS
2, IRIS_FFMPEG_BUILD_SECONDS 3600 per build command, IRIS_FFMPEG_FETCH_SECONDS
300 per fetch, IRIS_FFMPEG_COMMAND_SECONDS 60 per inspection/smoke,
IRIS_FFMPEG_COMMAND_BYTES 32768 per captured diagnostic/inspection,
IRIS_FFMPEG_MAX_NATIVE_FILES 256 traversal contexts per root, and
IRIS_FFMPEG_MAX_REPORT_BYTES 266240. Exceeding a budget fails closed, never
truncates a successful inventory. No candidate prefix or Cargo target is cached.
