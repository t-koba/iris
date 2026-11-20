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

    # No publishing action or release promotion may be invoked.
    for action in ("softprops/action-gh-release", "gh release create", "upload-release-asset"):
        require(action not in code, f"publishing action present: {action}")
    checks["upload_count"] = len(re.findall(r"actions/upload-artifact@", text))
    require(checks["upload_count"] <= 2, "at most two bounded artifact uploads")
    # Only examine the `path: |` block inside an upload-artifact step.
    upload_steps = re.findall(
        r"uses: actions/upload-artifact@[\s\S]*?path: \|([\s\S]*?)(?=\n      - name:|\Z)",
        text,
    )
    def _all_paths_json(block):
        # Path entries are the lines before the upload-artifact option keys.
        entries = []
        for ln in block.splitlines():
            s = ln.strip()
            if not s:
                continue
            if s.split(":", 1)[0] in ("retention-days", "compression-level", "if-no-files-found", "name"):
                break
            entries.append(s)
        return bool(entries) and all(ln.endswith(".json") for ln in entries)

    checks["evidence_json_only"] = bool(upload_steps) and all(
        _all_paths_json(block) for block in upload_steps
    )
    require(checks["evidence_json_only"], "uploaded artifacts must be JSON evidence only")

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
