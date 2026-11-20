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
)

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


def upload_paths(text):
    """Return (step_count, paths) for every upload-artifact step, inline or block.

    Every `uses: actions/upload-artifact@` step is scanned; inline `path: X` and
    `path: |` block scalars are both collected so no upload form can evade the
    JSON-evidence guard.
    """
    lines = text.splitlines()
    paths = []
    steps = 0
    for i, line in enumerate(lines):
        if "uses: actions/upload-artifact@" not in line:
            continue
        steps += 1
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

    for gate in OFFLINE_GATES:
        require(gate in text, f"offline gate missing from workflow: {gate}")
    checks["offline_gates"] = list(OFFLINE_GATES)

    for marker in REQUIRED_MARKERS:
        require(marker in text, f"required fail-closed marker missing: {marker}")
    checks["required_markers"] = list(REQUIRED_MARKERS)

    # Scan executable (non-comment) lines only; explanatory comments may name
    # the things the workflow refuses to do.
    code_lines = [line for line in text.splitlines() if not line.lstrip().startswith("#")]
    code = "\n".join(code_lines)
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
        ), indent=2))
        return
    checks = validate(WORKFLOW.read_text())
    print(json.dumps(checks, indent=2))


if __name__ == "__main__":
    main()
