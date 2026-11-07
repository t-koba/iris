#!/usr/bin/env python3
"""Guard CI triggers and reusable report-only validation routes."""

from pathlib import Path
import re

root = Path(__file__).resolve().parents[1]
ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
prototype = (root / ".github/workflows/windows-ffmpeg-prototype.yml").read_text(encoding="utf-8")
linux = (root / ".github/workflows/linux-ffmpeg-validation.yml").read_text(encoding="utf-8")

macos = (root / ".github/workflows/macos-runtime-validation.yml").read_text(encoding="utf-8")
macos_call = "uses: ./.github/workflows/macos-runtime-validation.yml"
# Capture the entire upload configuration: extra paths, globs, or a second
# uploader must not quietly turn this evidence-only route into package retention.
macos_uploads = re.findall(
    r"^      (?:- |  )uses: actions/upload-artifact@[^\n]+\n"
    r"        with:\n((?:          [^\n]*\n)+)", macos, re.MULTILINE
)

checks = {
    "CI mizu exclusion and all-tag filter": """  push:
    branches-ignore:
      - mizu
    tags:
      - '**'
""" in ci,
    "CI invokes the reusable prototype only on manual dispatch": """  windows-ffmpeg-prototype:
    if: github.event_name == 'workflow_dispatch'
    permissions:
      contents: read
    uses: ./.github/workflows/windows-ffmpeg-prototype.yml
""" in ci,
    "prototype is reusable, not an independently registered workflow": """on:
  workflow_call:
""" in prototype
    and all(trigger not in prototype for trigger in ("  workflow_dispatch:", "  pull_request:", "  push:")),
    "CI invokes the Linux runtime audit only on manual dispatch": """  linux-ffmpeg-validation:
    if: github.event_name == 'workflow_dispatch'
    permissions:
      contents: read
    uses: ./.github/workflows/linux-ffmpeg-validation.yml
""" in ci,
    "Linux runtime audit is reusable, not independently registered": """on:
  workflow_call:
""" in linux
    and all(trigger not in linux for trigger in ("  workflow_dispatch:", "  pull_request:", "  push:")),
    "CI invokes the macOS runtime audit only on manual dispatch": """  macos-runtime-validation:
    if: github.event_name == 'workflow_dispatch'
    permissions:
      contents: read
    uses: ./.github/workflows/macos-runtime-validation.yml
""" in ci and ci.count(macos_call) == 1,
    "macOS runtime audit is workflow_call only": """on:
  workflow_call:

permissions:
""" in macos,
    "macOS runtime audit preserves the default product feature variant": "      IRIS_BUILD_VARIANT: product\n" in macos,
    "macOS runtime audit checks out the caller's exact SHA without credentials": """      - uses: actions/checkout@v7
        with:
          ref: ${{ github.sha }}
          persist-credentials: false
""" in macos,
    "macOS runtime audit uploads only its JSON reports": macos_uploads == ["""          name: macos-runtime-audit
          if-no-files-found: error
          path: |
            audit/macos.json
            audit/macos-candidate.json
"""] and macos.count("actions/upload-artifact@") == 1 and "audit/macos-candidate.json" in macos,
    "macOS current audit keeps release report beside candidate": """          trap 'rm -rf dist/macos target/package-macos' EXIT
          ./scripts/package-macos-app.sh
          python3 scripts/release-audit.py macos --output audit/macos.json""" in macos
    and """          trap 'rm -rf dist/macos target/macos-runtime-validation' EXIT
          python3 scripts/macos-ffmpeg.py validate audit/macos-candidate.json""" in macos,
    "macOS validation cleans temporary apps before retaining evidence": """      - name: Clean temporary bytes even after interruption
        if: always()
        run: rm -rf dist/macos target/package-macos target/macos-runtime-validation
      - name: Retain JSON evidence only
        if: always()
        uses: actions/upload-artifact@v4
""" in macos,
    "CI manual, pull-request, and scheduled triggers": all(
        trigger in ci for trigger in ("  workflow_dispatch:\n", "  pull_request:\n", "  schedule:\n")
    ),
}
failed = [name for name, passed in checks.items() if not passed]
if failed:
    raise SystemExit("workflow trigger regression: " + "; ".join(failed))
print("CI trigger and reusable report-only audit dispatch checks passed")
