#!/usr/bin/env python3
"""Guard CI triggers and reusable report-only validation routes."""

from fnmatch import fnmatchcase
from pathlib import Path
import re

root = Path(__file__).resolve().parents[1]
ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
prototype = (root / ".github/workflows/windows-ffmpeg-prototype.yml").read_text(encoding="utf-8")
linux = (root / ".github/workflows/linux-ffmpeg-validation.yml").read_text(encoding="utf-8")

macos = (root / ".github/workflows/macos-runtime-validation.yml").read_text(encoding="utf-8")
release = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
macos_call = "uses: ./.github/workflows/macos-runtime-validation.yml"
bootstrap_timeout = (
    "Downloading https://github.com/microsoft/vcpkg-tool/releases/download/2026-10-29/vcpkg.exe "
    "-> <work>...While calling Windows API function WinHttpReceiveResponse got error "
    "0x00002EE2: The operation timed out"
)
# Capture the entire upload configuration: extra paths, globs, or a second
# uploader must not quietly turn this evidence-only route into package retention.
macos_uploads = re.findall(
    r"^      (?:- |  )uses: actions/upload-artifact@[^\n]+\n"
    r"        with:\n((?:          [^\n]*\n)+)", macos, re.MULTILINE
)


# Match the deliberately simple trigger shape without a YAML dependency. These
# examples use only literal paths and /** directory suffixes (no GitHub-only globs).
prototype_triggers = prototype.split("on:\n", 1)[1].split("\npermissions:", 1)[0]
prototype_paths = {
    "prototype/windows-ffmpeg/**", ".github/workflows/windows-ffmpeg-prototype.yml",
    ".github/workflows/ci.yml", "Cargo.toml", "Cargo.lock", ".cargo/**",
    "rust-toolchain.toml", "assets/icons/iris-icon.ico", "crates/ui-shell/**",
    "crates/thumb-preview/**",
}
positive_paths = (
    "prototype/windows-ffmpeg/preview-smoke/src/main.rs",
    ".github/workflows/windows-ffmpeg-prototype.yml", ".github/workflows/ci.yml",
    "Cargo.toml", "Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml",
    "assets/icons/iris-icon.ico", "crates/ui-shell/src/main.rs",
    "crates/thumb-preview/src/lib.rs",
)
negative_paths = (
    "README.md", "docs/development.md", ".github/workflows/release.yml",
    "prototype/other/main.rs", "Cargo.toml.bak", "crates-other/main.rs",
    "crates/smb-client/src/lib.rs", "crates/backends/src/pcloud/api_client.rs",
    "scripts/test-workflow-triggers.py", "scripts/package-linux.sh",
    "packaging/windows/app.manifest", "assets/screenshots/guide.png",
)


def prototype_event_contract(event):
    block = re.search(
        rf"^  {event}:\n(.*?)(?=^  [a-z_]+:|\Z)",
        prototype_triggers, re.MULTILINE | re.DOTALL,
    )
    if not block:
        return False
    filters = re.fullmatch(
        r"    branches: \[dev\]\n    paths:\n((?:      - '[^'\n]+'\n)+)\n?",
        block[1],
    )
    if not filters:
        return False
    paths = re.findall(r"      - '([^']+)'", filters[1])
    return (
        set(paths) == prototype_paths
        and all(any(fnmatchcase(path, pattern) for pattern in paths) for path in positive_paths)
        and not any(fnmatchcase(path, pattern) for path in negative_paths for pattern in paths)
    )

main_tag_guard = """      - name: Verify tag commit belongs to main
        if: github.event_name == 'push'
        shell: bash
        run: |
          if ! git merge-base --is-ancestor "$GITHUB_SHA" refs/remotes/origin/main; then
            echo "release tags must point to a commit reachable from main" >&2
            exit 1
          fi
"""

checks = {
    "CI reruns bounded SMB diagnostics for invalid-parameter failures": (
        "          if grep -Fq 'returned unexpected status 0xc000000d' \"$diagnostic_output\"; then\n" in ci
        and "IRIS_PERF_SKIP_SFTP=1" in ci
        and "IRIS_PERF_SMB_LOG_LEVEL=10" in ci
        and "          exit \"$status\"\n" in ci
    ),
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
    "prototype keeps workflow_call and has no duplicate standalone push": (
        "  workflow_call:\n" in prototype_triggers
        and re.findall(r"^  ([a-z_]+):", prototype_triggers, re.MULTILINE)
        == ["workflow_call", "pull_request"]
    ),
    "prototype PRs target only dev with relevant build paths": prototype_event_contract("pull_request"),
    "prototype vcpkg retries are configurable and timeout-specific": all(
        text in prototype for text in (
            "IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRIES: ${{ vars.IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRIES || 1 }}",
            "IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRY_DELAY_SECONDS: ${{ vars.IRIS_PROTOTYPE_VCPKG_BOOTSTRAP_RETRY_DELAY_SECONDS || 2 }}",
            "$diagnostic -match '(?i)Downloading .*vcpkg(?:-arm64)?\\.exe'",
            "$diagnostic -match '(?i)WinHttpReceiveResponse'",
            "$diagnostic -match '(?i)(?:timed out|0x00002EE2)'",
            "if (!$downloadTimeout -or $attempt -ge $retryCount)",
            '& $exe --version',
        )
    ) and all(
        re.search(pattern, bootstrap_timeout) for pattern in (
            r"(?i)Downloading .*vcpkg(?:-arm64)?\.exe",
            r"(?i)WinHttpReceiveResponse",
            r"(?i)(?:timed out|0x00002EE2)",
        )
    ),
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
            audit/macos-product.json
            audit/macos-candidate.json
"""] and macos.count("actions/upload-artifact@") == 1 and "audit/macos-candidate.json" in macos,
    "macOS Homebrew baseline is explicitly report-only": """      - name: Audit report-only Homebrew baseline
        shell: bash
        run: |
          # Historical Homebrew comparison only; never the production selector.
          # Only JSON is retained, not the temporary app/ZIP/PKG.
          trap 'rm -rf dist/macos target/package-macos' EXIT
          IRIS_MACOS_REPORT_ONLY=1 IRIS_MACOS_FFMPEG_BASELINE=1 ./scripts/package-macos-app.sh
          python3 scripts/release-audit.py macos --output audit/macos.json
""" in macos,
    "macOS production audit is independent and requires successful prerequisites": """      - name: Audit pinned production package
        if: ${{ !cancelled() && steps.candidate_contracts.outcome == 'success' }}
        shell: bash
        run: |
          # Exercise the same default-product runtime/staging/sign/archive path
          # without credentials or publication. Retain only bounded JSON evidence.
          trap 'rm -rf dist/macos target/package-macos' EXIT
          ./scripts/package-macos-app.sh
          python3 scripts/macos-ffmpeg.py check dist/macos/Iris.app
          python3 scripts/release-audit.py macos --output audit/macos-product.json
""" in macos,
    "macOS candidate runs after baseline failure only when its prerequisites passed": """      - name: Check candidate contracts
        id: candidate_contracts
""" in macos
    and """      - name: Validate pinned candidate
        # Run after a baseline failure when candidate prerequisites passed;
        # never continue after cancellation or a failed candidate-contract step.
        if: ${{ !cancelled() && steps.candidate_contracts.outcome == 'success' }}
""" in macos
    and """          trap 'rm -rf dist/macos target/macos-runtime-validation' EXIT
          python3 scripts/macos-ffmpeg.py validate audit/macos-candidate.json""" in macos,
    "macOS validation cleans temporary apps before retaining evidence": """      - name: Clean temporary bytes even after interruption
        if: always()
        run: rm -rf dist/macos target/package-macos target/macos-runtime-validation
      - name: Retain JSON evidence only
        if: always()
        uses: actions/upload-artifact@v4
""" in macos,
    "preview corpus network smoke follows local and native integration tests": all(
        ci.index(f"      - name: {step}")
        < ci.index("      - name: Default-feature preview corpus correctness and bounded latency smoke")
        for step in (
            "Samba failure log selection regression",
            "Darwin native Samba startup regression",
            "Samba test-server policy regression",
            "Native SFTP and SMB container integration",
            "Native SMB 3.0 AES-CCM compatibility",
            "Native SMB 2.1 signed compatibility",
            "Fuzz smoke",
        )
    ),
    "Release tags are checked against main before package jobs": (
        "          fetch-depth: 0\n" in release
        and main_tag_guard in release
        and all(
            f"  {job}:\n    needs: release-preflight\n" in release
            for job in ("package-macos", "package-linux", "package-windows")
        )
    ),
    "CI manual, pull-request, and scheduled triggers": all(
        trigger in ci for trigger in ("  workflow_dispatch:\n", "  pull_request:\n", "  schedule:\n")
    ),
}
failed = [name for name, passed in checks.items() if not passed]
if failed:
    raise SystemExit("workflow trigger regression: " + "; ".join(failed))
print("CI trigger and reusable report-only audit dispatch checks passed")
