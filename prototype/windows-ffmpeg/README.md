# Isolated Windows default-preview prototype (test-only)

This directory is a non-publishing prototype for the Windows default-preview
investigation. It never changes product defaults, release payloads, security
behavior, or licensing conclusions, and it never publishes.

## Scope

- Test-only inputs and scripts under `prototype/windows-ffmpeg/` plus the
  reusable workflow `.github/workflows/windows-ffmpeg-prototype.yml`. The
  registered `.github/workflows/ci.yml` calls it only for `workflow_dispatch`,
  so it can run against an exact published ref without adding a separately
  registered workflow to the default branch.
- Does not touch `.github/workflows/release.yml` or the release packaging
  scripts (`scripts/package-windows.ps1`, `scripts/windows-installer.iss`,
  `scripts/fetch-pdfium-runtime.sh`, `scripts/release-audit.py`).
- The product builds keep their defaults. The prototype stages a separate
  test-only app directory under `target/`; nothing under `dist/` is written.
- Runner, vcpkg, FFmpeg, Rust binding, and PDFium pins live in `pins.json`
  (single source of truth). Every script fails closed on drift and never falls back to
  FFmpeg 9.0.2, an unknown runtime, or a minimal build.

## Pins (single source of truth: `pins.json`)

- FFmpeg source: `https://github.com/FFmpeg/FFmpeg.git` at
  `db8537d8cc7962bd3d07254d6ceff21d888817d4`. The official
  [FFmpeg download page](https://ffmpeg.org/download.html) lists this GitHub
  repository as a mirror of its main repository. Fetch and detached-checkout
  HEAD verification must succeed for that exact commit; an unavailable mirror
  or commit stops the run, without a source or version fallback.
- Test-only `ffmpeg-sys-next` source override: version `9.1.0` from
  `https://github.com/zmwangx/rust-ffmpeg-sys` at immutable commit
  `5aa9dc312e0ab9b762bae7ca367d17761d0c9447`. This is a pinned, **unsigned**
  upstream commit, not a signature-verified source or release approval. Its
  CUDA type declarations fix the 9.0.1 wrapper's mismatch with the pinned
  FFmpeg headers (`CUarray` and by-value `CUDA_ARRAY3D_DESCRIPTOR`).
  Both staged app and detached preview-smoke builds receive the same Cargo CLI
  `patch.crates-io` configuration and explicit `ffmpeg-next/non-exhaustive-enums`
  feature through the transaction helper. Product manifests, the root workspace
  Cargo.lock and default features stay unchanged; the detached prototype lock
  records its explicit direct wrapper dependency. Neither graph is proven
  compatible without
  an exact-source native build and smoke; unknown enum values may still panic.
- vcpkg / tooling baseline: `b8dfe957ec8b5c0ef5c4b31eea594b8486af744f`
  (`vcpkg.json` `builtin-baseline` and `vcpkg-configuration.json` carry the
  same baseline; the scripts fail closed on drift). The reusable prototype
  workflow retries only the observed WinHTTP timeout while downloading the
  versioned vcpkg tool: `IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRIES` defaults to 1
  (set to 0 to disable) and `IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRY_DELAY_SECONDS`
  defaults to 2. Other bootstrap failures remain immediate failures; the
  pinned checkout and version selection are unchanged.
- Triplet base: `x64-windows` (dynamic). The isolated install uses the custom
  dynamic triplet `triplets/x64-windows-prototype-dynamic.cmake`
  (`VCPKG_TARGET_TRIPLET=VCPKG_DEFAULT_TRIPLET=VCPKGRS_TRIPLET=x64-windows-prototype-dynamic`,
  `VCPKGRS_DYNAMIC=1`). `VCPKG_ROOT` is the isolated `.vcpkg-prototype`
  checkout; vcpkg installs into its `installed/` tree, exactly where
  `vcpkg-rs` searches. `VCPKG_INSTALLED_DIR`, used for staging, must agree;
  any `static` linkage fails.
- vcpkg features: LGPL-only / default library features plus exactly one
  test-only `ffmpeg` executable feature for deterministic smoke-video
  generation. No `gpl`, `nonfree`, or broad `all` feature is requested; the
  build gate fails on any such request.
- Windows PDFium: release `chromium/8138`, asset `pdfium-win-x64.tgz`,
  4,031,929 bytes, SHA256
  `8d48973e3eb472dda0f5f2435e595ac043f9f2219dd71883392f549445afa8a5`.
- Runner: `windows-2022` (Windows Server 2022, Build 20348, VS2022 17.x);
  `windows-latest` is never used. GitHub's `windows-2022` image label is still
  mutable: the gate asserts OS build and VS major and records the exact
  observed compiler/SDK/Git/CMake/Ninja/Python versions, but does not yet pin their
  minor versions or a runner-image revision. Capture a Windows run before
  establishing those assertions. Rust 1.97.1, Inno Setup 6.7.1.

## Gates (all fail closed)

1. `assert-windows-toolchain-prototype.ps1` (workflow): asserts Server 2022
   caption/build/arch, `cl.exe` on PATH via the MSVC setup step, VS2022
   17.x via `vswhere`, `git`/`cmake`/`ninja`/`python` presence with logged versions,
   `VCPKG_ROOT` HEAD == pinned baseline with bootstrapped `vcpkg.exe`, and
   the prototype triplet env (`VCPKGRS_DYNAMIC=1`). Records
   `target/prototype-windows-ffmpeg/toolchain.json`.
2. `build-windows-ffmpeg-prototype.ps1`: asserts the source/baseline pins,
   the LGPL-only manifest (default library features + only the test-only
   `ffmpeg` tool feature), and the custom dynamic triplet; logs observed
   MSVC/SDK/tool versions (only OS build and VS major are currently asserted);
   verifies `VCPKG_ROOT` HEAD == baseline; clones and verifies the exact
   FFmpeg commit with repository-local `core.autocrlf=false` so its source
   contexts match vcpkg's LF patch blobs; processes patches sequentially in
   declared portfile order.
   The pinned vcpkg `0004-dependencies.patch`, `0007-fix-lib-naming.patch`
   and `0045-use-prebuilt-bin2c.patch` digests are checked; all three
   adaptations require exact source contexts and their generated replacements
   must apply after prior patches.
   The library-name adaptation preserves FFmpeg's newer dynamic zlib mapping
   while adding the `libmp3lame.lib` mapping. The 0045 adapter reads
   `ffbuild/common.mak` at its sequential patch stage, replaces the unique exact
   BIN2CEXE/BIN2C assignments with `BIN2C = bin2c`, and removes the exact
   BIN2CEXE host-link rule. Its deletion anchors come from the hash-verified
   baseline patch; the generated diff uses the actual source context, retaining
   the intervening `INSTALL_FILES` comment (context drift, not line endings).
   The hash-pinned `0047-fix-msvc-utf8.patch` is not applied: the pinned
   [upstream `configure`](https://github.com/FFmpeg/FFmpeg/blob/db8537d8cc7962bd3d07254d6ceff21d888817d4/configure) already has `check_allcflags -utf-8` in the MSVC block.
   Before omitting it at its sequential stage, the validator requires that
   exact unique block and unique helper definitions: `check_allcflags` calls
   both `check_cxxflags` and `check_cflags`, which test and add their flags.
   Baseline digest, configure digest, commit and context digests are recorded
   as upstream-supersession evidence, not as a successful patch application.
   `0052-fix-disable-unstable-swscale-link.patch` is also omitted only after
   verifying its baseline SHA-256 (`994fda85…cdff879`) and the pinned pristine
   `configure`, `libswscale/format.c` and `graph.c` blob hashes in `pins.json`.
   Unique function/dependency contexts establish the retained chroma/matrix
   helpers and private `decode_colors`; old `CONFIG_UNSTABLE` and
   `ff_sws_decode_colors` format contexts are absent. The graph uses
   `CONFIG_SWSCALE_VULKAN`, not `CONFIG_VULKAN`; Makefile checks retain compiled
   format/graph objects and component-gated Vulkan objects, while configure
   requires `swscale_vulkan_deps="vulkan unstable"`. No source text is executed.
   `unstable-swscale-supersession.json` records patch/source/context hashes,
   both commits and rationale; it also records sequential-stage file hashes
   after checking that earlier patches preserved this evidence. A failed patch
   application alone never authorizes omission.
   Missing, changed or duplicate contexts stop the run, with no 9.0.2 fallback.
   Per-patch evidence is recorded in `ffmpeg-source.json`. It derives
   a test-only overlay from the baseline port (rewrites to
   `vcpkg_from_git(URL https://github.com/FFmpeg/FFmpeg.git REF
   db8537d8cc7962bd3d07254d6ceff21d888817d4)`, validates the pinned helper
   signature and generated parameters, removes the archive hash, replaces only
   the `0004`, `0007` and `0045` PATCHES entries with their pinned-source
   replacements, omits validated 0047/0052 from PATCHES and overlay patch files,
   and marks the overlay `version-string` as the pinned commit);
   Before patching, the same adapter verifies pristine `configure` against
   `pins.vcpkg.ffmpeg_unstable_swscale_patch.source_sha256.configure`
   (`b7aef8b5…ad78cf`) and requires zero occurrences of both OpenGL options.
   It removes only the unique exact baseline else-branch line
   `set(OPTIONS "${OPTIONS} --disable-opengl")`: this pinned source rejects that
   unsupported no-op default. The `--enable-opengl` branch remains, so future
   OpenGL opt-in still fails closed. Missing, duplicate or changed baseline
   stanza/options, source drift, or source option support reject the rewrite.
   `portfile_transformation` in the source/overlay/build JSON records the
   configure and stanza hashes, removed option, zero counts and result. Source
   bundle verification independently rechecks the archived configure proof and
   the portfile outside the authorized fetch/PATCHES changes; the original
   baseline port and 0047/0052 provenance remain intact. This is not evidence
   of a successful native build, security status or licensing approval.
   It then runs `vcpkg install` with the prototype manifest, `--overlay-ports`
   (generated), `--overlay-triplets triplets/`,
   `--triplet x64-windows-prototype-dynamic`, and `--no-binarycaching`.
   Records the actual commit, tool versions, `ffmpeg -buildconf`/`-version`,
   overlay hashes, install log, and native DLL hashes in `ffmpeg-build.json`.
   Locates the test-only `ffmpeg.exe` tool, writes `ffmpeg-tool.path`, and
   exports `IRIS_PROTOTYPE_FFMPEG_EXE`. Incompatible patches or a failed
   build fail with per-patch/build evidence, never a fake success.
3. `stage-windows-pdfium-prototype.ps1`: verifies the pinned
   `pdfium-win-x64.tgz` size/SHA256 (or a caller-supplied
   `IRIS_PDFIUM_TARBALL` against the same pin), lists members with
   `tar -tzf` to `pdfium-members.txt` before extraction, refuses absolute
   or traversal members, requires `bin/pdfium.dll` (or `pdfium.dll`) plus
   `LICENSE`, `VERSION`, `args.gn`, and a complete `licenses/` closure, then
   extracts and stages `pdfium.dll` with `PROVENANCE.json`. A differing
   layout reports members and fails closed.
4. `stage-windows-prototype-app.ps1`: requires the FFmpeg gate + build
   evidence (`ffmpeg-source.json` clean, `ffmpeg-build.json` pinned
   commit/baseline/triplet, `ffmpeg-buildconf.txt`, `overlay.json`,
   `vcpkg-install.log`, `ffmpeg-tool.path`), resolves the actual prototype
   `VCPKG_INSTALLED_DIR`/`x64-windows-prototype-dynamic/bin` closure, sets
   `VCPKGRS_DYNAMIC=1` and the triplet env, builds `iris-ui-shell` with
   product defaults (no `minimal` variant) through `prototype-binding-build.py`
   (Python 3.11+ for `tomllib`). For the staged app command, the helper
   selects `iris-thumb-preview` too and enables upstream
   `ffmpeg-next/non-exhaustive-enums` so new FFmpeg enum constants do not stop
   compilation. Its unknown-value fallback is `unimplemented!()` and can panic;
   this is only a bounded fixture-smoke probe, not arbitrary-media compatibility
   or a production workaround. The default video/PDF/full-RAW Iris features stay
   enabled. The helper retains the original root `Cargo.lock` bytes in memory,
   prepares only the exact binding source/version/checksum transition, and refuses any
   unrelated package, dependency-edge or lock-metadata change before building
   with `--locked`. It restores the original bytes in `finally` on both success
   and failure. No Git override config or temporary lock is persisted. Abrupt
   process termination cannot run cleanup; use only the isolated CI checkout.
   `binding-override.json` records the exact unsigned source, the CLI-only enum
   compatibility feature and its panic limitation, original/temporary/restored
   lock hashes, build status and restoration result in the bounded
   audit report, and is copied into the app docs. This is test evidence, not
   release provenance approval. The staging script stages the complete FFmpeg DLL
   closure plus the pinned `pdfium.dll`, and emits hashes plus copied
   source/license/notice material and tested config evidence
   (`ffmpeg-buildconf.txt`, `ffmpeg-version.txt`, `overlay.json`,
   `ffmpeg copyright`, FFmpeg source `COPYING.*`, PDFium `LICENSE`/`VERSION`/
   `args.gn`/`licenses/`). Copies only; no licensing conclusion.
5. `inventory-windows-imports.ps1` (`param` first): recursively discovers
   every staged EXE/DLL via `dumpbin /DEPENDENTS`, resolves staged first,
   then the OS allowlist (`windows-system-dll-allowlist.txt`), then the VC
   redist allowlist (`windows-vc-redist-allowlist.txt`). Unknown CRTs are
   never treated as OS. Emits `IMPORT-CLOSURE.json` with per-binary imports,
   resolutions (`staged`/`os`/`vc-redist`/`unknown`), hashes, plus
   `packaged_files` hashes for every staged file. Fails on any unknown
   import.
6. `smoke-windows-installed-previews.ps1` with `preview-smoke/`
   (prototype-only smoke binary, explicit `video`+`pdf`+`raw-full`+
   `test-fixtures`, never `minimal`): requires `IMPORT-CLOSURE.json` and the
   prototype `ffmpeg.exe` tool path, builds a fresh Inno Setup installer
   from the prototype ISS, builds the detached smoke with the same prototype
   env (`VCPKGRS_DYNAMIC=1`, triplet, `VCPKG_INSTALLED_DIR`,
   `IRIS_PROTOTYPE_FFMPEG_EXE`) through `prototype-binding-build.py --preview-smoke`.
   This separate workspace exposes its existing `ffmpeg-next` wrapper as a direct
   dependency solely for explicit CLI compatibility-feature selection; the baseline
   lock adds that edge, not a new wrapper version. The helper validates only the
   pinned binding transition in this workspace's own `Cargo.lock`, builds with
   `--locked`, and restores original bytes on success, failure, or Python interruption.
   It emits separate `preview-smoke-binding-override.json` lock hashes, target,
   feature/limitation, build status and restoration evidence into the bounded audit
   report. No temporary lock or Cargo patch config is retained; forced process
   termination cannot run `finally`, so use an isolated checkout. The enum fallback
   can panic and does not establish arbitrary-media compatibility.
   The script silently installs to a fresh unique
   directory (refuses pre-existing dirs), verifies staged vs installed
   `Iris.exe`/`pdfium.dll` hashes and the DLL closure, runs the smoke from
   inside the installed dir with the installed `pdfium.dll` beside the exe
   and `PATH` prefixed (`--app-dir` = installed dir), then uninstalls and
   verifies no `Iris.exe`/`pdfium.dll`/FFmpeg DLLs remain. The smoke
   exercises video (deterministic `testsrc` via the prototype `ffmpeg.exe`
   only, no PATH fallback), generated PDF (mirrors
   `crates/thumb-preview/examples/pdf_thumbnail_smoke.rs`, proving the
   installed `pdfium.dll` loads), and synthetic RAW DNG via the default
   `raw-full` path. Writes `smoke-result.json` for the bounded evidence
   upload.

## Evidence boundaries

The offline fixtures check script behavior and bundle integrity; they do not
prove a Windows-native build, runtime DLL closure, PDFium loading, installer
behavior, or installed preview success. Those results require the path-scoped
workflow on the exact source snapshot and its retained evidence artifacts.
Only a completed native build and clean-installed smoke can establish those
prototype observations; neither those observations nor copied notices are a
licensing, signing, notarization, or SmartScreen conclusion.

The Windows OS and VC runtime import allowlists are documented starting sets.
They may be revised only from measured `dumpbin` evidence, never to turn an
unknown import into an assumed system dependency.

## Retained FFmpeg source material

The build creates `target/prototype-windows-ffmpeg/source-bundle/` before
installing FFmpeg. It contains:

- `ffmpeg-db8537d8cc7962bd3d07254d6ceff21d888817d4.tar`: a deterministic,
  uncompressed `git archive` of the exact upstream commit tree, with a fixed
  prefix and tar permissions. It reads the commit, not the patched worktree.
- `baseline/ports/ffmpeg/`: every tracked file from the exact pinned vcpkg
  baseline, including the original portfile, manifest and patches;
  `baseline/scripts/cmake/vcpkg_from_git.cmake` identifies the fetch helper.
  These are Git blob bytes, not potentially CRLF-converted checkout copies.
- `overlay/ffmpeg/`: the actual generated port, manifest and patch/config
  files used for the prototype. Baseline order and unrelated bytes are retained;
  only `0004-dependencies.patch`, `0007-fix-lib-naming.patch` and
  `0045-use-prebuilt-bin2c.patch` are replaced by pinned-source adaptations. The
  overlay portfile also omits only the unsupported default
  `--disable-opengl` option after a hash-verified configure/stanza proof.
  Superseded 0047/0052 are absent from both overlay PATCHES and overlay files;
  their exact Git blobs remain in `baseline/ports/ffmpeg/`. Each adapter checks its
  exact baseline patch digest and source context. The 0004 adapter preserves
  the newer MSVC zlib probe and applies the dependency/link changes; the 0007
  adapter preserves dynamic zlib library-name detection while adding
  `libmp3lame.lib`. The 0045 adapter selects the prebuilt `bin2c` and removes
  its host-link rule in `ffbuild/common.mak`, retaining the newer comment.
  Missing, duplicate or changed contexts fail before install;
  there is no fallback to FFmpeg 9.0.2.
- `build-inputs/`: pins, vcpkg manifest/configuration, custom dynamic triplet,
  source/overlay/build/toolchain scripts, the adapter and its offline
  regression tests, workflow and this recipe. The sibling `ffmpeg-build.json`,
  `ffmpeg-buildconf.txt` and `toolchain.json` evidence records the observed
  build/tools; runner/tool minor versions are not yet reproducibly pinned.

`MANIFEST.json` records SHA-256 and byte count for every included input, the
source/baseline commits, and separate baseline/overlay PATCHES blocks with
comments and exact order. It binds all three authorized patch replacements to
pinned baseline patch digests; verifies the untouched overlay patches against
their Git-blob copies (normalizing checkout line endings only); and requires
each adapted patch to change only its target (`configure` for 0004/0007,
`ffbuild/common.mak` for 0045) and retain its intended changes, including the
dynamic zlib mapping. Schema 5 separately records both supersessions and the
proof-bound default OpenGL option omission, including baseline hashes and
archived source/context hashes. Verification
reads exactly one regular member per evidence file from the pristine pinned
archive (without extracting it), repeats MSVC/helper and unstable-swscale
checks, and compares the recomputed evidence to the manifest. Rehashed metadata cannot substitute for
that source proof. Staging accepts only the pinned evidenced omissions and
verifies all required files, patches and hashes both before and after copying to app
`doc/ffmpeg-source-bundle/`. The prototype installer
recursively includes that directory; the evidence artifact retains both the
generated bundle and the staged app-doc copy. Missing or altered material
fails staging instead of silently omitting source.

This is FFmpeg source material and the prototype recipe, **not complete source
for other dependencies**, vcpkg tooling or SDKs. It makes no licensing/legal
compliance conclusion and does not change production packaging. Windows
bundle generation, native compilation and installed-package contents still
require evidence from the Windows prototype run.

## Non-goals

- No change to `release.yml`, production packaging scripts, product features,
  release artifacts, or installer behavior.
- No security or licensing conclusion. Records emitted here are evidence
  inputs only.
- No network fallback: unknown or incompatible runtime code fails instead
  of substituting another build.

## Focused offline checks

- `pwsh -File prototype/windows-ffmpeg/test-overlay-derivation.ps1` checks
  baseline port rewriting, parameter rejection, patch order and all three pinned
  patch replacements without downloading or building native code.
- `pwsh -File prototype/windows-ffmpeg/test-installed-smoke-lifecycle.ps1`
  checks bounded install/smoke/uninstall/cleanup fault handling.
- `python prototype/windows-ffmpeg/test-ffmpeg-dependency-patch.py` checks the
  pinned dependency and library-name configure adaptations (CRLF/LF), and
  the prebuilt-bin2c adaptation with the intervening comment and a
  `ffbuild/common.mak`-only applicable diff. It checks baseline hash rejection
  and fail-closed source drift using offline fixtures; the bin2c positive
  fixture uses a synthetic baseline, not upstream provenance. UTF-8 tests cover
  missing/changed/duplicate MSVC and helper contexts and baseline hash rejection.
- `python prototype/windows-ffmpeg/test-source-bundle.py` checks deterministic
  source bundles, all three patch adaptations/order, 0047/0052 baseline retention and
  overlay omission, rehashed supersession/source-context tampering, blob/hash integrity, missing or
  corrupted material and package/evidence wiring using synthetic local Git
  repositories. The fixtures are not upstream provenance and do not build
  Windows code. `test-overlay-derivation.ps1` also exercises exact PATCHES
  removal with quotes, comments, LF/CRLF and missing/duplicate entries.

These checks do not establish a Windows build or installed previews. The
workflow runs the offline checks before native gates; its evidence artifacts
are required to review any Windows-native result.
