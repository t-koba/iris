#!/usr/bin/env python3
"""Guard CI triggers and reusable report-only validation routes."""

from fnmatch import fnmatchcase
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parents[1]
ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
prototype = (root / ".github/workflows/windows-ffmpeg-prototype.yml").read_text(encoding="utf-8")
linux = (root / ".github/workflows/linux-ffmpeg-validation.yml").read_text(encoding="utf-8")

macos = (root / ".github/workflows/macos-runtime-validation.yml").read_text(encoding="utf-8")
media_probe = (root / ".github/workflows/windows-media-foundation-probe.yml").read_text(encoding="utf-8")
win_validation = (root / ".github/workflows/windows-ffmpeg-validation.yml").read_text(encoding="utf-8")
release = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
release_macos_job = release.split("  package-macos:\n", 1)[1].split("\n  package-linux:\n", 1)[0]
release_publish = release.split("  publish:\n", 1)[1]
fuzz_long = (root / ".github/workflows/fuzz-long.yml").read_text(encoding="utf-8")
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


def smb_diagnostic_status_contract():
    # Exercise the workflow's actual grep pattern, including token boundaries.
    trigger = re.search(
        r"if grep -Eq '([^'\n]+)' \"\$diagnostic_output\"; then", ci,
    )
    if not trigger:
        return False
    cases = {
        "returned unexpected status 0xc000000d": True,
        "SMB Ioctl returned unexpected status 0xc0000022": True,
        'Error: "SMB Ioctl returned unexpected status 0xc0000022"': True,
        "returned unexpected status 0xc000000d (STATUS_INVALID_PARAMETER)": True,
        "returned unexpected status 0xc000000d0": False,
        "returned unexpected status 0xc00000220": False,
        "returned unexpected status 0xc0000022_suffix": False,
        "returned unexpected status 0xc000000dx": False,
        "returned unexpected status 0xc0000001": False,
        "unrelated log status 0xc0000022": False,
    }
    return all(
        subprocess.run(["grep", "-Eq", trigger[1]], input=line, text=True).returncode
        == (0 if expected else 1)
        for line, expected in cases.items()
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

def ci_job_block(name, workflow=ci):
    try:
        jobs_text = workflow.split("jobs:\n", 1)[1]
    except IndexError:
        return ""
    starts = list(re.finditer(rf"(?m)^  {re.escape(name)}:\n", jobs_text))
    if len(starts) != 1:
        return ""
    start = starts[0]
    following = re.search(r"(?m)^  [a-zA-Z0-9_-]+:\n", jobs_text[start.end():])
    end = start.end() + following.start() if following else len(jobs_text)
    return jobs_text[start.start():end]


def ci_job_has(name, *needles, workflow=ci):
    block = ci_job_block(name, workflow)
    return bool(block) and all(needle in block for needle in needles)


def ci_matrix_job_has(name, *needles, workflow=ci):
    return ci_job_has(name, "runs-on: ${{ matrix.os }}", *needles, workflow=workflow)


def matrix_runner_mutation_regression():
    job = "trash-path-preflight"
    block = ci_job_block(job)
    runner = "runs-on: ${{ matrix.os }}"
    if block.count(runner) != 1:
        return False
    mutated_block = block.replace(runner, "runs-on: ubuntu-latest", 1)
    mutated_workflow = ci.replace(block, mutated_block, 1)
    expected = (
        "os: [ubuntu-latest, windows-latest]",
        "cargo test -p iris-platform-integration --lib trash::exact_items::tests:: --locked",
    )
    return ci_matrix_job_has(job, *expected) and not ci_matrix_job_has(
        job, *expected, workflow=mutated_workflow
    )


def ci_step_block(step_name, job_name, workflow=ci):
    """Return the YAML text of one named step in a job, or '' if absent."""
    job = ci_job_block(job_name, workflow)
    if not job:
        return ""
    m = re.search(rf"(?m)^      - name: {re.escape(step_name)}\n", job)
    if not m:
        return ""
    following = re.search(r"(?m)^      - name: ", job[m.end():])
    end = m.end() + following.start() if following else len(job)
    return job[m.start():end]


def workspace_tests_unconditional(workflow=ci):
    """The main test job must run whole-workspace tests without a gate."""
    step = ci_step_block("Tests", "test", workflow)
    if "run: cargo test --workspace --locked" not in step:
        return False
    # A step-level `if:` sits at exactly eight spaces; a gated or disabled
    # Tests step can silently skip workspace tests while this contract stays green.
    if re.search(r"(?m)^        if:", step):
        return False
    # A job-level `if:` is a four-space key anywhere in the job block (YAML
    # mapping keys are unordered); gating the job makes Tests unreachable.
    return not re.search(r"(?m)^    if:", ci_job_block("test", workflow))


def workspace_tests_mutation_regression():
    """Prove the workspace-tests guard rejects a disabled Tests step."""
    step = ci_step_block("Tests", "test")
    if not step:
        return False
    disabled = step.replace(
        "      - name: Tests\n", "      - name: Tests\n        if: false\n", 1
    )
    mutated = ci.replace(step, disabled, 1)
    return workspace_tests_unconditional() and not workspace_tests_unconditional(
        workflow=mutated
    )


def workspace_tests_job_gate_mutation_regression():
    """Prove the guard rejects a job gate even when `if:` follows the steps."""
    block = ci_job_block("test")
    if not block:
        return False
    gated = block.rstrip("\n") + "\n    if: false\n"
    mutated = ci.replace(block, gated, 1)
    return workspace_tests_unconditional() and not workspace_tests_unconditional(
        workflow=mutated
    )


def fuzz_long_uses_asan_compatible_target():
    step = re.search(
        r"^      - name: Run long fuzz target\n(?P<body>(?:        [^\n]*\n)+)",
        fuzz_long, re.MULTILINE,
    )
    return bool(
        step
        and "IRIS_FUZZ_TARGET: x86_64-unknown-linux-gnu" in step["body"]
        and "FUZZ_MAX_TOTAL_TIME=3600 ./scripts/fuzz-target.sh" in step["body"]
    )


def workflow_step(workflow, name):
    """Return one named step of these single-job validation workflows."""
    start_marker = f"      - name: {name}\n"
    start = workflow.find(start_marker)
    if start < 0:
        return ""
    following = workflow.find("      - name: ", start + len(start_marker))
    return workflow[start:] if following < 0 else workflow[start:following]


def run_line(command):
    """Block-scalar and inline `run:` forms of one exact command line."""
    return (f"          {command}\n", f"        run: {command}\n")


def has_run_line(block, command):
    return any(candidate in block for candidate in run_line(command))


def in_order(block, *commands):
    positions = []
    for command in commands:
        found = [block.find(candidate) for candidate in run_line(command) if candidate in block]
        positions.append(min(found) if found else -1)
    return all(position >= 0 for position in positions) and positions == sorted(positions)


def audit_commands(workflow):
    return [line.strip().removeprefix("run: ") for line in workflow.splitlines()
            if "scripts/release-audit.py" in line]


installer_verify = ("python3 scripts/macos-installer-trust.py verify --pkg dist/macos/Iris-macos.pkg "
                    "--output audit/macos-installer-trust.json")
installer_check = ("python3 scripts/macos-installer-trust.py check --pkg dist/release/Iris-macos.pkg "
                   "--evidence audit/macos/macos-installer-trust.json")
installer_secrets = (
    "          IRIS_MACOS_INSTALLER_IDENTITY: ${{ github.event_name == 'push' && secrets.IRIS_MACOS_INSTALLER_IDENTITY || '' }}\n",
    "          IRIS_MACOS_INSTALLER_NOTARY_PROFILE: ${{ github.event_name == 'push' && secrets.IRIS_MACOS_INSTALLER_NOTARY_PROFILE || '' }}\n",
)


macos_generate = (
    "python3 scripts/posix-native-evidence.py generate --platform macos --stage dist/macos "
    "--mapping packaging/native-evidence/macos-mapping.json "
    "--output audit/native-evidence/macos-product.json "
    "--report audit/native-evidence/macos-product-review.json"
)
macos_product_audit = (
    "python3 scripts/release-audit.py macos --output audit/macos-product.json "
    "--native-evidence audit/native-evidence/macos-product.json --require-native-evidence"
)
macos_baseline_audit = "python3 scripts/release-audit.py macos --output audit/macos.json"
linux_generate = (
    "python3 scripts/posix-native-evidence.py generate --platform linux --stage dist/linux "
    "--mapping packaging/native-evidence/linux-mapping.json "
    "--output audit/native-evidence/linux.json "
    "--report audit/native-evidence/linux-review.json"
)
linux_audit = (
    "python3 scripts/release-audit.py linux --output audit/linux-native-closure.json "
    "--native-evidence audit/native-evidence/linux.json --require-native-evidence"
)
build_step = "Build verified FFmpeg and package default product locally"
evidence_step = "Generate reviewed native evidence from pristine Linux package stage"
audit_step = "Audit full staged Linux native closure (report only)"
smoke_step = "Generate built-in codec clip and smoke AppDir"
macos_product_step = workflow_step(macos, "Audit pinned production package")
macos_baseline_step = workflow_step(macos, "Audit report-only Homebrew baseline")
ci_triggers = ci.split("on:\n", 1)[1].split("\nconcurrency:", 1)[0]
pdfium_candidate_step = ci_step_block(
    "Verify date-later PDFium candidate under the production gate", "test"
)
pdfium_candidate_gate = (
    "        if: matrix.os == 'ubuntu-latest' && github.event_name == 'workflow_dispatch' "
    "&& inputs.validate_pdfium_candidate\n"
)
linux_upload = """        with:
          name: linux-runtime-audit
          if-no-files-found: error
          path: |
            audit/*.json
            audit/native-evidence/linux.json
            audit/native-evidence/linux-review.json
"""
audit_bounds = (
    "      IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES: ${{ vars.IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES || 2097152 }}",
    "      IRIS_RELEASE_AUDIT_MAX_COMMAND_BYTES: ${{ vars.IRIS_RELEASE_AUDIT_MAX_COMMAND_BYTES || 32768 }}",
    "      IRIS_RELEASE_AUDIT_MAX_METADATA_BYTES: ${{ vars.IRIS_RELEASE_AUDIT_MAX_METADATA_BYTES || 8388608 }}",
    "      IRIS_RELEASE_AUDIT_MAX_CARGO_PACKAGES: ${{ vars.IRIS_RELEASE_AUDIT_MAX_CARGO_PACKAGES || 1024 }}",
    "      IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES: ${{ vars.IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES || 256 }}",
    "      IRIS_RELEASE_AUDIT_COMMAND_SECONDS: ${{ vars.IRIS_RELEASE_AUDIT_COMMAND_SECONDS || 30 }}",
)


def release_actions_are_pinned():
    """External release actions must be commit SHAs annotated with their ref."""
    uses_lines = [line for line in release.splitlines() if "uses:" in line]
    if not uses_lines:
        return False
    external = 0
    for line in uses_lines:
        m = re.fullmatch(
            r"\s+(?:- )?uses: (?P<ref>[^\s#]+)(?: # (?P<comment>[^\s#]+))?\s*", line
        )
        if not m:
            return False
        ref, comment = m.group("ref"), m.group("comment")
        if ref.startswith("./"):
            # Local reusable workflow has no external ref to pin.
            continue
        external += 1
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}", ref):
            return False
        if not re.fullmatch(r"(?:v[0-9]+(?:\.[0-9]+)*|master|main)", comment or ""):
            return False
    return external > 0


def linux_pristine_stage_order():
    names = re.findall(r"(?m)^      - name: ([^\n]+)$", linux)
    if not all(name in names for name in (build_step, evidence_step, audit_step, smoke_step)):
        return False
    build, evidence, audit, smoke = (
        names.index(name) for name in (build_step, evidence_step, audit_step, smoke_step)
    )
    return (
        evidence == build + 1
        and audit == evidence + 1
        and smoke > audit
        and linux.index('cp "$CARGO_TARGET_DIR/release/examples/') > linux.index(
            f"      - name: {audit_step}\n"
        )
    )


checks = {
    "Final macOS installer trust runs before publication and is offline-tested": (
        release.count(installer_verify) == 1 and release.count(installer_check) == 1
        and all(line in release for line in installer_secrets)
        and all(line in workflow_step(release_macos_job, "Package macOS app")
                for line in installer_secrets)
        and all(line in release_macos_job for line in installer_secrets)
        and all(line not in release_publish for line in installer_secrets)
        and "        run: python3 scripts/test-macos-installer-trust.py\n" in release
        and "        if: ${{ github.event_name == 'push' }}\n"
        in workflow_step(release_macos_job, "Verify final macOS installer trust")
        and release_macos_job.index("      - name: Verify macOS artifact\n")
        < release_macos_job.index("      - name: Verify final macOS installer trust\n")
        < release_macos_job.index("      - name: Retain macos audit evidence\n")
        and "            audit/macos-installer-trust.json\n"
        in workflow_step(release_macos_job, "Retain macos audit evidence")
        and release_publish.index("      - name: Download staged release assets\n")
        < release_publish.index("      - name: Gate publication on release policy (not legal approval)\n")
        < release_publish.index("      - name: Gate publication on final macOS installer trust\n")
        < release_publish.index("      - name: Verify complete release set\n")
        < release_publish.index("      - name: Publish complete release\n")
    ),
    "Preview corpus CI retries and delay cap keep operator overrides": all(
        f"IRIS_PREVIEW_CORPUS_RETRIES: ${{{{ vars.IRIS_PREVIEW_CORPUS_RETRIES || {retries} }}}}" in workflow
        and f"IRIS_PREVIEW_CORPUS_MAX_RETRY_DELAY_SECONDS: ${{{{ vars.IRIS_PREVIEW_CORPUS_MAX_RETRY_DELAY_SECONDS || {cap} }}}}" in workflow
        for workflow, retries, cap in (
            (ci, "4", "30"), (linux, "4", "30"), (macos, "4", "30"),
            (media_probe, "'4'", "'30'"),
        )
    ),
    "All release audit jobs inherit the operator-configured native component bound": (
        "\n  IRIS_RELEASE_AUDIT_MAX_NATIVE_COMPONENTS: "
        "${{ vars.IRIS_RELEASE_AUDIT_MAX_NATIVE_COMPONENTS || 4096 }}\n" in release
        and release.count("IRIS_RELEASE_AUDIT_MAX_NATIVE_COMPONENTS:") == 1
        and "--max-native-components" not in release
    ),
    "Every production release audit consumes a generated, required platform closure": all(
        f"          python3 scripts/posix-native-evidence.py generate \\\n"
        f"            --platform {platform} \\\n"
        f"            --stage dist/{platform} \\\n"
        f"            --mapping packaging/native-evidence/{platform}-mapping.json \\\n"
        f"            --output audit/native-evidence/{platform}.json \\\n"
        f"            --report audit/native-evidence/{platform}-review.json\n" in release
        and f"        run: python scripts/release-audit.py {platform} --output audit/{platform}.json "
        f"--native-evidence audit/native-evidence/{platform}.json "
        "--require-native-evidence\n" in release
        and f"            audit/native-evidence/{platform}.json\n" in release
        for platform in ("macos", "linux")
    ) and ("        run: python scripts/release-audit.py windows --output audit/windows.json "
           "--native-evidence verified/windows-native-evidence/windows-native-closure.json "
           "--require-native-evidence\n" in release)
    and "      - name: Validate POSIX native-evidence producer offline\n" in release
    and "        run: python scripts/test-posix-native-evidence.py\n" in release
    and "packaging/native-evidence/windows.json" not in release
    and len(re.findall(r"run: python scripts/release-audit\.py ", release)) == 3,
    "Windows release consumes reviewed native-stage evidence end to end": (
        "        if: ${{ inputs.export_release_assets }}\n"
        "        uses: actions/upload-artifact@v4\n"
        "        with:\n"
        "          name: windows-native-evidence\n" in win_validation
        and "python3 scripts/windows-native-evidence.py prepare" in win_validation
        and "          name: windows-native-evidence\n"
        "          path: verified/windows-native-evidence\n" in release
        and "python3 scripts/windows-native-evidence.py seal" in release
        and "--require-native-evidence" in release
    ),
    "CI reruns bounded SMB diagnostics for exact invalid-parameter/access-denied statuses": (
        smb_diagnostic_status_contract()
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
    "Media Foundation probe resolves runner temp from a step, not job-level context": (
        "runner.temp" not in media_probe
        and "    steps:\n" in media_probe
        and "Join-Path $env:RUNNER_TEMP" in media_probe
        and "\"IRIS_MF_TEMP_ROOT=$root\" >> $env:GITHUB_ENV" in media_probe
        and "if: always()" in media_probe
        and "actions/upload-artifact@" not in media_probe
    ),
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
            audit/native-evidence/macos-product.json
            audit/native-evidence/macos-product-review.json
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
          python3 scripts/posix-native-evidence.py generate --platform macos --stage dist/macos --mapping packaging/native-evidence/macos-mapping.json --output audit/native-evidence/macos-product.json --report audit/native-evidence/macos-product-review.json
          python3 scripts/release-audit.py macos --output audit/macos-product.json --native-evidence audit/native-evidence/macos-product.json --require-native-evidence
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
    "Release workflow pins external actions to commit SHAs": release_actions_are_pinned(),
    "Release tags are checked against main before package jobs": (
        "          fetch-depth: 0\n" in release
        and main_tag_guard in release
        and all(
            f"  {job}:\n    needs: release-preflight\n" in release
            for job in ("package-macos", "package-linux")
        )
        and "  windows-product-assets:\n    needs: release-preflight\n" in release
        and "  package-windows:\n    needs: [release-preflight, windows-product-assets]\n" in release
    ),
    "Trash path preflight regressions run on Ubuntu and Windows outside container smoke": ci_matrix_job_has(
        "trash-path-preflight",
        "os: [ubuntu-latest, windows-latest]",
        "cargo test -p iris-platform-integration --lib trash::exact_items::tests:: --locked",
    ),
    "Trash redo receipt regression runs on Ubuntu and Windows outside container smoke": ci_matrix_job_has(
        "trash-redo-receipts",
        "os: [ubuntu-latest, windows-latest]",
        "cargo test -p iris-ui-shell --lib update::undo_ops::tests::trash_redo_passes_fresh_receipts_to_next_undo --locked --no-default-features --features minimal",
    ),
    "Private copy destination regression runs on Unix CI outside native smoke": ci_matrix_job_has(
        "copy-private-destinations",
        "os: [ubuntu-latest, macos-latest]",
        "cargo test -p iris-platform-integration --lib copy::tests::new_copy_destinations_are_private_until_success_under_umask --locked",
    ),
    "Matrix runner contracts reject a forced single Ubuntu runner": matrix_runner_mutation_regression(),
    "CI test job checkout does not persist Git credentials": (
        "\n      - uses: actions/checkout@v7\n"
        "        with:\n          persist-credentials: false\n"
    ) in ci_job_block("test"),
    "Workspace tests run unconditionally in the main CI test job": workspace_tests_unconditional(),
    "Workspace-tests guard rejects a disabled Tests step": workspace_tests_mutation_regression(),
    "Workspace-tests guard rejects a gated test job": workspace_tests_job_gate_mutation_regression(),
    "Workflow contract checks run independently of platform integration": ci_job_has(
        "workflow-contracts",
        "run: python3 scripts/test-workflow-triggers.py",
    ),
    "Product-CI macOS evidence comes from the exact production stage before its audit": (
        in_order(macos_product_step, "./scripts/package-macos-app.sh",
                 "python3 scripts/macos-ffmpeg.py check dist/macos/Iris.app",
                 macos_generate, macos_product_audit)
        and macos.count(macos_generate) == 1
        and macos.count(macos_product_audit) == 1
    ),
    "Product-CI Homebrew baseline stays report-only": (
        has_run_line(macos_baseline_step, macos_baseline_audit)
        and "posix-native-evidence" not in macos_baseline_step
        and "--require-native-evidence" not in macos_baseline_step
    ),
    "Product-CI Linux evidence and audit run before smoke copies": (
        linux_pristine_stage_order()
        and has_run_line(workflow_step(linux, evidence_step), linux_generate)
        and has_run_line(workflow_step(linux, audit_step), linux_audit)
        and linux.count(linux_generate) == 1
        and linux.count(linux_audit) == 1
    ),
    "Product-CI retains both native-evidence sidecars and review reports": (
        linux_upload in linux and linux.count("uses: actions/upload-artifact@") == 1
    ),
    "Product-CI audit lines have no bypass or bound overrides": (
        audit_commands(macos) == [macos_baseline_audit, macos_product_audit]
        and audit_commands(linux) == [linux_audit]
        and not any(flag in macos + linux for flag in (
            "--ignore", "--allow-unmapped", "--skip-native", "--max-report-bytes",
            "--max-native-files", "--max-cargo-packages", "--max-native-components",
        ))
    ),
    "Product-CI adds no package build, Release dispatch or tag": (
        macos.count("./scripts/package-macos-app.sh") == 2
        and linux.count("./scripts/package-linux-appdir.sh") == 1
        and all(token not in workflow for workflow in (macos, linux) for token in (
            "release.yml", "workflow_dispatch:", "softprops/action-gh-release",
            "gh release", "git tag", "refs/tags/", "actions/create-release",
        ))
    ),
    "Product-CI keeps audit bounds, PDFium scope and release hold": (
        all(line in macos and line in linux for line in audit_bounds)
        and 'IRIS_VERIFY_PDFIUM_PROVENANCE: "0"' in linux
        and "pdfium['attestation']['status'] == 'not_attempted'" in linux
        and "Untested by this product-CI gate: verified PDFium attestation, Windows closure, tag-only publish." in linux
    ),
    "SMB adapter fake-server regressions run separately from native container smoke": ci_job_has(
        "smb-adapter-unit",
        "cargo test -p iris-backends --lib smb::adapter::tests:: --locked",
    ),
    "SMB client unit suite isolates SMB signing and encryption KDF vectors from container smoke": ci_job_has(
        "smb3-signing-kdf",
        "cargo test -p iris-smb-client --lib --locked",
    ),
    "Scheduled long fuzz builds for the ASan-compatible GNU target":
        fuzz_long_uses_asan_compatible_target(),
    "CI manual, pull-request, and scheduled triggers": all(
        trigger in ci for trigger in ("  workflow_dispatch:\n", "  pull_request:\n", "  schedule:\n")
    ),
    "CI dispatch input opts in to the unshipped PDFium candidate": ci_triggers == """  push:
    branches-ignore:
      - mizu
    tags:
      - '**'
  pull_request:
  workflow_dispatch:
    inputs:
      validate_pdfium_candidate:
        description: Validate the unshipped PDFium 8179 candidate under the production gate
        type: boolean
        default: false
  schedule:
    - cron: "17 3 * * 1"
""",
    "Unshipped PDFium candidate step runs only on explicit dispatch opt-in": (
        pdfium_candidate_gate in pdfium_candidate_step
        and 'IRIS_PDFIUM_PROFILE: "8179"' in pdfium_candidate_step
        and all(
            "        if: matrix.os == 'ubuntu-latest'\n" in ci_step_block(step, "test")
            for step in (
                "Package Linux AppDir and Debian installer",
                "Build PDFium smoke test",
                "Clean-installed Debian PDFium thumbnail smoke",
            )
        )
    ),
    "PDFium attestation credential is unset before the native smoke": (
        in_order(
            pdfium_candidate_step,
            './scripts/fetch-pdfium-runtime.sh "$stage"',
            '[[ $status == verified ]] || { echo "PDFium candidate attestation status is $status" >&2; exit 1; }',
            "unset GH_TOKEN GITHUB_TOKEN",
            'export IRIS_PDFIUM_REQUIRE_LIBRARY="$library"',
            '(cd "$stage/usr/lib/iris" && "$GITHUB_WORKSPACE/target/release/examples/pdf_thumbnail_smoke") > "$output"',
        )
        and all(snippet in pdfium_candidate_step for snippet in (
            "if len(records) != 1:",
            'if record.get("required") is not True:',
            "if recorded != required:",
        ))
    ),
    "CI invokes Windows native validation only on manual dispatch": """  windows-ffmpeg-validation:
    if: github.event_name == 'workflow_dispatch'
    permissions:
      contents: read
      attestations: read
    uses: ./.github/workflows/windows-ffmpeg-validation.yml
""" in ci,
    "Windows native validation is workflow_call only with a gated asset export": """on:
  workflow_call:
    inputs:
      export_release_assets:
"""
    in win_validation
    and all(
        trigger not in win_validation
        for trigger in ("  workflow_dispatch:", "  pull_request:", "  push:")
    ),
    "Windows native validation checks out the caller's exact SHA": """      - uses: actions/checkout@v4
        with:
          ref: ${{ github.sha }}
          persist-credentials: false
""" in win_validation,
}
failed = [name for name, passed in checks.items() if not passed]
if failed:
    raise SystemExit("workflow trigger regression: " + "; ".join(failed))
print("CI trigger and reusable report-only audit dispatch checks passed")
