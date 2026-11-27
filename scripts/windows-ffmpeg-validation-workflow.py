#!/usr/bin/env python3
"""Authoritative offline contract for the Windows native-preview validation workflow.

The workflow is reusable (workflow_call only) and report-only by default; a
release caller may opt into a bounded native asset export. It never promotes a
release itself. This module is the single source of truth for its fail-closed shape:
exact-SHA checkout and immutable third-party action refs, all offline gates,
pinned vcpkg HEAD assertion, verified source acquisition, derived overlay,
stock bindings, PDFium attestation, PE import inventory, clean-install preview
smoke, bounded JSON evidence and cleanup of native bytes. It performs no build
and no network access.
"""

import argparse
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/windows-ffmpeg-validation.yml"

BASELINE = "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f"
SOURCE_VERSION = "8.1.3"
PDFIUM_RELEASE = "chromium/8138"

OFFLINE_GATES = (
    "python3 scripts/test-windows-ffmpeg-provenance.py",
    "python3 scripts/test-windows-ffmpeg-source.py",
    "python3 scripts/test-windows-ffmpeg-overlay.py",
    "python3 scripts/test-windows-release-source-bundle.py",
    "python3 scripts/windows-ffmpeg-validation-config.py",
    "python3 scripts/test-windows-ffmpeg-validation-config.py",
    "python3 scripts/test-windows-ffmpeg-validation-workflow.py",
    "python3 scripts/test-windows-pdfium-candidate.py",
)

# One RUNNER_TEMP stage binds both release binaries, the vcpkg DLL closure and
# the attested PDFium DLL. The preview example probes `rustc --version`, so the
# real compiler and its DLLs are staged too; no toolchain path is added to PATH.
STAGE_MARKERS = (
    "cargo build --release -p iris-ui-shell --locked",
    "cargo build --release -p iris-thumb-preview --example preview_corpus_smoke --locked",
    '$stage = Join-Path $env:RUNNER_TEMP "iris-runtime-stage"',
    '"IRIS_VALIDATION_PDFIUM_RUNTIME=$runtime" >> $env:GITHUB_ENV',
    '"IRIS_VALIDATION_STAGE=$stage" >> $env:GITHUB_ENV',
    'Copy-Item -LiteralPath $pdfium -Destination (Join-Path $stage "pdfium.dll") -Force',
    'Copy-Item -LiteralPath $rustcExe -Destination (Join-Path $stage "rustc.exe") -Force',
    '$smokeExe = Join-Path $env:IRIS_VALIDATION_STAGE "preview_corpus_smoke.exe"',
    "& $smokeExe $corpus",
    "$exitCode = $LASTEXITCODE",
    '$status = "fail"',
    'if ($exitCode -ne 0) { throw "installed preview smoke failed with exit code $exitCode" }',
    '"preview-smoke.json"',
)

# The staged smoke emits exactly one correctness and five latency JSON lines.
# Retain every parsed record (not just the diagnostic tail) under a bounded
# serialized size, and refuse a successful run with a different record shape.
PREVIEW_RECORD_MARKERS = (
    '$previewRecords = @()',
    'foreach ($line in ($smokeOutput -split "`r?`n")) {',
    '$line.StartsWith("PREVIEW_CORRECTNESS ")',
    '$line.StartsWith("PREVIEW_LATENCY ")',
    '$previewRecords += ($json | ConvertFrom-Json)',
    'invalid preview record JSON in smoke output: $json',
    '$correctnessRecords++',
    '$latencyRecords++',
    '$previewRecords.Count -ne 6',
    '$correctnessRecords -ne 1',
    '$latencyRecords -ne 5',
    'output_tail = $smokeOutput',
    'preview_records = $previewRecords',
    'record_count = $previewRecords.Count',
    '$serialized = $record | ConvertTo-Json -Depth 8 -Compress',
    '$serialized.Length -gt 200000',
    'preview smoke evidence exceeds 200000 characters',
)

# dumpbin must be checked for failure, and every non-system import must be
# resolved in the stage and then queued for its own inventory.
PE_IMPORT_MARKERS = (
    "dumpbin /DEPENDENTS $binary",
    '$systemDirs = @(',
    '(Join-Path $env:SystemRoot "System32")',
    'foreach ($sysDir in $systemDirs) {',
    '(Join-Path $env:IRIS_VALIDATION_STAGE "pdfium.dll")',
    '$roots += Join-Path $stage $shippedName',
    '$rootNames = @($roots | ForEach-Object { [IO.Path]::GetFileName($_) })',
    'roots = @($rootNames)',
    'if (!$visited.ContainsKey("pdfium.dll")) { throw "staged pdfium.dll missing from PE inventory" }',
    'if (Test-Path -LiteralPath (Join-Path $sysDir $import) -PathType Leaf) { $isSystem = $true; break }',
    '$isSystem = $import.StartsWith("api-ms-win-") -or $import.StartsWith("ext-ms-win-")',
    "$candidate = Join-Path $stage $import",
    'if (!(Test-Path -LiteralPath $candidate -PathType Leaf)) {',
    'if ($LASTEXITCODE -ne 0) { throw "dumpbin /DEPENDENTS failed for $name" }',
    "while ($queue.Count -gt 0)",
    "$queue.Enqueue($candidate)",
    '$unresolved += "$name -> $import"',
    'if ($unresolved.Count -gt 0) { throw ("unresolved staged PE imports: "',
    '"pe-import-closure.json"',
)


# VC redistributable imports are a separate resolution from Windows OS DLLs.
# The checked-in allowlists, the ordered classifier, the runner-negative
# self-test and the JSON evidence fields are all fail-closed markers.
OS_IMPORT_ALLOWLIST = "packaging/windows/validation/windows-system-dll-allowlist.txt"
VC_IMPORT_ALLOWLIST = "packaging/windows/validation/windows-vc-redist-allowlist.txt"

RUNNER_PROBE_DIRS = (
    'foreach ($dir in @(\n'
    '              (Join-Path $env:SystemRoot "System32"),\n'
    '              (Join-Path $env:SystemRoot "SysWOW64"),\n'
    '              $env:SystemRoot\n'
    '            )) {'
)

VC_IMPORT_MARKERS = (
    OS_IMPORT_ALLOWLIST,
    RUNNER_PROBE_DIRS,
    VC_IMPORT_ALLOWLIST,
    '$osAllowPath = Join-Path $env:GITHUB_WORKSPACE "' + OS_IMPORT_ALLOWLIST + '"',
    '$vcAllowPath = Join-Path $env:GITHUB_WORKSPACE "' + VC_IMPORT_ALLOWLIST + '"',
    "function Read-ImportAllowlist",
    "function Test-AllowedPattern",
    "function Test-AllowedOSDll",
    "function Test-AllowedVcRedist",
    "function Test-LooksLikeCrt",
    "function Resolve-ImportResolution",
    'if ($Import.StartsWith("api-ms-win-") -or $Import.StartsWith("ext-ms-win-")) { return "system" }',
    'if (Test-Path -LiteralPath $candidate -PathType Leaf) { return "stage" }',
    'if (Test-AllowedVcRedist $Import) { return "vc-redist" }',
    'if (Test-LooksLikeCrt $Import) { return "unresolved" }',
    'if ($inSystemDir -and (Test-AllowedOSDll $Import)) { return "os" }',
    'return "unresolved"',
    '$probeStage = Join-Path $env:RUNNER_TEMP "iris-vc-classifier-probe"',
    '$vcSelfTest = @(',
    'foreach ($name in @("vcruntime140.dll", "vcruntime140_1.dll")) {',
    'if (!(Test-AllowedVcRedist $name)) { throw "VC self-test missing from VC allowlist: $name" }',
    'if (Test-AllowedOSDll $name) { throw "VC self-test wrongly in OS allowlist: $name" }',
    '$forcedVc = Resolve-ImportResolution -Import $name -Stage $probeStage -inSystemDir $true',
    '$observed = Resolve-ImportResolution -Import $name -Stage $probeStage -inSystemDir $runnerPresent',
    'if ($forcedVc -ne "vc-redist" -or $observed -ne "vc-redist") {',
    'runner_present = $runnerPresent',
    '$resolution = Resolve-ImportResolution -Import $import -Stage $stage -inSystemDir $isSystem',
    '} elseif ($resolution -eq "vc-redist") {',
    'resolution_values = @("system", "stage", "vc-redist", "os", "unresolved")',
    'vc_imports = @($vcImports | Sort-Object -Unique)',
    'vc_self_test = @{ status = "pass"; dlls = @($vcSelfTest) }',
)

# Both staged-runtime results must be read into the single JSON manifest.
EVIDENCE_MARKERS = (
    'report(ev / "pe-import-closure.json", json.loads)',
    'report(ev / "preview-smoke.json", json.loads)',
    "pe_import_closure=closure",
    "preview_smoke=smoke",
    'report(ev / "pdfium-8179-candidate.json", json.loads)',
    "pdfium_8179_candidate=candidate",
    'source_bundle = report(ev / "ffmpeg-source-bundle.json", json.loads)',
    "source_bundle=source_bundle,",
)

CLEANUP_MARKER = (
    "rm -rf build/validation/ffmpeg-src build/validation/overlay "
    "target/debug target/release .vcpkg-validation/installed"
)
PATH_MARKER = "$env:PATH = (@($stage) + $systemPath) -join [IO.Path]::PathSeparator"
VCPKG_EMPTY_PATH_GUARD = (
    'if (@($manifestRoot, $installRoot, $tripletDir, $env:IRIS_VALIDATION_OVERLAY)'
    '.Where({ [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) { throw "empty vcpkg path" }'
)

# The PDFium attestation step must verify the exact asset against the pinned
# bundle, identity and source before extraction. Check these on executable
# lines, so a comment describing verification cannot stand in for the command.
ATTESTATION_MARKERS = (
    '"pdfium-attestation.json"',
    "18267",
    "7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7",
    '$pin.attestation_bytes',
    '$pin.attestation_sha256',
    'Get-FileHash -Algorithm SHA256 -LiteralPath "$bundle.part"',
    "attestations: read",
    "GH_TOKEN: ${{ github.token }}",
    "gh attestation verify $asset --repo $pin.repo --bundle $bundle",
    "--signer-workflow $pin.signer_workflow",
    "--signer-digest $pin.signer_digest",
    "--source-digest $pin.source_digest",
    "--source-ref $pin.source_ref",
    "PDFium release attestation verification failed",
)


# The opt-in 8179 candidate runs after the production staged smoke, from one
# helper that owns the size/SHA/attestation/native-smoke gate; the workflow must
# never duplicate its gh command or touch a shipped pin.
CANDIDATE_MARKERS = (
    "- name: Verify date-later Windows PDFium candidate under the production gate",
    "if: ${{ inputs.validate_pdfium_candidate }}",
    "python3 scripts/windows-pdfium-candidate.py",
    '"pdfium-8179-candidate.json"',
    "build/validation/evidence/pdfium-8179-candidate.json",
)

# The shippable inventory is derived only from the product binary, the vcpkg
# DLL closure and the attested PDFium DLL. Preview helpers and toolchain DLLs
# stay in the smoke stage and cannot enter the rehearsal ZIP.
PRODUCT_INVENTORY_MARKERS = (
    '$productNames = @("Iris.exe") + @($dlls | ForEach-Object { $_.Name }) + @("pdfium.dll")',
    'if ($productNames.Count -lt 3) { throw "product inventory empty" }',
    'foreach ($helper in @("rustc.exe", "preview_corpus_smoke.exe")) {',
    'if (!(Test-Path -LiteralPath (Join-Path $stage $required) -PathType Leaf)) { throw "required product file missing: $required" }',
    '$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $copy).Hash.ToLowerInvariant()',
    '$inventory = @{ roots = @($stage); files = @($productFiles) }',
    '"product-inventory.json"',
)

# The clean directory is rebuilt from inventory names only, the ZIP is
# assembled by the fail-closed script, and its paths are exported for the next
# step and for cleanup.
PRODUCT_PACKAGE_MARKERS = (
    "- name: Assemble clean product ZIP payload",
    '$clean = Join-Path $env:RUNNER_TEMP "iris-product-clean"',
    '$source = Join-Path $env:IRIS_VALIDATION_STAGE $name',
    'Copy-Item -LiteralPath $source -Destination (Join-Path $clean $name) -Force',
    '"IRIS_PRODUCT_STAGE=$clean" >> $env:GITHUB_ENV',
    '"IRIS_PRODUCT_INVENTORY=$inventoryPath" >> $env:GITHUB_ENV',
    '"IRIS_PRODUCT_ZIP=$zipPath" >> $env:GITHUB_ENV',
    '$env:IRIS_PRODUCT_STAGE = $clean',
    '$env:IRIS_PRODUCT_INVENTORY = $inventoryPath',
    '$env:IRIS_PRODUCT_ZIP = $zipPath',
    "pwsh scripts/windows-product-package.ps1",
    'if ($LASTEXITCODE -ne 0) { throw "product package rehearsal failed" }',
)

# The third-party notice must be staged with the shippable files and copied
# into the clean ZIP stage by the assembling step, not merely exist in the
# checkout. Both copies come from the same checked-in, verified source bytes.
PRODUCT_NOTICE_MARKERS = (
    '$noticeSource = Join-Path $env:GITHUB_WORKSPACE "packaging/windows/ffmpeg-README.md"',
    'if (!(Test-Path -LiteralPath $noticeSource -PathType Leaf)) { throw "FFmpeg notice missing: $noticeSource" }',
    'if ((Get-Item -LiteralPath $noticeSource).Length -le 0) { throw "FFmpeg notice empty: $noticeSource" }',
    'Copy-Item -LiteralPath $noticeSource -Destination (Join-Path $stage "FFMPEG-NOTICE.md") -Force',
    '$productNames += "FFMPEG-NOTICE.md"',
    'Copy-Item -LiteralPath $noticeSource -Destination (Join-Path $clean "FFMPEG-NOTICE.md") -Force',
)


# The source/notice bundle is built from already verified runner inputs and
# inventoried with the product binaries. The helper writes only allowlisted
# flat files; every name must survive into the ZIP, installer list and native
# evidence through the one product inventory.
SOURCE_BUNDLE_OUTPUTS = (
    "ffmpeg-8.1.3.tar.xz",
    "ffmpeg-legal-notices.tar.gz",
    "ffmpeg-build-materials.tar.gz",
    "pdfium-notices.tar.gz",
    "ffmpeg-source-bundle.json",
)
SOURCE_BUNDLE_MARKERS = (
    "$sourceBundle = @(",
) + SOURCE_BUNDLE_OUTPUTS + (
    '$sourceArchive = Join-Path $env:RUNNER_TEMP "ffmpeg-8.1.3.tar.xz"',
    '$pdfiumAsset = Join-Path $env:RUNNER_TEMP "pdfium-win-x64.tgz"',
    '$verifiedSource = Join-Path $env:IRIS_VALIDATION_SOURCE "ffmpeg-8.1.3"',
    '$bundleReport = Join-Path $env:IRIS_VALIDATION_EVIDENCE "ffmpeg-source-bundle.json"',
    "python3 scripts/windows-release-source-bundle.py build",
    "--source-archive $sourceArchive",
    "--source-tree $verifiedSource",
    '--gpg-dir "build/validation/gpg"',
    "--overlay $env:IRIS_VALIDATION_OVERLAY",
    "--pdfium-asset $pdfiumAsset",
    '--pdfium-extract (Join-Path $env:RUNNER_TEMP "pdfium-extract")',
    "--notice $noticeSource",
    "--output $stage",
    "--report $bundleReport",
    'if ($LASTEXITCODE -ne 0) { throw "source and notice bundle derivation failed" }',
    "foreach ($name in $sourceBundle) {",
    'throw "source/notice bundle missing from stage: $name"',
    'throw "source/notice bundle empty: $name"',
    "$productNames += $sourceBundle",
    'foreach ($name in @("ffmpeg-8.1.3.tar.xz", "ffmpeg-legal-notices.tar.gz", "ffmpeg-build-materials.tar.gz", "pdfium-notices.tar.gz", "ffmpeg-source-bundle.json")) {',
    'throw "source/notice bundle missing from product inventory: $name"',
    'source_bundle = report(ev / "ffmpeg-source-bundle.json", json.loads)',
    "source_bundle=source_bundle,",
)


# The release packager is exercised against the same clean stage, with a real
# compiler. The default discards its output after one bounded JSON evidence
# upload; a gated release caller may export the verified assets instead.
RELEASE_PACKAGE_MARKERS = (
    "- name: Install Inno Setup for release package rehearsal",
    "choco install innosetup --version=6.7.1 --allow-downgrade --yes --no-progress",
    "- name: Exercise release packaging from exact product inventory",
    "$env:IRIS_RELEASE_OUTPUT_DIR = $releaseDir",
    "pwsh scripts/package-windows.ps1",
    'if ($result.status -ne "pass") { throw "release package result is not pass" }',
    'if ($result.installer_source_count -ne $expectedNames.Count) { throw "installer source count mismatch" }',
    '"release-package-rehearsal.json"',
    'report(ev / "release-package-rehearsal.json", json.loads)',
    "release_package_rehearsal=release_package",
    "build/validation/evidence/release-package-rehearsal.json",
    "Copy-Item -LiteralPath $env:IRIS_PRODUCT_INVENTORY",
    '"${RUNNER_TEMP:-}/iris-release-package"',
)


# The ZIP is read in place: entry names, SHA-256 digests and byte counts must
# match the inventory exactly. No preview is run from the ZIP.
PRODUCT_ZIP_MARKERS = (
    "- name: Verify product ZIP entry inventory",
    "Add-Type -AssemblyName System.IO.Compression.FileSystem",
    '$archive = [IO.Compression.ZipFile]::OpenRead($env:IRIS_PRODUCT_ZIP)',
    "foreach ($entry in $archive.Entries) {",
    'if (!$expected.ContainsKey($key)) { throw "unexpected zip entry: $name" }',
    'if ($hash -ne $expected[$key].sha256) { throw "zip entry hash mismatch: $name" }',
    'if (!$expected.ContainsKey("ffmpeg-notice.md")) { throw "FFmpeg notice missing from product inventory" }',
    'if (!$equal) { throw "product zip entry inventory mismatch" }',
    '"product-package-rehearsal.json"',
)

PRODUCT_EVIDENCE_MARKERS = (
    'report(ev / "product-package-rehearsal.json", json.loads)',
    "product_package_rehearsal=package",
    "build/validation/evidence/product-package-rehearsal.json",
    "build/validation/evidence/product-inventory.json",
)

PRODUCT_CLEANUP_MARKERS = (
    '"${IRIS_PRODUCT_STAGE:-}"',
    '"${RUNNER_TEMP:-}/iris-product-clean"',
    '"${IRIS_PRODUCT_ZIP:-}"',
    '"${RUNNER_TEMP:-}/iris-product-rehearsal.zip"',
    'if [ -n "$native_file" ] && [ -f "$native_file" ]; then rm -f "$native_file"; fi',
)

# The reviewed native closure evidence is exported separately, gated by the
# same input. Both gated artifacts stay flat and JSON-only where appropriate;
# the default report-only path still uploads only the bounded JSON report.
NATIVE_EVIDENCE_ARTIFACT_NAME = "windows-native-evidence"
NATIVE_EVIDENCE_BASENAMES = (
    "windows-native-evidence.json",
    "validation-inputs.json",
    "pe-import-closure.json",
    "product-inventory.json",
    "release-package-result.json",
)
NATIVE_EVIDENCE_ROOT = "build/validation/evidence"
NATIVE_EVIDENCE_PREPARE_MARKER = "python3 scripts/windows-native-evidence.py prepare"

REQUIRED_MARKERS = (
    "workflow_call:",
    "ref: ${{ github.sha }}",
    "persist-credentials: false",
    "git -C $vcpkgRoot rev-parse HEAD",
    BASELINE,
    "scripts/windows-ffmpeg-source.py stage",
    "scripts/windows-ffmpeg-overlay.py derive",
    'Join-Path $env:IRIS_VALIDATION_SOURCE "ffmpeg-8.1.3"',
    'Join-Path $overlay "ffmpeg"',
    "VCPKG_OVERLAY_TRIPLETS",
    'Join-Path $env:VCPKG_INSTALLED_DIR "x64-windows-iris-dynamic/bin"',
    "IRIS_VALIDATION_TRIPLET: x64-windows-iris-dynamic",
    "Get-FileHash -Algorithm SHA256",
    SOURCE_VERSION,
    PDFIUM_RELEASE,
    "dumpbin",
    "preview_corpus_smoke",
    "actions/upload-artifact@",
    "if: always()",
    "rm -rf",
    "ev.mkdir(parents=True, exist_ok=True)",
    NATIVE_EVIDENCE_PREPARE_MARKER,
    "python3 scripts/windows-release-source-bundle.py build",
    "$gpgDir = \"build/validation/gpg\"",
    "& gpg --homedir $gpgHome --batch --import $key",
    "$gpgHome = \"$gpgDir/home\"",
    '$manifestRoot = Join-Path $env:GITHUB_WORKSPACE "packaging/windows/validation"',
    '$installRoot = Join-Path $env:IRIS_VALIDATION_VCPKG_ROOT "installed"',
    '"--x-manifest-root=$manifestRoot"',
    '"--x-install-root=$installRoot"',
    'empty vcpkg path',
    '.Where({ [string]::IsNullOrWhiteSpace($_) }).Count -gt 0',
    '"status": "not_run"',
) + ATTESTATION_MARKERS + STAGE_MARKERS + PREVIEW_RECORD_MARKERS + PE_IMPORT_MARKERS + EVIDENCE_MARKERS + VC_IMPORT_MARKERS + CANDIDATE_MARKERS + (
    CLEANUP_MARKER, PATH_MARKER,
) + PRODUCT_INVENTORY_MARKERS + PRODUCT_PACKAGE_MARKERS + PRODUCT_NOTICE_MARKERS + (
    PRODUCT_EVIDENCE_MARKERS + PRODUCT_CLEANUP_MARKERS + SOURCE_BUNDLE_MARKERS
) + PRODUCT_ZIP_MARKERS + RELEASE_PACKAGE_MARKERS

# Only these exact action refs may appear in the validation workflow. The
# trailing `# version` text on a `uses:` line is documentation, not part of the
# ref; GitHub executes the immutable SHA. Mutable tags and branch names are
# forbidden on the release path, so the closed allowlist names SHAs only.
ALLOWED_ACTIONS = frozenset((
    "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
    "ilammy/msvc-dev-cmd@0b201ec74fa43914dc39ae48a89fd1d8cb592756",
    "dtolnay/rust-toolchain@df2a88e9fe4098bad09cb42c3dec7187f15861fc",
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
))
UPLOAD_ACTION_PREFIX = "actions/upload-artifact@"
UPLOAD_EXPORT_INPUT = "inputs.export_release_assets"
CANDIDATE_INPUT = "inputs.validate_pdfium_candidate"
EXPORT_ARTIFACT_NAME = "windows-release-assets"
EXPORT_ASSET_BASENAMES = (
    "Iris-windows.zip",
    "Iris-windows-setup.exe",
    "release-package-result.json",
    "product-inventory.json",
)
EXPORT_ARTIFACT_ROOT = "${{ runner.temp }}/iris-release-package"
# Resolve action references syntactically, not by line position: block,
# spaced, quoted and flow-mapping spellings all yield the same reference.
ACTION_REF_RE = re.compile(r"['\"]?uses['\"]?\s*:\s*['\"]?([^\s,'\"}#]+)")
INLINE_PATH_RE = re.compile(r"['\"]?paths?['\"]?\s*:\s*['\"]?([^'\"},]+)")
FLOW_STEP_RE = re.compile(r"^\s*-\s*\{", re.MULTILINE)
# The workflow uses only simple block mappings. Explicit YAML keys/values and
# split key/value spellings can hide a `uses` key from the scanners above, so
# the whole syntax class is refused on executable lines.
EXPLICIT_KEY_RE = re.compile(r"^\s*(?:-\s*)?[?:](?:\s|$)")
# An anchor or alias in key position can resolve to `uses` even though the
# line has no literal `uses` token. This workflow uses neither in mappings.
ANCHOR_KEY_RE = re.compile(r"^\s*(?:-\s*)?[&*]\S")
# YAML tags on a key are another way to spell a key without a literal token.
TAG_KEY_RE = re.compile(r"^\s*(?:-\s*)?!")
# PowerShell variable names are case-insensitive; $HOME is read-only. A whole
# HOME token on an executable line is forbidden regardless of the cmdlet,
# alias, abbreviation or scope spelling used. The only exception is the exact
# relative GPG path literal, so adding another spelling requires an explicit
# contract change rather than slipping past the guard.
HOME_VARIABLE_RE = re.compile(r"\$\{?\s*(?:[A-Za-z_][\w-]*:)?\s*home\b", re.IGNORECASE)
HOME_TOKEN_RE = re.compile(r"(?<![\w])home(?![\w])", re.IGNORECASE)
ALLOWED_GPG_HOME_PATH = '"$gpgDir/home"'


def home_forbidden(line):
    """Reject any HOME spelling except the one allowed relative GPG path."""
    return bool(HOME_VARIABLE_RE.search(line)
                or HOME_TOKEN_RE.search(line.replace(ALLOWED_GPG_HOME_PATH, "")))
# YAML double-quoted scalars resolve numeric escapes before GitHub reads a
# mapping key, so decode them before any action reference is scanned.
YAML_ESCAPE_RE = re.compile(r"\\(?:u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|x[0-9a-fA-F]{2})")

FORBIDDEN_MARKERS = (
    # Mutable refs previously used on this path stay forbidden even when the
    # ref parser is bypassed by a future YAML spelling.
    "actions/checkout@v4",
    "ilammy/msvc-dev-cmd@v1",
    "dtolnay/rust-toolchain@master",
    "actions/upload-artifact@v4",
    "workflow_dispatch:",
    "  pull_request:",
    "  push:",
    "IRIS_BUILD_VARIANT: minimal",
    "release.yml",
    "db8537d8",
    "9.1.0",
    "--enable-gpl",
    "--enable-nonfree",
)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def permissions_read_only(text):
    """Require contents:read and reject any non-read permission at any level."""
    lines = text.splitlines()
    seen_contents_read = False
    for i, line in enumerate(lines):
        if re.match(r"^\s*permissions:\s*\S+", line):
            return False, f"inline permissions are not allowed: {line.strip()}"
        if not re.match(r"^\s*permissions:\s*$", line):
            continue
        base = len(line) - len(line.lstrip())
        j = i + 1
        while j < len(lines):
            nxt = lines[j]
            if not nxt.strip():
                j += 1
                continue
            ind = len(nxt) - len(nxt.lstrip())
            if ind <= base:
                break
            m = re.match(r"^\s*([a-z][a-z-]*):\s*(\S+)\s*$", nxt)
            if not m:
                return False, f"unparsed permission entry: {nxt.strip()}"
            key, value = m.group(1), m.group(2)
            if value not in ("read", "none"):
                return False, f"permission {key}: {value} is not read-only"
            if key == "contents" and value == "read":
                seen_contents_read = True
            j += 1
    if not seen_contents_read:
        return False, "workflow must declare contents: read"
    return True, ""


def decode_yaml_escapes(line):
    r"""Resolve numeric escapes in quoted keys so `\uXXXX` cannot hide a key."""
    def repl(match):
        return chr(int(match.group(0)[2:], 16))
    return YAML_ESCAPE_RE.sub(repl, line)


def action_refs(line):
    """Return every action reference on a line, independent of YAML spelling."""
    return ACTION_REF_RE.findall(decode_yaml_escapes(line))


def upload_paths(text):
    """Return (step_count, paths) for every upload-artifact step, inline or block.

    Action references are scanned across each line regardless of YAML
    spelling; inline flow-mapping `path:` values and following `path: |` block
    scalars are both collected so no upload form can evade the guard.
    """
    lines = [decode_yaml_escapes(line) for line in text.splitlines()]
    paths = []
    steps = 0
    for i, line in enumerate(lines):
        uploads = [ref for ref in action_refs(line)
                   if ref.startswith(UPLOAD_ACTION_PREFIX)]
        if not uploads:
            continue
        steps += len(uploads)
        # A flow-style step carries its `with:` paths on the same line.
        paths.extend(value.strip() for value in INLINE_PATH_RE.findall(line))
        base = len(line) - len(line.lstrip())
        block = []
        j = i + 1
        while j < len(lines):
            nxt = lines[j]
            stripped = nxt.strip()
            ind = len(nxt) - len(nxt.lstrip())
            if stripped.startswith("- ") and ind <= base:
                break
            block.append(nxt)
            j += 1
        for k, bl in enumerate(block):
            s = bl.strip()
            if not s.startswith("path:"):
                continue
            value = s[len("path:"):].strip()
            if value in ("|", "|-", "|+", ">", ">-"):
                pi = len(bl) - len(bl.lstrip())
                m = k + 1
                while m < len(block):
                    entry = block[m]
                    if not entry.strip():
                        m += 1
                        continue
                    if (len(entry) - len(entry.lstrip())) <= pi:
                        break
                    paths.append(entry.strip())
                    m += 1
            elif value:
                paths.append(value.strip("\"'"))
    return steps, paths


def indent_of(line):
    return len(line) - len(line.lstrip())


def upload_step_records(text):
    """Return one record per upload-artifact step, fail-closed by shape.

    Each record carries the action reference, `if:` condition, artifact name,
    `with:` flags and paths. Step boundaries come from indentation, so an
    inserted upload cannot hide inside another step or escape classification.
    """
    lines = [decode_yaml_escapes(line) for line in text.splitlines()]
    records = []
    for i, line in enumerate(lines):
        refs = [ref for ref in action_refs(line)
                if ref.startswith(UPLOAD_ACTION_PREFIX)]
        if not refs:
            continue
        action_ind = indent_of(line)
        start = i
        while start > 0:
            prev = lines[start - 1]
            if prev.strip().startswith("- ") and indent_of(prev) <= action_ind:
                start -= 1
                break
            if prev.strip() and indent_of(prev) < action_ind:
                start -= 1
                break
            start -= 1
        end = i + 1
        while end < len(lines):
            nxt = lines[end]
            if nxt.strip() and indent_of(nxt) < action_ind:
                break
            if nxt.strip().startswith("- ") and indent_of(nxt) <= action_ind:
                break
            end += 1
        block = lines[start:end]
        body_ind = max(action_ind, indent_of(block[0]) + 2)
        condition = ""
        name = ""
        flags = {}
        with_ind = None
        for bl in block:
            stripped = bl.strip()
            if not stripped or stripped.startswith("#") or indent_of(bl) < body_ind:
                continue
            key = re.match(
                r"^(?:-\s*)?['\"]?(if|name|with)['\"]?\s*:\s*(.*)$", stripped)
            if key and indent_of(bl) == body_ind:
                field, value = key.group(1), key.group(2).strip()
                if field == "if":
                    condition = value.strip("\"'")
                elif field == "name":
                    name = value.strip("\"'")
                else:
                    with_ind = indent_of(bl)
                continue
            if with_ind is not None and indent_of(bl) > with_ind:
                flag = re.match(r"^['\"]?([a-z][a-z-]*)['\"]?\s*:\s*(.*)$", stripped)
                if flag:
                    flags[flag.group(1)] = flag.group(2).strip().strip("\"'")
        if with_ind is not None and not name and "name" in flags:
            name = flags.pop("name")
        paths = [value.strip() for value in INLINE_PATH_RE.findall(line)]
        for k in range(i - start + 1, len(block)):
            bl = block[k]
            stripped = bl.strip()
            if not stripped.startswith("path:"):
                continue
            value = stripped[len("path:"):].strip()
            if value in ("|", "|-", "|+", ">", ">-", ">+"):
                pi = indent_of(bl)
                for entry in block[k + 1:]:
                    if not entry.strip():
                        continue
                    if indent_of(entry) <= pi:
                        break
                    paths.append(entry.strip())
            elif value:
                paths.append(value.strip("\"'"))
        for ref in refs:
            records.append(dict(
                action=ref, condition=condition, name=name, flags=dict(flags),
                paths=list(paths), gated=UPLOAD_EXPORT_INPUT in condition))
    return records


def boolean_input_declared(text, name):
    """Require workflow_call.inputs to declare a boolean switch defaulting to false."""
    lines = text.splitlines()
    for idx, line in enumerate(lines):
        if not re.match(r"^\s*workflow_call:\s*$", line):
            continue
        base = indent_of(line)
        inputs_idx = None
        for j in range(idx + 1, len(lines)):
            nxt = lines[j]
            if nxt.strip() and indent_of(nxt) <= base:
                break
            if re.match(r"^\s*inputs:\s*$", nxt) and indent_of(nxt) > base:
                inputs_idx = j
                break
        if inputs_idx is None:
            continue
        inputs_ind = indent_of(lines[inputs_idx])
        for j in range(inputs_idx + 1, len(lines)):
            entry = lines[j]
            if entry.strip() and indent_of(entry) <= inputs_ind:
                break
            if not re.match(rf"^\s*{re.escape(name)}:\s*$", entry):
                continue
            decl_ind = indent_of(entry)
            fields = {}
            for sub in lines[j + 1:]:
                if sub.strip() and indent_of(sub) <= decl_ind:
                    break
                field = re.match(r"^\s*([a-z-]+):\s*(.*)$", sub)
                if field:
                    fields[field.group(1)] = field.group(2).strip()
            if fields.get("type") == "boolean" and fields.get("default") == "false":
                return True
    return False


def validate(text):
    """Return a dict of checks; raise ValueError on the first fail-closed violation."""
    checks = {}
    trigger_block = text.split("on:\n", 1)[1].split("\npermissions:", 1)[0]
    triggers = re.findall(r"^  ([a-z_]+):", trigger_block, re.MULTILINE)
    checks["reusable_only"] = triggers == ["workflow_call"]
    require(checks["reusable_only"], "workflow must be workflow_call only")
    checks["export_input_declared"] = boolean_input_declared(trigger_block, "export_release_assets")
    require(checks["export_input_declared"],
            "workflow_call must declare export_release_assets as boolean defaulting to false")
    checks["candidate_input_declared"] = boolean_input_declared(
        trigger_block, "validate_pdfium_candidate")
    require(checks["candidate_input_declared"],
            "workflow_call must declare validate_pdfium_candidate as boolean defaulting to false")

    # Flow-style step mappings are not used by this workflow. Rejecting them
    # keeps every step in the block form the structural checks below can see.
    checks["block_step_mappings_only"] = not FLOW_STEP_RE.search(text)
    require(checks["block_step_mappings_only"],
            "flow-style step mappings are not permitted by this workflow contract")

    for gate in OFFLINE_GATES:
        require(gate in text, f"offline gate missing from workflow: {gate}")
    checks["offline_gates"] = list(OFFLINE_GATES)

    for marker in REQUIRED_MARKERS:
        require(marker in text, f"required fail-closed marker missing: {marker}")
    checks["required_markers"] = list(REQUIRED_MARKERS)

    # Scan executable (non-comment) lines only; explanatory comments may name
    # the things the workflow refuses to do.
    code_lines = [line for line in text.splitlines() if not line.lstrip().startswith("#")]
    for line in code_lines:
        require(not EXPLICIT_KEY_RE.match(line),
                f"explicit YAML key/value syntax is not permitted: {line.strip()}")
        require(not ANCHOR_KEY_RE.match(line),
                f"YAML anchor/alias in key position is not permitted: {line.strip()}")
        require(not TAG_KEY_RE.match(line),
                f"YAML tag in key position is not permitted: {line.strip()}")
    checks["no_special_keys"] = True
    for line in code_lines:
        require(not home_forbidden(line),
                f"reference to read-only PowerShell $HOME is not permitted: {line.strip()}")
    checks["no_home_reference"] = True
    code = "\n".join(code_lines)
    require("--x-install-root=(" not in code,
            "vcpkg install root must be an explicit path variable, not an inline expression")
    require(".Where({ [string]::IsNullOrWhiteSpace($_) })) { throw" not in code,
            "empty-path guard must test the match count, not coerce a single-element collection")
    require(VCPKG_EMPTY_PATH_GUARD in code,
            "empty-path guard must be executable in the vcpkg install step")
    install_call = '& (Join-Path $env:IRIS_VALIDATION_VCPKG_ROOT "vcpkg.exe") install'
    require(install_call in code, "vcpkg install invocation missing from executable lines")
    require(code.index(VCPKG_EMPTY_PATH_GUARD) < code.index(install_call),
            "empty-path guard must run before vcpkg install")
    checks["vcpkg_empty_path_guard"] = True
    for marker in ATTESTATION_MARKERS:
        require(marker in code,
                f"attestation verification missing from executable workflow lines: {marker}")
    checks["attestation_verification"] = True
    for marker in PE_IMPORT_MARKERS:
        require(marker in code,
                f"staged PE import guard missing from executable workflow lines: {marker}")
    require('roots = @("Iris.exe", "preview_corpus_smoke.exe", "rustc.exe")' not in code,
            "PE inventory roots must be derived from the scanned staged list, not hardcoded")
    # System DLL classification must be fail-closed and robust: forwarders by
    # name, other system DLLs by physical presence in a pinned Windows system
    # dir and a checked-in allowlist, never an inline hardcoded array or the
    # build tree. The forwarder check must read $import only after it is bound.
    require('$system = @(' not in code and '-like $_' not in code,
            "system DLL classification must not use a hardcoded allowlist")
    import_binding = '$import = $m.Groups[1].Value.ToLowerInvariant()'
    forwarder = '$isSystem = $import.StartsWith("api-ms-win-") -or $import.StartsWith("ext-ms-win-")'
    require(import_binding in code and forwarder in code,
            "forwarder classification must read the bound import")
    require(code.index(import_binding) < code.index(forwarder),
            "forwarder classification must run after $import is bound inside the loop")
    # The directory allowlist itself must be pinned. If a build/vcpkg/PDFium
    # directory could be added to $systemDirs, a private DLL in that tree would
    # be classified "system" without staging or recursive inventory, voiding
    # the fail-closed closure. Extract the $systemDirs block and require exactly
    # the three Windows-provided roots, nothing else.
    block_start = code.index('$systemDirs = @(')
    block_body = code[block_start:].split('\n', 1)[1]
    block_body = block_body.split('\n          )', 1)[0]
    entries = sorted(line.strip().rstrip(',').strip()
                     for line in block_body.splitlines() if line.strip())
    allowed = sorted((
        '(Join-Path $env:SystemRoot "System32")',
        '(Join-Path $env:SystemRoot "SysWOW64")',
        '$env:SystemRoot',
    ))
    require(entries == allowed,
            f"system dirs must be exactly the Windows system roots, got: {entries}")
    # Variable names are case-insensitive and the array can be mutated by
    # index, method or cmdlet, so pin every executable line naming the token.
    system_dirs_lines = sorted(line.strip() for line in code_lines
                               if "systemdirs" in line.lower())
    allowed_lines = sorted((
        '$systemDirs = @(',
        'foreach ($sysDir in $systemDirs) {',
    ))
    require(system_dirs_lines == allowed_lines,
            f"system dirs token must only appear in the pinned declaration and probe: "
            f"{system_dirs_lines}")
    for marker in VC_IMPORT_MARKERS:
        require(marker in code,
                f"VC import classification marker missing from executable workflow lines: {marker}")
    # The allowlists must be bound, the classifier defined and the negative
    # self-test completed before the import loop; the VC summary is evidence
    # written after it. Moving the self-test past the loop keeps every marker
    # but defeats the pre-flight negative check, so fail closed on order too.
    classification_order = (
        '$vcAllowPath = Join-Path $env:GITHUB_WORKSPACE "' + VC_IMPORT_ALLOWLIST + '"',
        "function Resolve-ImportResolution(",
        "$vcSelfTest = @(",
        "while ($queue.Count -gt 0) {",
        "vc_imports = @($vcImports | Sort-Object -Unique)",
    )
    positions = [code.index(marker) for marker in classification_order]
    require(positions == sorted(positions),
            "VC allowlists, classifier, negative self-test, import loop and evidence must be ordered")
    import_step = "- name: Inventory staged PE import closure"
    import_block = text.split(import_step, 1)[1].split("\n      - name:", 1)[0]
    for marker in ("function Resolve-ImportResolution(", "$vcSelfTest = @(",
                   'vc_imports = @($vcImports | Sort-Object -Unique)'):
        require(marker in import_block,
                f"VC import classification marker escaped the PE import step: {marker}")
    checks["vc_import_classification"] = True
    checks["vc_negative_self_test"] = True

    checks["system_dirs_token_scoped"] = True
    checks["system_dirs_pinned"] = True
    checks["system_dll_resolution"] = True
    checks["pe_import_closure"] = True

    # Git bash prepends its own link.exe ahead of MSVC's; cargo builds must run
    # from pwsh so the developer PATH created by msvc-dev-cmd is authoritative.
    build_marker = "- name: Build product with stock locked bindings"
    require(build_marker in text, "release build step missing")
    build_block = text.split(build_marker, 1)[1].split("\n      - name:", 1)[0]
    require(re.search(r"^\s+shell: pwsh\s*$", build_block, re.MULTILINE) is not None,
            "release builds must use pwsh so MSVC link.exe wins over Git bash link.exe")
    checks["release_build_shell"] = True

    # No build, vcpkg or PDFium directory may be added to PATH; the smoke must
    # run with exactly one PATH assignment naming the stage and system dirs.
    require(">> $env:GITHUB_PATH" not in code,
            "no build, vcpkg or PDFium directory may be added to the job PATH")
    path_assignments = [line.strip() for line in code_lines
                        if re.search(r"\$env:PATH\s*\+?=", line, re.IGNORECASE)]
    require(path_assignments == [PATH_MARKER],
            f"smoke PATH must contain only the stage and system directories: {path_assignments}")
    checks["staged_only_path"] = True
    stage_export = '"IRIS_VALIDATION_STAGE=$stage" >> $env:GITHUB_ENV'
    require(code.index(stage_export)
            < code.index(PATH_MARKER) < code.index("& $smokeExe $corpus"),
            "the smoke must bind the stage before narrowing PATH and running it")
    checks["stage_binding"] = True

    for marker in PREVIEW_RECORD_MARKERS:
        require(marker in code,
                f"structured preview evidence marker missing from executable workflow lines: {marker}")
    for marker in CANDIDATE_MARKERS:
        require(marker in code,
                f"PDFium candidate gate marker missing from executable workflow lines: {marker}")
    require(code.count("gh attestation verify") == 1,
            "the production attestation step must own the only gh attestation verify command")
    require(code.index("- name: Staged preview smoke (video, PDF, RAW)")
            < code.index(CANDIDATE_MARKERS[0])
            < code.index("- name: Assemble clean product ZIP payload"),
            "the opt-in candidate gate must follow the staged preview smoke")
    checks["candidate_gate_after_smoke"] = True
    require(CANDIDATE_INPUT in code,
            "the opt-in candidate gate must be guarded by validate_pdfium_candidate")
    checks["candidate_gate_ci_only"] = True
    smoke_step = text.split("- name: Staged preview smoke (video, PDF, RAW)", 1)[1]
    smoke_step = smoke_step.split("\n      - name:", 1)[0]
    require(smoke_step.index("$previewRecords = @()")
            < smoke_step.index("if ($smokeOutput.Length -gt 2000)")
            < smoke_step.index("output_tail = $smokeOutput"),
            "preview records must be parsed from full stdout before tail truncation")
    checks["preview_records_retained"] = True

    require(code.index("PDFium sha256 mismatch")
            < code.index("gh attestation verify $asset --repo $pin.repo --bundle $bundle")
            < code.index("tar -xzf $asset"),
            "attestation verification must follow the asset hash check and precede extraction")
    for marker in FORBIDDEN_MARKERS:
        require(marker not in code, f"forbidden marker present in workflow: {marker}")
    checks["forbidden_absent"] = list(FORBIDDEN_MARKERS)

    # No publishing action, gh release subcommand, or write permission may appear.
    for action in ("softprops/action-gh-release", "upload-release-asset",
                   "actions/upload-release-asset@"):
        require(action not in code, f"publishing action present: {action}")
    require(not re.search(r"\bgh\s+release\b", code),
            "gh release invocation present (all subcommands are forbidden)")
    checks["no_publishing"] = True

    # The action id itself is syntax-invariant. Counting it on executable
    # lines catches aliases, anchors and any future YAML spelling that the
    # structural scans above might not recognise.
    literal_uploads = sum(decode_yaml_escapes(line).count(UPLOAD_ACTION_PREFIX)
                          for line in code_lines)
    checks["upload_literal_count"] = literal_uploads
    require(literal_uploads == 3,
            "workflow must name exactly three upload-artifact actions")

    # Every action step must be known. This catches new or renamed actions,
    # including upload actions written with unusual but valid YAML spelling.
    seen_actions = []
    for line in code_lines:
        for action in action_refs(line):
            require(action in ALLOWED_ACTIONS, f"unrecognized action step: {action}")
            seen_actions.append(action)
        # Independently of reference parsing, any line naming a `uses` key must
        # contain an allowlisted action id. A future spelling that defeats the
        # scanner therefore still fails closed.
        if re.search(r"['\"]?uses['\"]?\s*:", line):
            require(any(ref in line for ref in ALLOWED_ACTIONS),
                    f"uses line without an allowlisted action: {line.strip()}")
    checks["actions"] = seen_actions

    ok, reason = permissions_read_only(text)
    require(ok, reason)
    checks["read_only_permissions"] = True

    # Exactly three uploads: one ungated bounded JSON report, one gated
    # release-asset export and one gated reviewed native-evidence export.
    # Classify by each step's own `if:` condition so no extra native upload
    # can slip through and the default path stays JSON-evidence-only.
    steps, _ = upload_paths(text)
    records = upload_step_records(text)
    checks["upload_count"] = steps
    require(steps == 3, "workflow must have exactly three upload-artifact steps")
    require(len(records) == steps,
            "every upload-artifact step must be classified by the upload guard")
    evidence_uploads = [r for r in records if not r["gated"]]
    export_uploads = [r for r in records if r["gated"]]
    checks["evidence_upload_count"] = len(evidence_uploads)
    checks["export_upload_count"] = len(export_uploads)
    require(checks["evidence_upload_count"] == 1,
            "workflow must have exactly one ungated evidence upload")
    require(checks["export_upload_count"] == 2,
            "workflow must have exactly two gated release exports")
    evidence_paths = [p for r in evidence_uploads for p in r["paths"]]
    checks["evidence_json_only"] = bool(evidence_paths) and all(
        p.endswith(".json") for p in evidence_paths)
    require(checks["evidence_json_only"],
            "every evidence upload path must be JSON evidence only")
    for evidence in ("pe-import-closure.json", "preview-smoke.json"):
        require(any(p.endswith(evidence) for p in evidence_paths),
                f"bounded evidence upload missing: {evidence}")
    require(any(p.endswith("product-package-rehearsal.json") for p in evidence_paths),
            "bounded evidence upload missing: product-package-rehearsal.json")
    checks["product_evidence_upload"] = True

    asset_exports = [r for r in export_uploads if r["name"] == EXPORT_ARTIFACT_NAME]
    native_exports = [r for r in export_uploads
                      if r["name"] == NATIVE_EVIDENCE_ARTIFACT_NAME]
    require(len(asset_exports) == 1 and len(native_exports) == 1,
            "gated exports must be windows-release-assets and windows-native-evidence")
    export = asset_exports[0]
    require(export["name"] == EXPORT_ARTIFACT_NAME,
            "gated export must upload the windows-release-assets artifact")
    require(export["flags"].get("if-no-files-found") == "error",
            "gated export must fail when a verified asset is missing")
    retention = export["flags"].get("retention-days", "")
    require(retention.isdigit() and 0 < int(retention) <= 7,
            "gated export retention must be bounded to at most seven days")
    require("compression-level" in export["flags"],
            "gated export must declare a compression level")
    normalized = sorted(p.replace("\\", "/") for p in export["paths"])
    basenames = sorted(p.rsplit("/", 1)[-1] for p in normalized)
    require(basenames == sorted(EXPORT_ASSET_BASENAMES),
            f"gated export must contain exactly the verified release assets, got: {basenames}")
    checks["export_assets_bounded"] = True
    # upload-artifact roots each artifact at the least common ancestor of
    # its search paths. One shared parent is what keeps the download flat
    # for the release consumer; basename checks alone cannot detect it.
    parents = {p.rsplit("/", 1)[0] for p in normalized}
    require(len(parents) == 1,
            f"gated export search paths must share one parent so artifact entries stay flat: {parents}")
    parent = parents.pop()
    require(parent == EXPORT_ARTIFACT_ROOT,
            f"gated export must read only the verified release stage, got: {parent}")
    require(normalized == [f"{parent}/{name}" for name in sorted(EXPORT_ASSET_BASENAMES)],
            "gated export artifact-relative paths must match the release consumer")
    checks["export_flat_root"] = True

    native = native_exports[0]
    require(native["flags"].get("if-no-files-found") == "error",
            "gated native evidence export must fail when a reviewed input is missing")
    retention = native["flags"].get("retention-days", "")
    require(retention.isdigit() and 0 < int(retention) <= 7,
            "gated native evidence retention must be bounded to at most seven days")
    require("compression-level" in native["flags"],
            "gated native evidence export must declare a compression level")
    native_paths = sorted(p.replace("\\", "/") for p in native["paths"])
    native_basenames = sorted(p.rsplit("/", 1)[-1] for p in native_paths)
    checks["native_evidence_json_only"] = bool(native_paths) and all(
        p.endswith(".json") for p in native_paths)
    require(checks["native_evidence_json_only"],
            "gated native evidence export must be JSON evidence only")
    require(native_basenames == sorted(NATIVE_EVIDENCE_BASENAMES),
            f"gated native evidence must contain exactly the reviewed inputs, got: {native_basenames}")
    native_parents = {p.rsplit("/", 1)[0] for p in native_paths}
    require(native_parents == {NATIVE_EVIDENCE_ROOT},
            "gated native evidence must read only the bounded evidence directory")
    require(native_paths == [f"{NATIVE_EVIDENCE_ROOT}/{name}"
                             for name in sorted(NATIVE_EVIDENCE_BASENAMES)],
            "gated native evidence artifact-relative paths must match the consumer")
    checks["native_evidence_bounded"] = True

    # The product rehearsal is an offline assembly over the already-validated
    # stage. Every new behaviour must be executable, ordered after the smoke,
    # and joined to the single JSON evidence upload.
    for group in (PRODUCT_INVENTORY_MARKERS, PRODUCT_PACKAGE_MARKERS,
                  PRODUCT_NOTICE_MARKERS, PRODUCT_ZIP_MARKERS,
                  RELEASE_PACKAGE_MARKERS,
                  PRODUCT_EVIDENCE_MARKERS, PRODUCT_CLEANUP_MARKERS,
                  SOURCE_BUNDLE_MARKERS):
        for marker in group:
            require(marker in code,
                    f"product rehearsal marker missing from executable workflow lines: {marker}")
    checks["product_rehearsal"] = True
    inventory_before_order = (
        '$productNames = @("Iris.exe") + @($dlls | ForEach-Object { $_.Name }) + @("pdfium.dll")',
        "pwsh scripts/windows-product-package.ps1",
        '$archive = [IO.Compression.ZipFile]::OpenRead($env:IRIS_PRODUCT_ZIP)',
        'report(ev / "product-package-rehearsal.json", json.loads)',
    )
    positions = [code.index(marker) for marker in inventory_before_order]
    require(positions == sorted(positions),
            "product rehearsal must inventory, package, verify and report in order")
    require(code.index('"IRIS_VALIDATION_STAGE=$stage" >> $env:GITHUB_ENV')
            < code.index(PATH_MARKER) < code.index("pwsh scripts/windows-product-package.ps1"),
            "product rehearsal must bind the stage before the smoke and packaging")
    require(code.count("pwsh scripts/windows-product-package.ps1") == 1,
            "product package script must be invoked exactly once")
    require(code.count("pwsh scripts/package-windows.ps1") == 1,
            "release package script must be invoked exactly once")
    require(code.index("- name: Verify product ZIP entry inventory")
            < code.index("- name: Exercise release packaging from exact product inventory")
            < code.index("- name: Assemble bounded evidence manifest"),
            "release package rehearsal must follow ZIP verification and precede reporting")
    script_pos = code.index("pwsh scripts/windows-product-package.ps1")
    # $clean/$inventoryPath/$zipPath are step-local PowerShell variables defined
    # only in the Assemble step; requiring each binding to follow the $clean
    # definition (and precede the call) pins it to that step. A binding moved
    # into an earlier step would expand to empty and reintroduce the bug.
    clean_def = '$clean = Join-Path $env:RUNNER_TEMP "iris-product-clean"'
    clean_pos = code.index(clean_def)
    for binding in ("$env:IRIS_PRODUCT_STAGE = $clean",
                    "$env:IRIS_PRODUCT_INVENTORY = $inventoryPath",
                    "$env:IRIS_PRODUCT_ZIP = $zipPath"):
        binding_pos = code.index(binding)
        require(clean_pos < binding_pos < script_pos,
                "product package env must be bound in the assembling step before the script runs")
    # The notice is copied once into the inventoried runtime stage before the
    # inventory is written and once into the clean ZIP stage by the assembling
    # step itself, after the inventory copy loop and before the ZIP path.
    notice_bind, notice_exists, notice_nonempty = PRODUCT_NOTICE_MARKERS[:3]
    stage_notice_copy, notice_inventory, clean_notice_copy = PRODUCT_NOTICE_MARKERS[3:]
    require(code.count(notice_bind) == 2,
            "FFmpeg notice source must be bound in both staging and assembling steps")
    inventory_create = '$inventory = @{ roots = @($stage); files = @($productFiles) }'
    stage_start = code.index('$stage = Join-Path $env:RUNNER_TEMP "iris-runtime-stage"')
    stage_block = code[stage_start:code.index(inventory_create)]
    for marker in PRODUCT_NOTICE_MARKERS[:5]:
        require(marker in stage_block,
                f"FFmpeg notice staging marker missing from the runtime stage: {marker}")
    require(stage_block.index(notice_bind) < stage_block.index(notice_exists)
            < stage_block.index(notice_nonempty) < stage_block.index(stage_notice_copy)
            < stage_block.index(notice_inventory),
            "runtime stage must verify and stage the FFmpeg notice before inventorying it")
    assemble_start = code.index("- name: Assemble clean product ZIP payload")
    assemble_block = code[assemble_start:script_pos]
    zip_def = '$zipPath = Join-Path $env:RUNNER_TEMP "iris-product-rehearsal.zip"'
    inventory_copy = 'Copy-Item -LiteralPath $source -Destination (Join-Path $clean $name) -Force'
    for marker in (notice_bind, notice_exists, notice_nonempty, clean_notice_copy):
        require(marker in assemble_block,
                f"FFmpeg notice staging marker missing from the assembling step: {marker}")
    require(assemble_block.index(inventory_copy) < assemble_block.index(notice_bind)
            < assemble_block.index(notice_exists) < assemble_block.index(notice_nonempty)
            < assemble_block.index(clean_notice_copy) < assemble_block.index(zip_def),
            "assembling step must verify and stage the FFmpeg notice after the inventory loop and before the ZIP path")
    checks["ffmpeg_notice_staged"] = True

    # The bundle helper must run inside the runtime stage, after the notice is
    # bound and before the inventory is written. Its names are appended to the
    # same product list that feeds the ZIP, installer and native evidence.
    bundle_def = "$sourceBundle = @("
    bundle_call = "python3 scripts/windows-release-source-bundle.py build"
    bundle_check = 'throw "source/notice bundle missing from stage: $name"'
    bundle_inventory = "$productNames += $sourceBundle"
    zip_guard = 'throw "source/notice bundle missing from product inventory: $name"'
    positions = [code.index(marker) for marker in (
        '$stage = Join-Path $env:RUNNER_TEMP "iris-runtime-stage"',
        notice_bind, bundle_def, bundle_call, bundle_check,
        bundle_inventory, inventory_create)]
    require(positions == sorted(positions),
            "source/notice bundle must be built and verified before the product inventory")
    zip_start = code.index("- name: Verify product ZIP entry inventory")
    require(zip_start < code.index(zip_guard) < code.index(
            'report(ev / "product-package-rehearsal.json", json.loads)'),
            "product ZIP must verify the source/notice bundle inventory before reporting")
    bundle_lines = code.split(bundle_def, 1)[1].splitlines()[1:6]
    bundle_names = tuple(line.strip().rstrip(",").strip('"') for line in bundle_lines)
    require(SOURCE_BUNDLE_OUTPUTS == bundle_names,
            "source/notice bundle output names drifted from the contract")
    checks["source_bundle_staged"] = True

    require("Expand-Archive" not in code and "ZipFile.ExtractToDirectory" not in code,
            "the product ZIP must be verified in place, never extracted")
    require(code.count("& $smokeExe $corpus") == 1,
            "only the staged preview smoke may run; never smoke an assembled ZIP")
    checks["no_extracted_zip_smoke"] = True

    # Native bytes must be cleaned before upload.
    require(CLEANUP_MARKER in text and "rm -rf" in text and "if: always()" in text,
            "native bytes cleanup is not fail-closed")
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs="?", choices=["check", "contract"], default="check")
    args = parser.parse_args()
    if args.command == "contract":
        print(json.dumps(dict(
            triggers=["workflow_call"], offline_gates=list(OFFLINE_GATES),
            required_markers=list(REQUIRED_MARKERS), forbidden_markers=list(FORBIDDEN_MARKERS),
            allowed_actions=sorted(ALLOWED_ACTIONS), flow_steps_allowed=False,
            upload_count=3, evidence_upload_count=1, export_upload_count=2,
            export_input=UPLOAD_EXPORT_INPUT, candidate_input=CANDIDATE_INPUT,
            export_artifact_name=EXPORT_ARTIFACT_NAME,
            export_asset_basenames=list(EXPORT_ASSET_BASENAMES),
            export_artifact_root=EXPORT_ARTIFACT_ROOT,
            native_evidence_artifact_name=NATIVE_EVIDENCE_ARTIFACT_NAME,
            native_evidence_basenames=list(NATIVE_EVIDENCE_BASENAMES),
            native_evidence_root=NATIVE_EVIDENCE_ROOT,
            native_evidence_prepare=NATIVE_EVIDENCE_PREPARE_MARKER,
            source_bundle_outputs=list(SOURCE_BUNDLE_OUTPUTS),
        ), indent=2))
        return
    checks = validate(WORKFLOW.read_text())
    print(json.dumps(checks, indent=2))


if __name__ == "__main__":
    main()
