#!/usr/bin/env python3
"""Authoritative offline contract for the Windows native-preview validation workflow.

The workflow is reusable (workflow_call only), report-only, and never promotes
a release. This module is the single source of truth for its fail-closed shape:
exact-SHA checkout, all offline gates, pinned vcpkg HEAD assertion, verified
source acquisition, derived overlay, stock bindings, PDFium attestation, PE
import inventory, clean-install preview smoke, bounded JSON evidence and
cleanup of native bytes. It performs no build and no network access.
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
    "python3 scripts/windows-ffmpeg-validation-config.py",
    "python3 scripts/test-windows-ffmpeg-validation-config.py",
    "python3 scripts/test-windows-ffmpeg-validation-workflow.py",
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

# dumpbin must be checked for failure, and every non-system import must be
# resolved in the stage and then queued for its own inventory.
PE_IMPORT_MARKERS = (
    "dumpbin /DEPENDENTS $binary",
    '$systemDirs = @(',
    '(Join-Path $env:SystemRoot "System32")',
    'foreach ($sysDir in $systemDirs) {',
    '(Join-Path $env:IRIS_VALIDATION_STAGE "pdfium.dll")',
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

# Both staged-runtime results must be read into the single JSON manifest.
EVIDENCE_MARKERS = (
    'report(ev / "pe-import-closure.json", json.loads)',
    'report(ev / "preview-smoke.json", json.loads)',
    "pe_import_closure=closure",
    "preview_smoke=smoke",
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

# The ZIP is read in place: entry names, SHA-256 digests and byte counts must
# match the inventory exactly. No preview is run from the ZIP.
PRODUCT_ZIP_MARKERS = (
    "- name: Verify product ZIP entry inventory",
    "Add-Type -AssemblyName System.IO.Compression.FileSystem",
    '$archive = [IO.Compression.ZipFile]::OpenRead($env:IRIS_PRODUCT_ZIP)',
    "foreach ($entry in $archive.Entries) {",
    'if (!$expected.ContainsKey($key)) { throw "unexpected zip entry: $name" }',
    'if ($hash -ne $expected[$key].sha256) { throw "zip entry hash mismatch: $name" }',
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
) + ATTESTATION_MARKERS + STAGE_MARKERS + PE_IMPORT_MARKERS + EVIDENCE_MARKERS + (
    CLEANUP_MARKER, PATH_MARKER,
) + PRODUCT_INVENTORY_MARKERS + PRODUCT_PACKAGE_MARKERS + PRODUCT_ZIP_MARKERS + (
    PRODUCT_EVIDENCE_MARKERS + PRODUCT_CLEANUP_MARKERS
)

# Only these actions may appear in the validation workflow. A closed allowlist
# fails closed on any new or renamed action (including a differently spelled
# upload action) instead of trying to enumerate every upload syntax.
ALLOWED_ACTIONS = frozenset((
    "actions/checkout@v4",
    "ilammy/msvc-dev-cmd@v1",
    "dtolnay/rust-toolchain@master",
    "actions/upload-artifact@v4",
))
UPLOAD_ACTION_PREFIX = "actions/upload-artifact@"
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


def validate(text):
    """Return a dict of checks; raise ValueError on the first fail-closed violation."""
    checks = {}
    trigger_block = text.split("on:\n", 1)[1].split("\npermissions:", 1)[0]
    triggers = re.findall(r"^  ([a-z_]+):", trigger_block, re.MULTILINE)
    checks["reusable_only"] = triggers == ["workflow_call"]
    require(checks["reusable_only"], "workflow must be workflow_call only")

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
    # name, other system DLLs by physical presence in a Windows system dir,
    # never a brittle hardcoded allowlist and never the build tree. The
    # forwarder check must read $import only after it is bound in the loop.
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
    require(literal_uploads == 1,
            "workflow must name exactly one upload-artifact action")

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

    # Exactly one bounded evidence upload; every path form must be JSON.
    steps, paths = upload_paths(text)
    checks["upload_count"] = steps
    require(steps == 1, "workflow must have exactly one bounded evidence upload")
    checks["evidence_json_only"] = bool(paths) and all(p.endswith(".json") for p in paths)
    require(checks["evidence_json_only"],
            "every uploaded artifact path must be JSON evidence only")
    for evidence in ("pe-import-closure.json", "preview-smoke.json"):
        require(any(p.endswith(evidence) for p in paths),
                f"bounded evidence upload missing: {evidence}")
    require(any(p.endswith("product-package-rehearsal.json") for p in paths),
            "bounded evidence upload missing: product-package-rehearsal.json")
    checks["product_evidence_upload"] = True

    # The product rehearsal is an offline assembly over the already-validated
    # stage. Every new behaviour must be executable, ordered after the smoke,
    # and joined to the single JSON evidence upload.
    for group in (PRODUCT_INVENTORY_MARKERS, PRODUCT_PACKAGE_MARKERS,
                  PRODUCT_ZIP_MARKERS, PRODUCT_EVIDENCE_MARKERS,
                  PRODUCT_CLEANUP_MARKERS):
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
    script_pos = code.index("pwsh scripts/windows-product-package.ps1")
    for binding in ("$env:IRIS_PRODUCT_STAGE = $clean",
                    "$env:IRIS_PRODUCT_INVENTORY = $inventoryPath",
                    "$env:IRIS_PRODUCT_ZIP = $zipPath"):
        require(code.index(binding) < script_pos,
                "product package env must be bound in the same step before the script runs")
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
        ), indent=2))
        return
    checks = validate(WORKFLOW.read_text())
    print(json.dumps(checks, indent=2))


if __name__ == "__main__":
    main()
