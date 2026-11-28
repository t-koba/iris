#!/usr/bin/env python3
"""Offline unit tests for the release parity acceptance gate (no cargo/network)."""

import copy
import hashlib
import importlib.util
import json
import ntpath
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
sys.path.insert(0, str(ROOT / "scripts"))
from product_build_profile import load as load_product_profiles  # noqa: E402
from parity_source_compare import BASELINE_SHA  # noqa: E402

PRODUCT_PROFILES = load_product_profiles()

CANDIDATE = "b" * 40
OTHER_SHA = "c" * 40
RUN_ID = "123456789"
PIXELS = "22" * 32
CORPUS_MANIFEST = json.loads(
    (ROOT / "tests" / "preview-corpus" / "fixtures.json").read_text(encoding="utf-8"))
HARNESS_DIGEST = hashlib.sha256(
    (ROOT / "crates" / "thumb-preview" / "examples" / m.HARNESS).read_bytes()).hexdigest()
BUILD_SCRIPT_DIGEST = hashlib.sha256((ROOT / m.BUILD_SCRIPT).read_bytes()).hexdigest()
WORKTREES = {"baseline": "/tmp/iris-parity-source-baseline",
             "candidate": "/tmp/iris-parity-source-candidate"}
WINDOWS_WORKTREES = {
    "baseline": r"D:\a\_temp\iris-parity-source-baseline",
    "candidate": r"D:\a\_temp\iris-parity-source-candidate",
}


WINDOWS_LONG_ROOT = r"C:\Users\runneradmin\AppData\Local\Temp\iris-parity"
WINDOWS_SHORT_ROOT = r"C:\Users\RUNNER~1\AppData\Local\Temp\iris-parity"


def windows_root_worktrees(root):
    return {side: ntpath.join(root, side) for side in ("baseline", "candidate")}


def set_windows_attribution(report, worktrees, libraries, filename="pdfium.dll"):
    """Point each side's worktree identity and loaded library at given directories."""
    for side in ("baseline", "candidate"):
        report[side]["worktree"] = worktrees[side]
        report[side]["corpus"]["pdfium_runtime"]["path"] = ntpath.join(
            libraries[side], filename)
    return report


def report_worktree(platform, side):
    return WINDOWS_WORKTREES[side] if platform == "windows" else WORKTREES[side]


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
        cpu_model="cpu", available_parallelism=4,
        build_profile=PRODUCT_PROFILES[platform]["build_profile"],
        release_opt_level=PRODUCT_PROFILES[platform]["release_opt_level"],
        features=dict(video=True, pdf=True, raw_full=True),
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


def timing(operation, platform, revision, median, run_id=RUN_ID, warmups=2, samples=None,
           input_pin=None, deltas=None):
    samples = m.SAMPLES_PER_SIDE if samples is None else samples
    input_pin = input_pin or fixture_pin(m.FIXTURE, m.FIXTURE_SHA256, m.FIXTURE_SIZE)
    deltas = [0] * samples if deltas is None else list(deltas)
    assert len(deltas) == samples
    samples_ns = [median + delta for delta in deltas]
    if len(samples_ns) == m.SAMPLES_PER_SIDE:
        round_medians = [m.percentile(sorted(samples_ns[index * m.BLOCK_SAMPLES:(index + 1) * m.BLOCK_SAMPLES]), 50)
                         for index in range(m.ROUNDS)]
    else:
        round_medians = [median] * m.ROUNDS
    return dict(
        record="iris.preview_latency.v1", operation=operation,
        uses_ffmpeg=operation.startswith("video::"),
        input=copy.deepcopy(input_pin),
        output=output(), identity=identity(platform, revision, run_id),
        timing=dict(unit="nanoseconds", warmup_iterations=warmups, sample_count=samples,
                    rounds=m.ROUNDS, samples_ns=samples_ns,
                    round_medians_ns=list(round_medians),
                    **m.timing_statistics(samples_ns, 'test')),
    )


def deltas_for_round_medians(medians, base):
    """Convert absolute round medians into per-sample deltas around base."""
    samples = [value for median in medians for value in [median, median, median]]
    assert len(samples) == m.SAMPLES_PER_SIDE
    return [value - base for value in samples]


def pdfium_filename(platform):
    return "libpdfium.so" if platform == "linux" else "pdfium.dll"


def section(platform, revision, operations, median, run_id=RUN_ID, warmups=2, samples=None,
            worktree=None, deltas=None):
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
                        timing_pin, deltas) for op in operations],
    )
    # macOS renders PDFs through CoreGraphics; the smoke emits no PDFium line.
    if platform != "macos":
        name = pdfium_filename(platform)
        if platform == "windows":
            directory = worktree or r"D:\opt\pdfium"
            path = ntpath.join(directory, name)
        else:
            path = str(Path(worktree or "/opt/pdfium") / name)
        result["pdfium_runtime"] = dict(path=path, required=True)
    return result


def resummarize(report):
    paired, paired_j2k, unpaired = m.pairing_plan(report["baseline"], report["candidate"])
    report.update(paired_latency=paired, paired_j2k=paired_j2k, unpaired=unpaired)
    return report


def make_report(platform, candidate=CANDIDATE, baseline=m.BASELINE_SHA, run_id=RUN_ID,
                baseline_median=100, candidate_median=90,
                baseline_deltas=None, candidate_deltas=None):
    operations = sorted(m.PREVIEW_OPERATIONS)
    report = dict(
        schema="iris.parity_source_compare.v1",
        parity_claim=m.PARITY_CLAIM,
        source_revision=dict(baseline=baseline, candidate=candidate),
        harness_sha256=HARNESS_DIGEST,
        build_script_sha256=BUILD_SCRIPT_DIGEST,
        fixture_pin=dict(filename=m.FIXTURE, sha256=m.FIXTURE_SHA256,
                         size_bytes=m.FIXTURE_SIZE),
        baseline=section(platform, baseline, operations, baseline_median, run_id,
                         worktree=report_worktree(platform, "baseline"),
                         deltas=baseline_deltas),
        candidate=section(platform, candidate, operations, candidate_median, run_id,
                          worktree=report_worktree(platform, "candidate"),
                          deltas=candidate_deltas),
        measurement_rounds=m.measurement_rounds(),
    )
    report["baseline_j2k"] = section(platform, baseline, [m.J2K_OPERATION],
                                    baseline_median, run_id,
                                    worktree=report_worktree(platform, "baseline"),
                                    deltas=baseline_deltas)
    report["candidate_j2k"] = section(platform, candidate, [m.J2K_OPERATION],
                                     candidate_median, run_id,
                                     worktree=report_worktree(platform, "candidate"),
                                     deltas=candidate_deltas)
    # pairing_plan() expects corpus and J2K under each source revision.
    report["baseline"] = dict(worktree=report_worktree(platform, "baseline"),
                              corpus=report["baseline"], j2k=report.pop("baseline_j2k"))
    report["candidate"] = dict(worktree=report_worktree(platform, "candidate"),
                               corpus=report["candidate"], j2k=report.pop("candidate_j2k"))
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

    def test_rejects_unstaged_or_wrong_pdfium_attribution(self):
        original = copy.deepcopy(self.reports["linux"])
        filename = pdfium_filename("linux")

        def mutate(report, kind):
            if kind == "missing":
                for side in ("baseline", "candidate"):
                    report[side]["corpus"].pop("pdfium_runtime", None)
            elif kind == "wrong_directory":
                for side in ("baseline", "candidate"):
                    report[side]["corpus"]["pdfium_runtime"]["path"] = (
                        f"/opt/wrong/{filename}")
            elif kind == "wrong_filename":
                report["candidate"]["corpus"]["pdfium_runtime"]["path"] = (
                    "/tmp/iris-parity-source-candidate/pdfium.dll")
            elif kind == "not_required":
                report["candidate"]["corpus"]["pdfium_runtime"]["required"] = False
            else:
                report["candidate"].pop("worktree")

        for kind in ("missing", "wrong_directory", "wrong_filename", "not_required",
                     "missing_worktree"):
            with self.subTest(kind=kind):
                report = copy.deepcopy(original)
                mutate(report, kind)
                self.save("linux", report)
                self.assert_rejected()

    def test_report_path_parts_uses_producing_platform_syntax(self):
        self.assertEqual(m.report_path_parts(r"C:\a\work\PDFIUM.DLL", "windows"),
                         (r"c:\a\work", "pdfium.dll"))
        self.assertEqual(m.report_path_parts(r"D:/a/work/pdfium.dll"),
                         (r"d:\a\work", "pdfium.dll"))
        self.assertEqual(m.report_path_parts(r"\\?\D:\a\work\pdfium.dll"),
                         (r"d:\a\work", "pdfium.dll"))
        self.assertEqual(m.report_path_parts(r"\\server\share\work\pdfium.dll"),
                         (r"\\server\share\work", "pdfium.dll"))
        self.assertEqual(m.report_path_parts("/opt/dir/libpdfium.so", "linux"),
                         ("/opt/dir", "libpdfium.so"))
        self.assertEqual(m.report_path_parts("/opt/dir/libpdfium.so"),
                         ("/opt/dir", "libpdfium.so"))
        for value in ("", "  ", None, 17):
            with self.subTest(value=value), self.assertRaises(ValueError):
                m.report_path_parts(value)

    def test_accepts_windows_drive_and_extended_length_paths(self):
        report = copy.deepcopy(self.reports["windows"])
        for side in ("baseline", "candidate"):
            worktree = report[side]["worktree"]
            self.assertTrue(worktree.startswith("D:"))
            self.assertTrue(report[side]["corpus"]["pdfium_runtime"]["path"].startswith("D:"))
            report[side]["corpus"]["pdfium_runtime"]["path"] = (
                "\\\\?\\" + worktree + "\\pdfium.dll")
        self.save("windows", report)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_wrong_windows_worktree_pdfium_attribution(self):
        report = copy.deepcopy(self.reports["windows"])
        wrong = r"D:\a\_temp\wrong"
        for side in ("baseline", "candidate"):
            report[side]["corpus"]["pdfium_runtime"]["path"] = ntpath.join(
                wrong, "pdfium.dll")
        self.save("windows", report)
        result = self.assert_rejected()
        self.assertIn("PDFium path is not the staged worktree library", result.stderr)

    def test_accepts_windows_short_name_ancestor_on_both_sides(self):
        original = copy.deepcopy(self.reports["windows"])
        long_worktrees = windows_root_worktrees(WINDOWS_LONG_ROOT)
        short_worktrees = windows_root_worktrees(WINDOWS_SHORT_ROOT)
        for kind, worktrees, libraries in (
                ("short_library", long_worktrees, short_worktrees),
                ("short_worktree", short_worktrees, long_worktrees)):
            with self.subTest(kind=kind):
                report = copy.deepcopy(original)
                self.save("windows", set_windows_attribution(
                    report, worktrees, libraries))
                result = self.gate()
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_windows_paths_that_are_not_the_same_worktree(self):
        original = copy.deepcopy(self.reports["windows"])
        worktrees = windows_root_worktrees(WINDOWS_LONG_ROOT)
        roots = {
            "different_drive": r"D:\Users\RUNNER~1\AppData\Local\Temp\iris-parity",
            "different_temp_name": r"C:\Users\RUNNER~1\AppData\Local\Other\iris-parity",
            "extra_component": r"C:\Users\RUNNER~1\AppData\Local\Temp\extra\iris-parity",
        }
        for kind, root in roots.items():
            with self.subTest(kind=kind):
                report = copy.deepcopy(original)
                self.save("windows", set_windows_attribution(
                    report, worktrees, windows_root_worktrees(root)))
                result = self.assert_rejected()
                self.assertIn("PDFium path is not the staged worktree library",
                              result.stderr)
        with self.subTest(kind="different_final_directory"):
            report = copy.deepcopy(original)
            libraries = {side: ntpath.join(WINDOWS_SHORT_ROOT, "wrong")
                         for side in ("baseline", "candidate")}
            self.save("windows", set_windows_attribution(report, worktrees, libraries))
            result = self.assert_rejected()
            self.assertIn("PDFium path is not the staged worktree library",
                          result.stderr)

    def test_rejects_wrong_windows_filename_with_short_name_ancestor(self):
        report = copy.deepcopy(self.reports["windows"])
        self.save("windows", set_windows_attribution(
            report, windows_root_worktrees(WINDOWS_LONG_ROOT),
            windows_root_worktrees(WINDOWS_SHORT_ROOT), filename="pdfium64.dll"))
        result = self.assert_rejected()
        self.assertIn("pdfium runtime mismatch", result.stderr)

    def test_posix_pdfium_attribution_stays_case_sensitive(self):
        original = copy.deepcopy(self.reports["linux"])
        for kind, worktree in (
                ("different_case", "/Tmp/iris-parity-source-baseline"),
                ("short_name_alias", "/tmp/iris-parity-source-b~1")):
            with self.subTest(kind=kind):
                report = copy.deepcopy(original)
                report["baseline"]["worktree"] = worktree
                self.save("linux", report)
                result = self.assert_rejected()
                self.assertIn("PDFium path is not the staged worktree library",
                              result.stderr)

    def test_rejects_posix_library_name_in_windows_report(self):
        report = copy.deepcopy(self.reports["windows"])
        for side in ("baseline", "candidate"):
            runtime = report[side]["corpus"]["pdfium_runtime"]
            runtime["path"] = runtime["path"].replace("pdfium.dll", "libpdfium.so")
        self.save("windows", report)
        result = self.assert_rejected()
        self.assertIn("pdfium runtime mismatch", result.stderr)

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

    def test_accepts_zero_slowdown_under_monotone_drift(self):
        """Monotone runner drift cancels in opposite-order balanced sums."""
        baseline_medians = [100, 115, 120, 135, 140, 155, 160, 175]
        candidate_medians = [105, 110, 125, 130, 145, 150, 165, 170]
        baseline_deltas = deltas_for_round_medians(baseline_medians, 100)
        candidate_deltas = deltas_for_round_medians(candidate_medians, 100)
        report = make_report("linux", baseline_median=100, candidate_median=100,
                             baseline_deltas=baseline_deltas,
                             candidate_deltas=candidate_deltas)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["round_deltas_ns"], [5, -5, 5, -5, 5, -5, 5, -5])
        self.assertEqual(pair["balanced_delta_sums_ns"], [0, 0, 0, 0])
        self.save("linux", report)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_consistent_ten_percent_regression_with_noisy_rounds(self):
        """A consistent ~10% slowdown fails even with a noisy round or two."""
        candidate_medians = [110, 110, 110, 110, 110, 110, 108, 108]
        candidate_samples = [value for median in candidate_medians
                             for value in [median, median, median]]
        candidate_deltas = [value - 110 for value in candidate_samples]
        report = make_report("linux", baseline_median=100, candidate_median=110,
                             baseline_deltas=[0] * m.SAMPLES_PER_SIDE,
                             candidate_deltas=candidate_deltas)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["round_deltas_ns"], [10, 10, 10, 10, 10, 10, 8, 8])
        self.assertEqual(pair["balanced_delta_sums_ns"], [20, 20, 20, 16])
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("paired candidate slowdown", result.stderr)

    def test_pooled_median_alone_does_not_gate(self):
        """A large pooled median delta with small drift-free effect passes."""
        baseline_medians = [100] * m.ROUNDS
        candidate_medians = [130, 70, 130, 70, 130, 70, 130, 130]
        baseline_deltas = [0] * m.SAMPLES_PER_SIDE
        candidate_samples = [value for median in candidate_medians
                             for value in [median, median, median]]
        candidate_deltas = [value - 100 for value in candidate_samples]
        report = make_report("linux", baseline_median=100, candidate_median=100,
                             baseline_deltas=baseline_deltas,
                             candidate_deltas=candidate_deltas)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["candidate_minus_baseline_ns"], 30)
        self.assertEqual(pair["balanced_delta_sums_ns"], [0, 0, 0, 60])
        self.save("linux", report)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_accepts_curved_drift_residual_with_zero_variance(self):
        """Curved monotone drift leaves a 5ns residual; sub-2% noise passes."""
        baseline_medians = [1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000]
        candidate_medians = [1498, 1512, 3478, 3532, 5458, 5552, 7438, 7572]
        baseline_deltas = deltas_for_round_medians(baseline_medians, 1000)
        candidate_deltas = deltas_for_round_medians(candidate_medians, 1000)
        report = make_report("linux", baseline_median=1000, candidate_median=1000,
                             baseline_deltas=baseline_deltas,
                             candidate_deltas=candidate_deltas)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["round_deltas_ns"],
                         [498, -488, 478, -468, 458, -448, 438, -428])
        self.assertEqual(pair["balanced_delta_sums_ns"], [10, 10, 10, 10])
        self.save("linux", report)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_accepts_sub_two_percent_zero_variance_shift(self):
        """Uniform 100->101ns (1ns, 1%) has zero variance but passes as noise."""
        report = make_report("linux", baseline_median=100, candidate_median=101,
                             baseline_deltas=[0] * m.SAMPLES_PER_SIDE,
                             candidate_deltas=[0] * m.SAMPLES_PER_SIDE)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["round_deltas_ns"], [1] * m.ROUNDS)
        self.assertEqual(pair["balanced_delta_sums_ns"], [2] * 4)
        self.save("linux", report)
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_consistent_ten_percent_zero_variance_regression(self):
        """Uniform 100->110ns (10ns, 10%) exceeds the 2% floor and still fails."""
        report = make_report("linux", baseline_median=100, candidate_median=110,
                             baseline_deltas=[0] * m.SAMPLES_PER_SIDE,
                             candidate_deltas=[0] * m.SAMPLES_PER_SIDE)
        pair = report["paired_latency"][0]
        self.assertEqual(pair["round_deltas_ns"], [10] * m.ROUNDS)
        self.assertEqual(pair["balanced_delta_sums_ns"], [20] * 4)
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("paired candidate slowdown", result.stderr)

    def test_rejects_tampered_min_relative_effect_policy(self):
        """A policy with changed min_relative_effect is rejected."""
        raw = json.loads(POLICY.read_text(encoding="utf-8"))
        for value in (0.01, 0.03, 0.0, 0.0200001, "0.02", None, True):
            with self.subTest(value=value):
                changed = copy.deepcopy(raw)
                changed["min_relative_effect"] = value
                path = self.work / "policy.json"
                path.write_text(json.dumps(changed), encoding="utf-8")
                self.assert_rejected(policy=path)
        changed = copy.deepcopy(raw)
        del changed["min_relative_effect"]
        path = self.work / "policy.json"
        path.write_text(json.dumps(changed), encoding="utf-8")
        self.assert_rejected(policy=path)

    def test_samples_help_text_says_24(self):
        """The paired budget is 24 samples per side, not 21."""
        source = (ROOT / "scripts" / "parity_source_compare.py").read_text(
            encoding="utf-8")
        self.assertIn("fixed at 24 by the paired budget", source)
        self.assertNotIn("fixed at 21 by the paired budget", source)


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

    def test_rejects_a_matched_pair_with_the_wrong_product_build_profile(self):
        for platform in m.PLATFORMS:
            with self.subTest(platform=platform):
                wrong_level = "3" if platform == "linux" else "z"
                report = copy.deepcopy(self.reports[platform])
                for side in ("baseline", "candidate"):
                    for name in ("corpus", "j2k"):
                        section = report[side][name]
                        for record in [section["correctness"], *section["timings"]]:
                            record["identity"]["release_opt_level"] = wrong_level
                self.save(platform, report)
                result = self.assert_rejected()
                self.assertIn("wrong product build profile", result.stderr)

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

    def test_t_critical_value_matches_exact_df3_tail(self):
        critical = m.t_critical_df3(0.05, 18)
        self.assertAlmostEqual(critical, 7.184868893978111, places=9)
        self.assertLessEqual(m.student_t_survival_df3(critical), 0.05 / 18)
        self.assertGreater(m.student_t_survival_df3(critical - 1e-6), 0.05 / 18)
        # Closed form: S(t) = 1/2 - (atan(t/sqrt(3)) + t*sqrt(3)/(t*t+3))/pi.
        import math
        expected = 0.5 - (math.atan(critical / math.sqrt(3.0))
                          + critical * math.sqrt(3.0) / (critical * critical + 3.0)) / math.pi
        self.assertAlmostEqual(m.student_t_survival_df3(critical), expected, places=12)
        with self.assertRaises(ValueError):
            m.student_t_survival_df3(-1.0)
        with self.assertRaises(ValueError):
            m.t_critical_df3(0.05, 0)

    def test_rejects_wrong_round_count_missing_samples_and_tampered_statistics(self):
        original = copy.deepcopy(self.reports["linux"])
        for field, value in (("rounds", 2), ("samples_ns", None), ("round_medians_ns", None)):
            with self.subTest(field=field):
                report = copy.deepcopy(original)
                for side in ("baseline", "candidate"):
                    for name in ("corpus", "j2k"):
                        for item in report[side][name]["timings"]:
                            if value is None:
                                item["timing"].pop(field)
                            else:
                                item["timing"][field] = value
                self.save("linux", report)
                self.assert_rejected()
        report = copy.deepcopy(original)
        for side in ("baseline", "candidate"):
            for name in ("corpus", "j2k"):
                for item in report[side][name]["timings"]:
                    item["timing"]["samples_ns"] = item["timing"]["samples_ns"][:-1]
        self.save("linux", report)
        self.assert_rejected()

        report = copy.deepcopy(original)
        for side in ("baseline", "candidate"):
            for name in ("corpus", "j2k"):
                for item in report[side][name]["timings"]:
                    item["timing"]["p90"] += 1
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("recomputed p90 mismatch", result.stderr)

        report = copy.deepcopy(original)
        for side in ("baseline", "candidate"):
            for name in ("corpus", "j2k"):
                for item in report[side][name]["timings"]:
                    item["timing"]["round_medians_ns"][0] += 1
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("round median mismatch", result.stderr)

    def test_rejects_tampered_round_deltas_and_round_alternation(self):
        report = copy.deepcopy(self.reports["linux"])
        report["paired_latency"][0]["round_deltas_ns"][0] += 1
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("paired_latency was tampered with", result.stderr)

        report = copy.deepcopy(self.reports["linux"])
        report["paired_latency"][0]["balanced_delta_sums_ns"][0] += 1
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("paired_latency was tampered with", result.stderr)

        report = copy.deepcopy(self.reports["linux"])
        report["measurement_rounds"][1]["order"] = ["baseline", "candidate"]
        self.save("linux", report)
        result = self.assert_rejected()
        self.assertIn("measurement round alternation mismatch", result.stderr)

    def test_rejects_stale_sign_test_fields(self):
        report = copy.deepcopy(self.reports["linux"])
        report["paired_latency"][0]["paired_differences_ns"] = [0] * m.SAMPLES_PER_SIDE
        report["paired_latency"][0]["positive_differences"] = 0
        self.save("linux", report)
        result = self.assert_rejected()
        # Stale sign-test fields no longer exist in recomputed pairs,
        # so they fail as tampering before the gate is even reached.
        self.assertIn("tampered", result.stderr)
        # The gate itself also rejects stale fields when called directly.
        policy = json.loads(POLICY.read_text(encoding="utf-8"))
        clean = self.reports["linux"]
        maps = {"corpus": {
            "baseline": m.timing_map(clean["baseline"]["corpus"], "test baseline"),
            "candidate": m.timing_map(clean["candidate"]["corpus"], "test candidate")}}
        stale = copy.deepcopy(clean["paired_latency"][0])
        stale["paired_differences_ns"] = [0] * m.SAMPLES_PER_SIDE
        with self.assertRaises(ValueError) as ctx:
            m.check_pairs([stale], maps, "corpus", policy, "test")
        self.assertIn("stale sign-test fields", str(ctx.exception))

    def test_rejects_wrong_warmup_and_sample_count(self):
        for field, value in (("warmup_iterations", 1), ("sample_count", 23)):
            with self.subTest(field=field):
                report = copy.deepcopy(self.reports["linux"])
                for side in ("baseline", "candidate"):
                    for name in ("corpus", "j2k"):
                        for item in report[side][name]["timings"]:
                            item["timing"][field] = value
                self.save("linux", report)
                self.assert_rejected()

    def test_rejects_invalid_policy(self):
        raw = json.loads(POLICY.read_text(encoding="utf-8"))
        for field, value in (("rounds", 2), ("alpha", 0.1),
                             ("significance_family_size", 14),
                             ("t_critical_df3", 5.0),
                             ("policy_rationale", ""),
                             ("baseline_sha", OTHER_SHA), ("samples", 23)):
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


def baseline_fetch_step(sha=BASELINE_SHA, cat_file_guard=True):
    """Exact pinned-baseline fetch step required immediately after checkout."""
    step = (
        "      - name: Fetch pinned baseline commit for paired parity\n"
        "        shell: bash\n"
        "        run: |\n"
        "          git fetch origin refs/heads/main\n"
    )
    if cat_file_guard:
        step += f"          git cat-file -e {sha}^{{commit}}\n"
    return step


class WorkflowContractTests(unittest.TestCase):
    JOBS = ("parity-linux", "parity-macos", "parity-windows")
    CHECKOUT_STEP = (
        f"      - uses: {CHECKOUT}\n"
        "        with:\n"
        "          ref: ${{ inputs.source_sha }}\n"
        "          fetch-depth: 0\n"
        "          persist-credentials: false\n"
    )

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

    def missing_baseline_fetches(self, text):
        expected = self.CHECKOUT_STEP + baseline_fetch_step()
        return [job for job in self.JOBS
                if expected not in self.job_block(text, job)]

    def test_each_parity_job_fetches_pinned_baseline_after_checkout(self):
        self.assertIn(BASELINE_SHA, baseline_fetch_step())
        self.assertEqual(self.missing_baseline_fetches(self.evidence), [])
        self.assertEqual(self.evidence.count(baseline_fetch_step()), len(self.JOBS))

    def test_rejects_baseline_fetch_mutations_in_every_job(self):
        step = baseline_fetch_step()
        checkout = self.CHECKOUT_STEP
        setup = (f"      - uses: {SETUP_PYTHON}\n"
                 "        with:\n          python-version: '3.13'\n")
        mutations = {
            "wrong_sha": (step, step.replace(BASELINE_SHA, OTHER_SHA)),
            "missing_cat_file": (step, baseline_fetch_step(cat_file_guard=False)),
            "out_of_order": (checkout + step, checkout + setup + step),
        }
        for name, (original, replacement) in mutations.items():
            with self.subTest(mutation=name):
                mutated = self.evidence.replace(original, replacement)
                self.assertNotEqual(mutated, self.evidence)
                self.assertEqual(set(self.missing_baseline_fetches(mutated)),
                                 set(self.JOBS))

    def test_rejects_baseline_fetch_in_only_one_or_two_jobs(self):
        step = baseline_fetch_step()
        for removed in (1, 2):
            with self.subTest(removed=removed):
                mutated = self.evidence
                for job in self.JOBS[:removed]:
                    block = self.job_block(mutated, job)
                    self.assertIn(step, block)
                    mutated = mutated.replace(block, block.replace(step, ""), 1)
                self.assertEqual(set(self.missing_baseline_fetches(mutated)),
                                 set(self.JOBS[:removed]))

    def test_product_build_profile_is_shared_by_packaging_and_parity(self):
        profiles = m.load_product_profiles(
            ROOT / "scripts" / "product-build-profile.json")
        self.assertEqual(profiles, {
            "linux": {"build_profile": "release", "release_opt_level": "z"},
            "macos": {"build_profile": "release", "release_opt_level": "3"},
            "windows": {"build_profile": "release", "release_opt_level": "3"},
        })
        linux_package = (ROOT / "scripts" / "package-linux-appdir.sh").read_text(
            encoding="utf-8")
        macos_package = (ROOT / "scripts" / "package-macos-app.sh").read_text(
            encoding="utf-8")
        self.assertIn(
            'export CARGO_PROFILE_RELEASE_OPT_LEVEL="$(python3 '
            '"$script_dir/product_build_profile.py" --platform linux)"', linux_package)
        self.assertNotIn("CARGO_PROFILE_RELEASE_OPT_LEVEL=z", linux_package)
        self.assertIn(
            'export CARGO_PROFILE_RELEASE_OPT_LEVEL="$(python3 '
            '"$script_dir/product_build_profile.py" --platform macos)"', macos_package)
        windows_validation = (ROOT / ".github" / "workflows" /
                              "windows-ffmpeg-validation.yml").read_text(encoding="utf-8")
        self.assertIn(
            "$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = (& python3 "
            "scripts/product_build_profile.py --platform windows | Out-String).Trim()",
            windows_validation)
        ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("python3 scripts/test-release-parity.py", ci)
        for platform, level in (("linux", "z"), ("macos", "3"), ("windows", "3")):
            block = self.job_block(self.evidence, f"parity-{platform}")
            self.assertIn(
                f'python3 scripts/product_build_profile.py --platform {platform}', block)
            self.assertIn("CARGO_PROFILE_RELEASE_OPT_LEVEL", block)
            self.assertEqual(level, profiles[platform]["release_opt_level"])

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
            platform = job.removeprefix("parity-")
            if platform == "windows":
                # The Windows producer runs from pwsh so the MSVC developer PATH
                # is authoritative; Git bash would shadow MSVC link.exe. The
                # corpus fetch stays on bash, so only the producer step is
                # checked for PowerShell spelling.
                self.assertIn("        shell: pwsh\n        env:\n"
                              "          SOURCE_SHA: ${{ inputs.source_sha }}\n", block)
                marker = "      - name: Produce Windows paired-source parity report\n"
                self.assertIn(marker, block)
                step = block[block.index(marker):].split("\n      - ", 1)[0] + "\n"
                self.assertIn("        shell: pwsh\n", step)
                self.assertIn("$env:SOURCE_SHA", step)
                self.assertIn("$env:RUNNER_TEMP", step)
                self.assertIn("$env:IRIS_VALIDATION_PDFIUM_RUNTIME", step)
                self.assertNotIn('"$SOURCE_SHA"', step)
                self.assertNotIn('"$RUNNER_TEMP', step)
                self.assertNotIn('"$IRIS_VALIDATION_PDFIUM_RUNTIME', step)
                self.assertIn(
                    'python3 scripts/parity_source_compare.py --repo .', step)
                self.assertIn('--corpus "$env:RUNNER_TEMP/iris-preview-corpus"', step)
                self.assertIn(
                    '--pdfium-library "$env:IRIS_VALIDATION_PDFIUM_RUNTIME/pdfium.dll"',
                    step)
                self.assertIn('--output audit/parity/windows.json', step)
                self.assertIn('$ErrorActionPreference = "Stop"', step)
                self.assertIn('if ($LASTEXITCODE -ne 0)', step)
            else:
                self.assertIn("        shell: bash\n        env:\n"
                              "          SOURCE_SHA: ${{ inputs.source_sha }}\n", block)
                pdfium_flag = {
                    "linux": ' --pdfium-library "$RUNNER_TEMP/iris-pdfium-stage/usr/lib/iris/libpdfium.so"',
                }.get(platform, "")
                self.assertIn(
                    f'--corpus "$RUNNER_TEMP/iris-preview-corpus"{pdfium_flag} '
                    f'--output audit/parity/{platform}.json\n', block)
                self.assertIn('--candidate "$SOURCE_SHA"', block)
                self.assertIn(
                    f'        run: python3 scripts/parity_source_compare.py --repo . '
                    f'--candidate "$SOURCE_SHA" --corpus "$RUNNER_TEMP/iris-preview-corpus"'
                    f'{pdfium_flag} --output audit/parity/{platform}.json\n', block)
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
                self.assertIn(
                    '--pdfium-library "$RUNNER_TEMP/iris-pdfium-stage/usr/lib/iris/'
                    'libpdfium.so"', block)
                self.assertIn("shared-key: parity-linux-cargo", block)
            elif platform == "macos":
                self.assertIn("brew install pkg-config gnupg nasm cmake", block)
                self.assertNotIn("brew install ffmpeg", block)
                self.assertNotIn("--pdfium-library", block)
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
                self.assertIn(
                    '--pdfium-library "$env:IRIS_VALIDATION_PDFIUM_RUNTIME/pdfium.dll"', block)
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

    def test_macos_exports_pinned_pkg_config_before_prefix_assertion(self):
        macos = self.job_block(self.evidence, "parity-macos")
        marker = "      - name: Build pinned FFmpeg runtime for macOS parity smoke\n"
        self.assertIn(marker, macos)
        step = macos[macos.index(marker):].split("\n      - ", 1)[0] + "\n"
        prepare = '          python3 scripts/macos-ffmpeg.py prepare "$prefix"\n'
        exports = ('          export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"\n'
                   '          export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"\n')
        assertion = '          test "$(pkg-config --variable=prefix libavcodec)" = "$prefix"\n'
        self.assertIn(prepare, step)
        self.assertIn(exports, step)
        self.assertIn(assertion, step)
        self.assertLess(step.index(prepare), step.index(exports))
        self.assertLess(step.index(exports), step.index(assertion))
        self.assertIn('          } >> "$GITHUB_ENV"\n', step)
        for needle in ('echo "FFMPEG_DIR=$prefix"',
                       'echo "PKG_CONFIG_PATH=$prefix/lib/pkgconfig"',
                       'echo "PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig"',
                       'echo "DYLD_LIBRARY_PATH=$prefix/lib'):
            self.assertIn(needle, step)

    def test_windows_restores_pinned_vcpkg_root_after_msvc_setup(self):
        windows = self.job_block(self.evidence, "parity-windows")
        self.assertIn(
            "      IRIS_PARITY_VCPKG_ROOT: ${{ github.workspace }}/.vcpkg-validation\n",
            windows)
        self.assertIn(
            "      - name: Restore isolated vcpkg root after MSVC setup\n"
            "        shell: pwsh\n"
            "        run: |\n"
            '          $ErrorActionPreference = "Stop"\n'
            "          $root = $env:IRIS_PARITY_VCPKG_ROOT\n"
            '          if ([string]::IsNullOrWhiteSpace($root)) { throw "IRIS_PARITY_VCPKG_ROOT is not set" }\n'
            '          if (!(Test-Path -LiteralPath $root -PathType Container)) { throw "pinned vcpkg checkout missing: $root" }\n'
            '          "VCPKG_ROOT=$root" >> $env:GITHUB_ENV\n',
            windows)
        self.assertNotIn('"VCPKG_ROOT=$env:VCPKG_ROOT" >> $env:GITHUB_ENV', windows)

    def test_windows_producer_runs_from_pwsh_without_linker_workaround(self):
        windows = self.job_block(self.evidence, "parity-windows")
        # LINK supplies linker *arguments*, not the linker executable; rustc
        # still resolves link.exe through PATH, so no LINK/GITHUB_PATH export
        # can select MSVC over Git bash link.exe.
        self.assertNotIn("LINK=", windows)
        self.assertNotIn("Bind MSVC linker", windows)
        self.assertNotIn("VCToolsInstallDir", windows)
        setup = (
            "      - name: Set up MSVC developer environment (x64)\n"
            "        uses: ilammy/msvc-dev-cmd@0b201ec74fa43914dc39ae48a89fd1d8cb592756 # v1.13.0\n"
            "        with:\n"
            "          arch: x64\n"
        )
        marker = "      - name: Produce Windows paired-source parity report\n"
        self.assertIn(setup, windows)
        self.assertIn(marker, windows)
        self.assertLess(windows.index(setup), windows.index(marker))
        step = windows[windows.index(marker):].split("\n      - ", 1)[0] + "\n"
        self.assertIn("        shell: pwsh\n", step)
        self.assertNotIn("        shell: bash\n", step)
        for needle in ('$ErrorActionPreference = "Stop"',
                       "$env:SOURCE_SHA",
                       "$env:RUNNER_TEMP",
                       "$env:IRIS_VALIDATION_PDFIUM_RUNTIME",
                       'python3 scripts/parity_source_compare.py --repo .',
                       '--corpus "$env:RUNNER_TEMP/iris-preview-corpus"',
                       '--pdfium-library "$env:IRIS_VALIDATION_PDFIUM_RUNTIME/pdfium.dll"',
                       "--output audit/parity/windows.json",
                       "if ($LASTEXITCODE -ne 0)"):
            self.assertIn(needle, step, f"Windows producer misses {needle!r}")
        for stale in ('"$SOURCE_SHA"', '"$RUNNER_TEMP',
                      '"$IRIS_VALIDATION_PDFIUM_RUNTIME', '"LINK=$linker"'):
            self.assertNotIn(stale, step, f"Windows producer keeps bash/LINK spelling {stale!r}")
        self.assertNotIn("GITHUB_PATH", step)

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
            # The Windows producer runs from pwsh ($env:RUNNER_TEMP) while the
            # fetch stays on bash ($RUNNER_TEMP); both name the same job-local dir.
            norm = lambda value: value.replace("$env:", "$")
            for value in (fetch_dir, corpus_dir):
                self.assertLessEqual(len(value), 256, f"{job} corpus path is unbounded")
                self.assertNotIn("tests/preview-corpus", value, f"{job} uses manifest-only corpus")
            self.assertEqual(norm(fetch_dir), norm(corpus_dir),
                             f"{job} must pass its fetched bytes as --corpus, not a manifest")
            self.assertIn("RUNNER_TEMP", fetch_dir, f"{job} corpus must be job-local")
            self.assertIn("iris-preview-corpus", fetch_dir, f"{job} corpus must be a fresh dir")


class CiPerformanceEvidenceTests(unittest.TestCase):
    def test_scroll_benchmark_uses_shipped_linux_profile(self):
        ci = (Path(__file__).resolve().parents[1] / ".github" / "workflows"
              / "ci.yml").read_text(encoding="utf-8")
        start = ci.index("      - name: UI virtual scroll performance acceptance\n")
        perf = ci[start:].split("\n      - ", 1)[0]
        self.assertIn(
            "        run: |\n"
            "          export CARGO_PROFILE_RELEASE_OPT_LEVEL=\"$(python3 "
            "scripts/product_build_profile.py --platform linux)\"\n"
            "          cargo run --release -p iris-ui-shell --example "
            "virtual_scroll_perf --no-default-features --features minimal --locked\n",
            perf)
        self.assertNotIn("IRIS_MAX_LINUX_RELEASE_BYTES", perf)
        size_start = ci.index("      - name: Product binary size upper bound\n")
        size = ci[size_start:].split("\n      - ", 1)[0]
        self.assertIn("test \"$actual\" -le \"$IRIS_MAX_LINUX_RELEASE_BYTES\"", size)
        self.assertLess(size_start, start)
        product_start = ci.index("      - name: Product default release build (Ubuntu)\n")
        product = ci[product_start:].split("\n      - ", 1)[0]
        profile_line = ("export CARGO_PROFILE_RELEASE_OPT_LEVEL=\"$(python3 "
                        "scripts/product_build_profile.py --platform linux)\"")

        def profile_step(step):
            lines = [line.strip() for line in step.splitlines()
                     if "CARGO_PROFILE_RELEASE_OPT_LEVEL=" in line]
            self.assertEqual(len(lines), 1, "each Linux release step must set one profile")
            return lines[0]

        self.assertEqual(profile_step(product), profile_line)
        self.assertEqual(profile_step(perf), profile_line)

    def test_scroll_benchmark_excludes_unfinished_thumbnail_tasks(self):
        example = (Path(__file__).resolve().parents[1] / "crates" / "ui-shell"
                   / "examples" / "virtual_scroll_perf.rs").read_text(encoding="utf-8")
        self.assertIn('format!("asset-{index:06}.dat")', example)
        self.assertIn("drops every iced Task", example)


if __name__ == "__main__":
    unittest.main()
