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
- Runner, vcpkg, FFmpeg, and PDFium pins live in `pins.json` (single source
  of truth). Every script fails closed on drift and never falls back to
  FFmpeg 9.0.2, an unknown runtime, or a minimal build.

## Pins (single source of truth: `pins.json`)

- FFmpeg source: `https://github.com/FFmpeg/FFmpeg.git` at
  `db8537d8cc7962bd3d07254d6ceff21d888817d4`. The official
  [FFmpeg download page](https://ffmpeg.org/download.html) lists this GitHub
  repository as a mirror of its main repository. Fetch and detached-checkout
  HEAD verification must succeed for that exact commit; an unavailable mirror
  or commit stops the run, without a source or version fallback.
- vcpkg / tooling baseline: `b8dfe957ec8b5c0ef5c4b31eea594b8486af744f`
  (`vcpkg.json` `builtin-baseline` and `vcpkg-configuration.json` carry the
  same baseline; the scripts fail closed on drift).
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
   FFmpeg commit; processes patches sequentially in declared portfile order.
   The pinned vcpkg `0004-dependencies.patch` and `0007-fix-lib-naming.patch`
   digests are checked; both configure adaptations require exact source
   contexts and their generated replacements must apply after prior patches.
   The library-name adaptation preserves FFmpeg's newer dynamic zlib mapping
   while adding the `libmp3lame.lib` mapping. Drift stops the run, with no 9.0.2
   fallback. Per-patch evidence is recorded in `ffmpeg-source.json`. It derives
   a test-only overlay from the baseline port (rewrites to
   `vcpkg_from_git(URL https://github.com/FFmpeg/FFmpeg.git REF
   db8537d8cc7962bd3d07254d6ceff21d888817d4)`, validates the pinned helper
   signature and generated parameters, removes the archive hash, replaces only
   the `0004` and `0007` PATCHES entries with their pinned-source replacements,
   and marks the overlay `version-string` as the pinned commit);
   then runs `vcpkg install` with the prototype manifest, `--overlay-ports`
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
   product defaults (no `minimal` variant), stages the complete FFmpeg DLL
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
   `IRIS_PROTOTYPE_FFMPEG_EXE`), silently installs to a fresh unique
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
  files used for the prototype. The ordered baseline patch series is retained;
  only `0004-dependencies.patch` and `0007-fix-lib-naming.patch` are replaced
  by their pinned-source configure-only adaptations. Each adapter checks its
  exact baseline patch digest and source context. The 0004 adapter preserves
  the newer MSVC zlib probe and applies the dependency/link changes; the 0007
  adapter preserves dynamic zlib library-name detection while adding
  `libmp3lame.lib`. Missing, duplicate or changed contexts fail before install;
  there is no fallback to FFmpeg 9.0.2.
- `build-inputs/`: pins, vcpkg manifest/configuration, custom dynamic triplet,
  source/overlay/build/toolchain scripts, the adapter and its offline
  regression tests, workflow and this recipe. The sibling `ffmpeg-build.json`,
  `ffmpeg-buildconf.txt` and `toolchain.json` evidence records the observed
  build/tools; runner/tool minor versions are not yet reproducibly pinned.

`MANIFEST.json` records SHA-256 and byte count for every included input, the
source/baseline commits, and separate baseline/overlay PATCHES blocks with
comments and exact order. It binds both authorized patch replacements to
pinned baseline patch digests; verifies the untouched overlay patches against
their Git-blob copies (normalizing checkout line endings only); and requires
each adapted patch to change `configure` alone and retain its intended
changes, including the dynamic zlib mapping. Staging verifies all required
files, patches and hashes both before and after copying to app
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
  baseline port rewriting, parameter rejection, patch order and both pinned
  patch replacements without downloading or building native code.
- `pwsh -File prototype/windows-ffmpeg/test-installed-smoke-lifecycle.ps1`
  checks bounded install/smoke/uninstall/cleanup fault handling.
- `python prototype/windows-ffmpeg/test-ffmpeg-dependency-patch.py` checks the
  pinned dependency and library-name configure adaptations, CRLF/LF patch
  applicability, baseline hashes, and fail-closed source drift using offline
  fixtures.
- `python prototype/windows-ffmpeg/test-source-bundle.py` checks deterministic
  source bundles, both patch adaptations/order, blob/hash integrity, missing or
  corrupted material and package/evidence wiring using synthetic local Git
  repositories. The fixtures are not upstream provenance and do not build
  Windows code.

These checks do not establish a Windows build or installed previews. The
workflow runs the offline checks before native gates; its evidence artifacts
are required to review any Windows-native result.
