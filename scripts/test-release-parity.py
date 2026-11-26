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
CORPUS_MANIFEST = json.loads(
    (ROOT / "tests" / "preview-corpus" / "fixtures.json").read_text(encoding="utf-8"))
HARNESS_DIGEST = hashlib.sha256(
    (ROOT / "crates" / "thumb-preview" / "examples" / m.HARNESS).read_bytes()).hexdigest()
BUILD_SCRIPT_DIGEST = hashlib.sha256((ROOT / m.BUILD_SCRIPT).read_bytes()).hexdigest()
CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7"
SETUP_PYTHON = "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1 # v6"
UPLOAD = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4"
RUST_TOOLCHAIN = "dtolnay/rust-toolchain@df2a88e9fe4098bad09cb42c3dec7187f15861fc # master"
RUST_CACHE = "Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2"
PINNED_FFMPEG = m.pinned_ffmpeg_identity()
VCPKG_FFMPEG_FLAGS = (
    "--toolchain=msvc --enable-pic --disable-doc --enable-runtime-cpudetect "
    "--disable-autodetect --target-os=win32 --enable-w32threads --enable-d3d11va "
    "--enable-d3d12va --enable-dxva2 --enable-mediafoundation --disable-ffmpeg "
    "--disable-ffplay --disable-ffprobe --enable-avcodec --enable-avdevice "
    "--enable-avformat --enable-avfilter --enable-swresample --enable-swscale"
)


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
        ffmpeg_runtime=dict(
            codec_version=PINNED_FFMPEG['codec_version'], format_version=618100,
            util_version=591100,
            configuration=(VCPKG_FFMPEG_FLAGS if platform == 'windows' else
                           '--prefix=/pinned/ffmpeg --libdir=/pinned/ffmpeg/lib '
                           + PINNED_FFMPEG['configuration']),
            license=PINNED_FFMPEG['license']),
        cargo_lock_sha256="11" * 32,
    )


def output():
    return dict(format="jpeg", width=128, height=128, bytes=2113,
                source="FullDecode", decoded_rgb_sha256=PIXELS)


def fixture_pin(filename, sha256, size_bytes):
    return dict(filename=filename, sha256=sha256, size_bytes=size_bytes)


def corpus_fixtures():
    """Correctness records for every input in the verifier's pinned manifest."""
    return [dict(fixture_pin(entry["filename"], entry["sha256"], entry["size"]),
                 output=output())
            for entry in CORPUS_MANIFEST["files"]]


def j2k_fixture():
    return dict(fixture_pin(m.FIXTURE, m.FIXTURE_SHA256, m.FIXTURE_SIZE),
                output=output())


def timing(operation, platform, revision, median, run_id=RUN_ID, warmups=2, samples=21,
           input_pin=None):
    input_pin = input_pin or fixture_pin(m.FIXTURE, m.FIXTURE_SHA256, m.FIXTURE_SIZE)
    return dict(
        record="iris.preview_latency.v1", operation=operation,
        uses_ffmpeg=operation.startswith("video::"),
        input=copy.deepcopy(input_pin),
        output=output(), identity=identity(platform, revision, run_id),
        timing=dict(unit="nanoseconds", warmup_iterations=warmups,
                    sample_count=samples, min=70, median=median,
                    p90=120, median_absolute_deviation=5, max=130),
    )


def section(platform, revision, operations, median, run_id=RUN_ID, warmups=2, samples=21):
    corpus = set(operations) == set(m.PREVIEW_OPERATIONS)
    fixtures = corpus_fixtures() if corpus else [j2k_fixture()]
    first_pin = next(entry for entry in CORPUS_MANIFEST['files']
                     if entry['filename'] == 'landscape.jpg') if corpus else None
    timing_pin = (fixture_pin(first_pin['filename'], first_pin['sha256'], first_pin['size'])
                  if corpus else fixture_pin(m.FIXTURE, m.FIXTURE_SHA256, m.FIXTURE_SIZE))
    result = dict(
        correctness=dict(record="iris.preview_correctness.v1", passed=True,
                         fixtures=fixtures,
                         identity=identity(platform, revision, run_id)),
        timings=[timing(op, platform, revision, median, run_id, warmups, samples,
                        timing_pin) for op in operations],
    )
    # macOS renders PDFs through CoreGraphics; the smoke emits no PDFium line.
    if platform != "macos":
        result["pdfium_runtime"] = dict(path="/opt/pdfium/libpdfium.so", required=True)
    return result


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
        build_script_sha256=BUILD_SCRIPT_DIGEST,
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
        script = self.repo / m.BUILD_SCRIPT
        script.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / m.BUILD_SCRIPT, script)
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

    def test_fixture_complete_reports_fit_the_report_budget(self):
        budget = json.loads(POLICY.read_text(encoding="utf-8"))["max_report_bytes"]
        for report in self.reports.values():
            for side in ("baseline", "candidate"):
                self.assertEqual(len(report[side]["corpus"]["correctness"]["fixtures"]),
                                 len(CORPUS_MANIFEST["files"]))
        for platform, report in self.reports.items():
            payload = json.dumps(report, indent=2, sort_keys=True) + "\n"
            self.assertLessEqual(len(payload.encode("utf-8")), budget, platform)

    def test_accepts_pinned_baseline_shape_with_shared_corpus_harness(self):
        source_spec = importlib.util.spec_from_file_location(
            "parity_source_compare", ROOT / "scripts" / "parity_source_compare.py")
        source = importlib.util.module_from_spec(source_spec)
        source_spec.loader.exec_module(source)
        self.assertIn(source.CORPUS_HARNESS, source.STAGED_EXAMPLES)
        for platform in m.PLATFORMS:
            self.save(platform, make_report(platform, baseline=m.BASELINE_SHA))
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("linux, macos and windows", result.stdout)

    def test_accepts_pinned_ffmpeg_runtime_from_policy(self):
        pin = m.pinned_ffmpeg_identity()
        self.assertEqual(pin['codec_version'], (62 << 16) | (28 << 8) | 103)
        self.assertEqual(pin['license'], 'LGPL version 2.1 or later')
        self.assertIn('--disable-gpl', pin['configuration'])
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_accepts_pinned_vcpkg_flags_on_windows(self):
        """The pinned Windows port uses different flags, but the same ABI and license."""
        self.assertIn('--toolchain=msvc', VCPKG_FFMPEG_FLAGS)
        self.assertNotIn(PINNED_FFMPEG['configuration'], VCPKG_FFMPEG_FLAGS)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_posix_flags_on_windows(self):
        report = copy.deepcopy(self.reports['windows'])
        for side in ('baseline', 'candidate'):
            for record in [report[side]['corpus']['correctness'],
                           *report[side]['corpus']['timings']]:
                record['identity']['ffmpeg_runtime']['configuration'] = (
                    PINNED_FFMPEG['configuration'])
        self.save('windows', report)
        result = self.assert_rejected()
        self.assertIn('pinned vcpkg FFmpeg flags mismatch', result.stderr)

    def test_rejects_forbidden_vcpkg_flags_on_windows(self):
        report = copy.deepcopy(self.reports['windows'])
        for side in ('baseline', 'candidate'):
            for record in [report[side]['corpus']['correctness'],
                           *report[side]['corpus']['timings']]:
                record['identity']['ffmpeg_runtime']['configuration'] += ' --enable-gpl'
        self.save('windows', report)
        result = self.assert_rejected()
        self.assertIn('pinned vcpkg FFmpeg flags mismatch', result.stderr)

    def test_rejects_system_ffmpeg_runtime_on_both_sides(self):
        """A matching pair is still invalid when neither side uses the pin."""
        for field, value in (
            ('codec_version', 618100),
            ('configuration', '--enable-shared --disable-gpl --enable-zlib'),
            ('license', 'GPL version 3 or later'),
        ):
            with self.subTest(field=field):
                report = copy.deepcopy(self.reports['linux'])
                for side in ('baseline', 'candidate'):
                    for record in [report[side]['corpus']['correctness'],
                                   *report[side]['corpus']['timings']]:
                        record['identity']['ffmpeg_runtime'][field] = value
                self.save('linux', report)
                result = self.assert_rejected()
                self.assertIn('pinned FFmpeg', result.stderr)

    def test_rejects_ffmpeg_runtime_drift_on_one_side(self):
        for field, value in (
            ('codec_version', PINNED_FFMPEG['codec_version'] + 1),
            ('configuration', PINNED_FFMPEG['configuration'] + ' --enable-gpl'),
            ('license', 'LGPL version 3 or later'),
        ):
            with self.subTest(field=field):
                report = copy.deepcopy(self.reports['linux'])
                for record in [report['candidate']['corpus']['correctness'],
                               *report['candidate']['corpus']['timings']]:
                    record['identity']['ffmpeg_runtime'][field] = value
                self.save('linux', report)
                self.assert_rejected()

    def test_rejects_ffmpeg_runtime_drift_in_timing_only(self):
        report = copy.deepcopy(self.reports['linux'])
        for side in ('baseline', 'candidate'):
            for record in report[side]['corpus']['timings']:
                if record['uses_ffmpeg']:
                    record['identity']['ffmpeg_runtime']['configuration'] += ' --enable-gpl'
        self.save('linux', report)
        result = self.assert_rejected()
        self.assertIn('pinned FFmpeg configuration mismatch', result.stderr)

    def test_rejects_missing_ffmpeg_runtime_identity(self):
        report = copy.deepcopy(self.reports['linux'])
        for side in ('baseline', 'candidate'):
            for record in report[side]['corpus']['timings']:
                if record['uses_ffmpeg']:
                    record['identity']['ffmpeg_runtime'].pop('configuration')
        self.save('linux', report)
        result = self.assert_rejected()
        self.assertIn('FFmpeg configuration', result.stderr)

    def test_rejects_wrong_candidate_sha(self):
        report = copy.deepcopy(self.reports["linux"])
        report["source_revision"]["candidate"] = OTHER_SHA
        self.save("linux", report)
        self.assert_rejected()
        self.save("linux", make_report("linux"))
        self.assert_rejected(expected=OTHER_SHA)

    def test_rejects_missing_pdfium_attribution_on_native_platforms(self):
        for platform in ("linux", "windows"):
            with self.subTest(platform=platform):
                report = copy.deepcopy(self.reports[platform])
                report["baseline"]["corpus"].pop("pdfium_runtime", None)
                self.save(platform, report)
                self.assert_rejected()
                self.save(platform, make_report(platform))
        report = copy.deepcopy(self.reports["linux"])
        report["candidate"]["corpus"]["pdfium_runtime"] = dict(path="  ", required=True)
        self.save("linux", report)
        self.assert_rejected()
        self.save("linux", make_report("linux"))

    def test_rejects_pdfium_attribution_removed_from_both_sides(self):
        for platform in ("linux", "windows"):
            with self.subTest(platform=platform):
                report = copy.deepcopy(self.reports[platform])
                for side in ("baseline", "candidate"):
                    report[side]["corpus"].pop("pdfium_runtime", None)
                paired, paired_j2k, unpaired = m.pairing_plan(
                    report["baseline"], report["candidate"])
                self.assertEqual(unpaired, [])
                report["paired_latency"] = paired
                report["paired_j2k"] = paired_j2k
                report["unpaired"] = unpaired
                self.save(platform, report)
                result = self.assert_rejected()
                self.assertIn(
                    "baseline corpus has no loaded PDFium library attribution",
                    result.stderr)
                self.save(platform, make_report(platform))

    def test_accepts_macos_without_pdfium_attribution(self):
        report = self.reports["macos"]
        self.assertNotIn("pdfium_runtime", report["baseline"]["corpus"])
        self.assertNotIn("pdfium_runtime", report["candidate"]["corpus"])
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

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

    def test_rejects_false_correctness_on_both_sides(self):
        """Agreed failure on both sides cannot pass the consumer boundary."""
        for name in ("corpus", "j2k"):
            with self.subTest(section=name):
                report = copy.deepcopy(self.reports["linux"])
                for side in ("baseline", "candidate"):
                    report[side][name]["correctness"]["passed"] = False
                self.save("linux", resummarize(report))
                result = self.assert_rejected()
                self.assertIn("correctness.passed must be true", result.stderr)

    def test_rejects_empty_corpus_fixture_coverage_on_both_sides(self):
        report = copy.deepcopy(self.reports["linux"])
        for side in ("baseline", "candidate"):
            report[side]["corpus"]["correctness"]["fixtures"] = []
        self.save("linux", resummarize(report))
        result = self.assert_rejected()
        self.assertIn("fixture coverage", result.stderr)

    def test_rejects_wrong_pinned_fixture_on_both_sides(self):
        for name in ("corpus", "j2k"):
            for field, value in (("sha256", "00" * 32), ("size_bytes", 1)):
                with self.subTest(section=name, field=field):
                    report = copy.deepcopy(self.reports["linux"])
                    for side in ("baseline", "candidate"):
                        report[side][name]["correctness"]["fixtures"][0][field] = value
                    self.save("linux", resummarize(report))
                    result = self.assert_rejected()
                    self.assertIn("fixture pin mismatch", result.stderr)

    def test_rejects_unpinned_timing_input_on_both_sides(self):
        report = copy.deepcopy(self.reports['linux'])
        for side in ('baseline', 'candidate'):
            for record in report[side]['corpus']['timings']:
                record['input'] = fixture_pin('unpinned.bin', '11' * 32, 1)
        self.save('linux', resummarize(report))
        result = self.assert_rejected()
        self.assertIn('timing input is not a pinned input', result.stderr)

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

    def test_rejects_invalid_compiled_build_identity(self):
        for profile, level in (("release", None), ("release", "9"), ("debug", "3"),
                               (None, "3")):
            with self.subTest(profile=profile, level=level):
                report = copy.deepcopy(self.reports["linux"])
                for side in ("baseline", "candidate"):
                    for name in ("corpus", "j2k"):
                        section = report[side][name]
                        for record in [section["correctness"], *section["timings"]]:
                            record["identity"]["build_profile"] = profile
                            record["identity"]["release_opt_level"] = level
                self.save("linux", resummarize(report))
                result = self.assert_rejected()
                self.assertTrue("invalid build profile" in result.stderr
                                or "invalid release_opt_level" in result.stderr, result.stderr)

    def test_rejects_wrong_harness_digest(self):
        report = copy.deepcopy(self.reports["linux"])
        report["harness_sha256"] = "0" * 64
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("harness digest mismatch", result.stderr)

    def test_rejects_tampered_build_script_digest(self):
        report = copy.deepcopy(self.reports["linux"])
        report["build_script_sha256"] = "0" * 64
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("build script digest mismatch", result.stderr)

    def test_rejects_stale_build_script_binding_after_repo_change(self):
        (self.repo / m.BUILD_SCRIPT).write_text("fn main() {}\n", encoding="utf-8")
        result = self.assert_rejected()
        self.assertIn("build script digest mismatch", result.stderr)

    def test_rejects_opt_level_drift_even_with_matching_build_script_digest(self):
        script = self.repo / m.BUILD_SCRIPT
        script.write_text("fn main() {}\n", encoding="utf-8")
        report = copy.deepcopy(self.reports["linux"])
        report["build_script_sha256"] = hashlib.sha256(script.read_bytes()).hexdigest()
        for name in ("corpus", "j2k"):
            section = report["candidate"][name]
            for record in [section["correctness"], *section["timings"]]:
                record["identity"]["release_opt_level"] = "2"
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("identity mismatch", result.stderr)

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
        self.assertNotIn("--corpus tests/preview-corpus", self.evidence)
        self.assertNotIn("package-linux-appdir.sh", self.evidence)
        self.assertNotIn("package-macos-app.sh", self.evidence)
        self.assertNotIn("softprops/action-gh-release", self.evidence)
        self.assertNotIn("workflow_dispatch", self.evidence)
        runners = dict(**{"parity-linux": "ubuntu-latest", "parity-macos": "macos-latest",
                          "parity-windows": "windows-latest"})
        for job, runner in runners.items():
            block = self.job_block(self.evidence, job)
            self.assertTrue(block, f"missing job {job}")
            self.assertIn(f"    runs-on: {runner}\n    timeout-minutes: 90\n", block)
            self.assertIn(f"      - uses: {CHECKOUT}\n", block)
            self.assertIn("          ref: ${{ inputs.source_sha }}\n"
                          "          fetch-depth: 0\n"
                          "          persist-credentials: false\n", block)
            self.assertIn(f"      - uses: {SETUP_PYTHON}\n", block)
            self.assertIn("          python-version: '3.13'\n", block)
            self.assertIn(f"      - uses: {RUST_TOOLCHAIN}\n", block)
            self.assertIn("          toolchain: 1.97.1\n", block)
            self.assertIn(f"      - uses: {RUST_CACHE}\n", block)
            self.assertIn("cargo fetch --locked", block)
            for needle in ("IRIS_PREVIEW_CORPUS_MAX_BYTES:",
                           "IRIS_PREVIEW_CORPUS_TIMEOUT_SECONDS:",
                           "IRIS_PREVIEW_CORPUS_RETRIES:",
                           "IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS:",
                           "IRIS_PREVIEW_LATENCY_WARMUPS:",
                           "IRIS_PREVIEW_LATENCY_SAMPLES:"):
                self.assertIn(needle, block, f"{job} misses corpus/latency env {needle}")
            self.assertIn("IRIS_PREVIEW_CORPUS_MAX_BYTES: ${{ vars.IRIS_PREVIEW_CORPUS_MAX_BYTES", block)
            self.assertIn("        run: python3 scripts/fetch-preview-corpus.py --output", block)
            self.assertNotIn("--corpus tests/preview-corpus", block)
            self.assertIn("        shell: bash\n        env:\n"
                          "          SOURCE_SHA: ${{ inputs.source_sha }}\n", block)
            platform = job.removeprefix("parity-")
            self.assertIn(
                f'--corpus "$RUNNER_TEMP/iris-preview-corpus" '
                f'--output audit/parity/{platform}.json\n', block)
            self.assertIn('--candidate "$SOURCE_SHA"', block)
            self.assertIn(
                f'        run: python3 scripts/parity_source_compare.py --repo . '
                f'--candidate "$SOURCE_SHA" --corpus "$RUNNER_TEMP/iris-preview-corpus" '
                f'--output audit/parity/{platform}.json\n', block)
            if platform == "linux":
                self.assertIn("sudo apt-get install -y", block)
                for package in ("pkg-config", "clang", "nasm"):
                    self.assertIn(package, block, f"linux misses build package {package}")
                for package in ("libavcodec-dev", "libavformat-dev", "libavutil-dev",
                                "libavfilter-dev", "libavdevice-dev",
                                "libswscale-dev", "libswresample-dev"):
                    self.assertNotIn(package, block, f"linux installs system FFmpeg {package}")
                self.assertIn("./scripts/fetch-pdfium-runtime.sh", block)
                self.assertIn("libpdfium.so", block)
                self.assertIn("IRIS_VERIFY_PDFIUM_PROVENANCE", block)
                self.assertIn("shared-key: parity-linux-cargo", block)
            elif platform == "macos":
                self.assertIn("brew install pkg-config gnupg nasm cmake", block)
                self.assertNotIn("brew install ffmpeg", block)
                self.assertIn("python3 scripts/test-macos-ffmpeg.py", block)
                self.assertIn("IRIS_FFMPEG_JOBS:", block)
                self.assertIn("shared-key: parity-macos-cargo", block)
            else:
                self.assertIn("microsoft/vcpkg", block)
                self.assertIn("b8dfe957ec8b5c0ef5c4b31eea594b8486af744f", block)
                self.assertIn("ilammy/msvc-dev-cmd@0b201ec74fa43914dc39ae48a89fd1d8cb592756 # v1.13.0", block)
                self.assertIn("vcpkg.exe", block)
                self.assertIn("VCPKG_TARGET_TRIPLET: x64-windows-iris-dynamic", block)
                self.assertIn("windows-ffmpeg-source.py", block)
                self.assertIn("windows-ffmpeg-overlay.py derive $portfile $sourceDir", block)
                self.assertIn("gh attestation verify", block)
                self.assertIn("chromium/8138", block)
                self.assertIn("IRIS_VALIDATION_PDFIUM_RUNTIME", block)
                self.assertIn("shared-key: parity-windows-cargo", block)
            self.assertIn(f"      - uses: {UPLOAD}\n", block)
            self.assertIn(f"          name: parity-{platform}\n"
                          f"          path: audit/parity/{platform}.json\n"
                          "          if-no-files-found: error\n", block)
        self.assertEqual(self.evidence.count("uses: actions/upload-artifact@"), 3)
        self.assertEqual(self.evidence.count("          name: parity-"), 3)
        self.assertEqual(self.evidence.count("python3 scripts/parity_source_compare.py"), 3)
        self.assertEqual(self.evidence.count("python3 scripts/fetch-preview-corpus.py --output"), 3)

    def test_paired_evidence_jobs_provision_the_pinned_runtime(self):
        linux = self.job_block(self.evidence, 'parity-linux')
        macos = self.job_block(self.evidence, 'parity-macos')
        for needle in ('IRIS_FFMPEG_PREFIX: ${{ github.workspace }}/build/linux-ffmpeg\n',
                       'FFMPEG_DIR: ${{ github.workspace }}/build/linux-ffmpeg\n',
                       'PKG_CONFIG_PATH: ${{ github.workspace }}/build/linux-ffmpeg/lib/pkgconfig\n',
                       'PKG_CONFIG_LIBDIR: ${{ github.workspace }}/build/linux-ffmpeg/lib/pkgconfig\n',
                       'LD_LIBRARY_PATH: ${{ github.workspace }}/build/linux-ffmpeg/lib\n',
                       './scripts/build-linux-ffmpeg.sh\n',
                       'python3 scripts/linux-ffmpeg.py validate "$IRIS_FFMPEG_PREFIX"\n'):
            self.assertIn(needle, linux, f'linux parity misses {needle!r}')
        self.assertNotIn('libavcodec-dev', linux)
        self.assertIn('python3 scripts/macos-ffmpeg.py prepare "$prefix"\n', macos)
        for needle in ('echo "FFMPEG_DIR=$prefix"\n',
                       'echo "PKG_CONFIG_PATH=$prefix/lib/pkgconfig"\n',
                       'echo "PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig"\n',
                       'echo "DYLD_LIBRARY_PATH=$prefix/lib',
                       'test "$(pkg-config --variable=prefix libavcodec)" = "$prefix"\n'):
            self.assertIn(needle, macos, f'macOS parity misses {needle!r}')
        self.assertNotIn('brew install ffmpeg', macos)
        windows = self.job_block(self.evidence, 'parity-windows')
        self.assertIn('python3 scripts/windows-ffmpeg-overlay.py derive', windows)

    def test_each_parity_job_fetches_bytes_into_its_corpus_directory(self):
        jobs = ["parity-linux", "parity-macos", "parity-windows"]
        self.assertEqual(len(jobs), 3)
        for job in jobs[:10]:
            block = self.job_block(self.evidence, job)
            self.assertTrue(block, f"missing job {job}")
            fetches = re.findall(
                r'python3 scripts/fetch-preview-corpus\.py --output "?([^"\s\n]+)"?', block)
            smokes = re.findall(
                r'python3 scripts/parity_source_compare\.py[^\n]*?--corpus "?([^"\s\n]+)"?', block)
            self.assertEqual(len(fetches), 1, f"{job} must fetch corpus bytes once")
            self.assertEqual(len(smokes), 1, f"{job} must run the parity smoke once")
            fetch_dir, corpus_dir = fetches[0].rstrip('"'), smokes[0].rstrip('"')
            for value in (fetch_dir, corpus_dir):
                self.assertLessEqual(len(value), 256, f"{job} corpus path is unbounded")
                self.assertNotIn("tests/preview-corpus", value, f"{job} uses manifest-only corpus")
            self.assertEqual(fetch_dir, corpus_dir,
                             f"{job} must pass its fetched bytes as --corpus, not a manifest")
            self.assertIn("RUNNER_TEMP", fetch_dir, f"{job} corpus must be job-local")
            self.assertIn("iris-preview-corpus", fetch_dir, f"{job} corpus must be a fresh dir")


if __name__ == "__main__":
    unittest.main()
