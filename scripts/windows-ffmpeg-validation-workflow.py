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
    '"status": "not_run"',
) + ATTESTATION_MARKERS

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
    for marker in ATTESTATION_MARKERS:
        require(marker in code,
                f"attestation verification missing from executable workflow lines: {marker}")
    checks["attestation_verification"] = True
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

    # Native bytes must be cleaned before upload.
    require("rm -rf" in text and "if: always()" in text, "native bytes cleanup is not fail-closed")
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
