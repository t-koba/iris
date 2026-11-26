#!/usr/bin/env python3
"""Offline unit tests for the release parity acceptance gate (no cargo/network)."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "release-parity.py"
POLICY = ROOT / "scripts" / "release-parity.json"
spec = importlib.util.spec_from_file_location("release_parity", SCRIPT)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

CANDIDATE = "b" * 40
OTHER_SHA = "c" * 40
RUN_ID = "123456789"
PIXELS = "22" * 32
HARNESS_DIGEST = hashlib.sha256(
    (ROOT / "crates" / "thumb-preview" / "examples" / m.HARNESS).read_bytes()).hexdigest()
CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7"
SETUP_PYTHON = "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1 # v6"
UPLOAD = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4"


def identity(platform, revision, run_id=RUN_ID):
    platform_identity = m.PLATFORM_IDENTITY[platform]
    return dict(
        identity_schema=m.IDENTITY_SCHEMA, source_revision=revision,
        workflow_run_id=run_id, rustc="rustc 1.97.1",
        target_os=platform_identity["target_os"], target_arch="x86_64",
        runner_name="runner", runner_os=platform_identity["runner_os"],
        runner_arch="X64", runner_image_os="image", runner_image_version="1",
        cpu_model="cpu", available_parallelism=4, build_profile="release",
        release_opt_level="3", features=dict(video=True, pdf=True, raw_full=True),
        ffmpeg_runtime=dict(codec_version=618100, format_version=618100,
                            util_version=591100),
        cargo_lock_sha256="11" * 32,
    )


def output():
    return dict(format="jpeg", width=128, height=128, bytes=2113,
                source="FullDecode", decoded_rgb_sha256=PIXELS)


def timing(operation, platform, revision, median, run_id=RUN_ID, warmups=2, samples=21):
    return dict(
        record="iris.preview_latency.v1", operation=operation,
        uses_ffmpeg=operation.startswith("video::"),
        input=dict(filename=m.FIXTURE, sha256=m.FIXTURE_SHA256,
                   size_bytes=m.FIXTURE_SIZE),
        output=output(), identity=identity(platform, revision, run_id),
        timing=dict(unit="nanoseconds", warmup_iterations=warmups,
                    sample_count=samples, min=70, median=median,
                    p90=120, median_absolute_deviation=5, max=130),
    )


def section(platform, revision, operations, median, run_id=RUN_ID, warmups=2, samples=21):
    return dict(
        correctness=dict(record="iris.preview_correctness.v1", passed=True,
                         fixtures=[dict(filename=m.FIXTURE, sha256=m.FIXTURE_SHA256,
                                        size_bytes=m.FIXTURE_SIZE, output=output())],
                         identity=identity(platform, revision, run_id)),
        timings=[timing(op, platform, revision, median, run_id, warmups, samples)
                 for op in operations],
        pdfium_runtime=dict(path="/opt/pdfium/libpdfium.so", required=True),
    )


def resummarize(report):
    paired, paired_j2k, unpaired = m.pairing_plan(report["baseline"], report["candidate"])
    report.update(paired_latency=paired, paired_j2k=paired_j2k, unpaired=unpaired)
    return report


def make_report(platform, candidate=CANDIDATE, baseline=m.BASELINE_SHA, run_id=RUN_ID,
                baseline_median=100, candidate_median=90):
    operations = sorted(m.PREVIEW_OPERATIONS)
    report = dict(
        schema="iris.parity_source_compare.v1",
        parity_claim=m.PARITY_CLAIM,
        source_revision=dict(baseline=baseline, candidate=candidate),
        harness_sha256=HARNESS_DIGEST,
        fixture_pin=dict(filename=m.FIXTURE, sha256=m.FIXTURE_SHA256,
                         size_bytes=m.FIXTURE_SIZE),
        baseline=section(platform, baseline, operations, baseline_median, run_id),
        candidate=section(platform, candidate, operations, candidate_median, run_id),
    )
    report["baseline_j2k"] = section(platform, baseline, [m.J2K_OPERATION],
                                    baseline_median, run_id)
    report["candidate_j2k"] = section(platform, candidate, [m.J2K_OPERATION],
                                     candidate_median, run_id)
    # pairing_plan() expects corpus and J2K under each source revision.
    report["baseline"] = dict(corpus=report["baseline"], j2k=report.pop("baseline_j2k"))
    report["candidate"] = dict(corpus=report["candidate"], j2k=report.pop("candidate_j2k"))
    return resummarize(report)


class ReleaseParityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="iris-release-parity-test-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.repo = self.work / "repo"
        examples = self.repo / "crates" / "thumb-preview" / "examples"
        examples.mkdir(parents=True)
        shutil.copyfile(ROOT / "crates" / "thumb-preview" / "examples" / m.HARNESS,
                        examples / m.HARNESS)
        self.reports = {platform: make_report(platform) for platform in m.PLATFORMS}
        for platform, report in self.reports.items():
            self.save(platform, report)

    def save(self, platform, report):
        self.reports[platform] = report
        path = self.work / f"{platform}.json"
        path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8")
        return path

    def gate(self, expected=CANDIDATE, policy=POLICY, reports=None):
        paths = [str(self.save(platform, self.reports[platform]))
                 for platform in (reports or m.PLATFORMS)]
        command = [sys.executable, str(SCRIPT), "--repo", str(self.repo),
                   "--expected-sha", expected, "--policy", str(policy)] + paths
        return subprocess.run(command, capture_output=True, text=True, check=False)

    def assert_rejected(self, **kwargs):
        result = self.gate(**kwargs)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertTrue(result.stderr.strip(), "failure must explain itself on stderr")
        return result

    def test_accepts_three_valid_reports(self):
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("linux, macos and windows", result.stdout)

    def test_rejects_wrong_candidate_sha(self):
        report = copy.deepcopy(self.reports["linux"])
        report["source_revision"]["candidate"] = OTHER_SHA
        self.save("linux", report)
        self.assert_rejected()
        self.save("linux", make_report("linux"))
        self.assert_rejected(expected=OTHER_SHA)

    def test_rejects_wrong_baseline(self):
        report = copy.deepcopy(self.reports["linux"])
        report["source_revision"]["baseline"] = OTHER_SHA
        self.save("linux", report)
        self.assert_rejected()

    def test_rejects_stale_and_oversized_reports(self):
        stale = copy.deepcopy(self.reports["linux"])
        stale["schema"] = "iris.parity_source_compare.v0"
        self.save("linux", stale)
        self.assert_rejected()
        oversized = copy.deepcopy(self.reports["linux"])
        oversized["limits"] = "x" * 300000
        self.save("linux", oversized)
        self.assert_rejected()

    def test_rejects_missing_platform(self):
        self.assert_rejected(reports=("linux", "macos"))

    def test_rejects_unpaired_section(self):
        report = copy.deepcopy(self.reports["linux"])
        report["unpaired"] = [dict(section="corpus", reason="simulated")]
        self.save("linux", report)
        self.assert_rejected()
        legacy = copy.deepcopy(self.reports["linux"])
        legacy["baseline"]["corpus"]["correctness"]["identity"]["identity_schema"] = \
            "iris.preview_identity.v1"
        self.save("linux", resummarize(legacy))
        self.assert_rejected()

    def test_rejects_positive_delta(self):
        report = copy.deepcopy(self.reports["linux"])
        report["candidate"]["corpus"]["timings"][1]["timing"]["median"] = 140
        report["candidate"]["j2k"]["timings"][0]["timing"]["median"] = 140
        self.save("linux", resummarize(report))
        result = self.assert_rejected()
        self.assertIn("candidate slowdown", result.stderr)

    def test_rejects_tampered_paired_summary(self):
        report = copy.deepcopy(self.reports["linux"])
        report["paired_latency"][0]["candidate_minus_baseline_ns"] += 1
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("tampered", result.stderr)

    def test_rejects_wrong_platform_identity(self):
        report = copy.deepcopy(self.reports["macos"])
        for side in ("baseline", "candidate"):
            report[side]["corpus"]["timings"][0]["identity"].update(
                target_os="linux", runner_os="Linux")
        self.save("macos", report)
        result = self.assert_rejected()
        self.assertIn("platform identity mismatch", result.stderr)

    def test_rejects_differing_workflow_run_id(self):
        report = copy.deepcopy(self.reports["windows"])
        report["baseline"]["j2k"]["correctness"]["identity"]["workflow_run_id"] = "999"
        self.save("windows", report)
        result = self.assert_rejected()
        self.assertIn("different workflow runs", result.stderr)

    def test_rejects_missing_operation(self):
        report = copy.deepcopy(self.reports["linux"])
        for side in ("baseline", "candidate"):
            report[side]["corpus"]["timings"] = [
                item for item in report[side]["corpus"]["timings"]
                if item["operation"] != "pdf::extract_pdf_thumbnail"]
        self.save("linux", resummarize(report))
        result = self.assert_rejected()
        self.assertIn("operation set mismatch", result.stderr)

    def test_rejects_wrong_harness_digest(self):
        report = copy.deepcopy(self.reports["linux"])
        report["harness_sha256"] = "0" * 64
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("harness digest mismatch", result.stderr)

    def test_rejects_wrong_warmup_and_sample_count(self):
        for field, value in (("warmup_iterations", 1), ("sample_count", 20)):
            with self.subTest(field=field):
                report = copy.deepcopy(self.reports["linux"])
                for side in ("baseline", "candidate"):
                    for name in ("corpus", "j2k"):
                        for item in report[side][name]["timings"]:
                            item["timing"][field] = value
                self.save("linux", resummarize(report))
                self.assert_rejected()

    def test_rejects_invalid_policy(self):
        raw = json.loads(POLICY.read_text(encoding="utf-8"))
        for field, value in (("max_candidate_minus_baseline_ns", 1),
                             ("baseline_sha", OTHER_SHA), ("samples", 20)):
            with self.subTest(field=field):
                changed = copy.deepcopy(raw)
                changed[field] = value
                path = self.work / "policy.json"
                path.write_text(json.dumps(changed), encoding="utf-8")
                self.assert_rejected(policy=path)
        changed = copy.deepcopy(raw)
        changed["extra"] = True
        path = self.work / "policy.json"
        path.write_text(json.dumps(changed), encoding="utf-8")
        self.assert_rejected(policy=path)


class WorkflowContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.release = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8")
        cls.evidence = (ROOT / ".github" / "workflows" / "parity-evidence.yml").read_text(
            encoding="utf-8")

    @staticmethod
    def job_block(text, name):
        start = re.search(rf"(?m)^  {re.escape(name)}:\n", text)
        if not start:
            return ""
        following = re.search(r"(?m)^  [a-zA-Z0-9_-]+:\n", text[start.end():])
        end = start.end() + following.start() if following else len(text)
        return text[start.start():end]

    def test_release_wires_parity_evidence_before_policy_gate(self):
        job = self.job_block(self.release, "parity-evidence")
        for needle in ("    if: github.event_name == 'push'\n",
                       "    needs: release-preflight\n",
                       "    permissions:\n      contents: read\n",
                       "    uses: ./.github/workflows/parity-evidence.yml\n",
                       "      source_sha: ${{ needs.release-preflight.outputs.source_sha }}\n"):
            self.assertIn(needle, job)
        publish = self.job_block(self.release, "publish")
        self.assertIn(
            "    needs: [package-macos, package-linux, package-windows, parity-evidence]\n",
            publish)
        self.assertIn("        run: python scripts/test-release-parity.py\n", self.release)
        self.assertIn("          pattern: parity-*\n          merge-multiple: true\n"
                      "          path: audit/parity\n", publish)
        gate = publish.split("      - name: Gate publication on paired parity evidence\n",
                             1)[1].split("      - name: ", 1)[0]
        self.assertIn("--repo . --expected-sha \"$GITHUB_SHA\"", gate)
        self.assertIn("          audit/parity/linux.json audit/parity/macos.json "
                      "audit/parity/windows.json\n", gate)
        self.assertLess(publish.index("      - name: Gate publication on paired parity evidence\n"),
                        publish.index("      - name: Gate publication on release policy "))

    def test_reusable_workflow_has_three_pinned_evidence_jobs(self):
        self.assertIn("on:\n  workflow_call:\n    inputs:\n      source_sha:\n", self.evidence)
        self.assertIn("        required: true\n        type: string\n", self.evidence)
        self.assertIn("permissions:\n  contents: read\n", self.evidence)
        self.assertNotIn("publish", self.evidence)
        runners = dict(**{"parity-linux": "ubuntu-latest", "parity-macos": "macos-latest",
                          "parity-windows": "windows-latest"})
        for job, runner in runners.items():
            block = self.job_block(self.evidence, job)
            self.assertTrue(block)
            self.assertIn(f"    runs-on: {runner}\n    timeout-minutes: 90\n", block)
            self.assertIn(f"      - uses: {CHECKOUT}\n", block)
            self.assertIn("          ref: ${{ inputs.source_sha }}\n"
                          "          fetch-depth: 0\n"
                          "          persist-credentials: false\n", block)
            self.assertIn(f"      - uses: {SETUP_PYTHON}\n", block)
            self.assertIn("          python-version: '3.13'\n", block)
            self.assertIn("        shell: bash\n        env:\n"
                          "          SOURCE_SHA: ${{ inputs.source_sha }}\n", block)
            platform = job.removeprefix("parity-")
            self.assertIn(
                '        run: python3 scripts/parity_source_compare.py --repo . '
                '--candidate "$SOURCE_SHA" --corpus tests/preview-corpus '
                f'--output audit/parity/{platform}.json\n', block)
            self.assertIn(f"      - uses: {UPLOAD}\n", block)
            self.assertIn(f"          name: parity-{platform}\n"
                          f"          path: audit/parity/{platform}.json\n"
                          "          if-no-files-found: error\n", block)
        self.assertEqual(self.evidence.count("uses: actions/upload-artifact@"), 3)
        self.assertEqual(self.evidence.count("          name: parity-"), 3)


if __name__ == "__main__":
    unittest.main()
