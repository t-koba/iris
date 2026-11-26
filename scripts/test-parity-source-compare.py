#!/usr/bin/env python3
"""Offline unit tests for the report-only paired-source diagnostic."""

import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "parity_source_compare", ROOT / "scripts" / "parity_source_compare.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

PIXELS = "22" * 32


def identity(**changes):
    base = dict(
        source_revision="a" * 40, rustc="rustc 1.97.1", target_os="linux",
        target_arch="x86_64", runner_name="runner", runner_os="Linux",
        runner_arch="X64", runner_image_os="ubuntu24", runner_image_version="1",
        cpu_model="cpu", available_parallelism=4,
        build_profile="release", release_opt_level="3",
        features=dict(video=True, pdf=True, raw_full=False),
        ffmpeg_runtime=dict(codec_version=618100, format_version=618100,
                            util_version=591100),
        cargo_lock_sha256="11" * 32,
    )
    base.update(changes)
    return base


def output(width=128, height=128, pixels=PIXELS):
    return dict(format="jpeg", width=width, height=height, bytes=2113,
                source="FullDecode", decoded_rgb_sha256=pixels)


def timing(operation, median=100, filename="paired-j2k.jp2"):
    return dict(record="iris.preview_latency.v1", operation=operation, uses_ffmpeg=False,
                input=dict(filename=filename, sha256=m.FIXTURE_SHA256, size_bytes=m.FIXTURE_SIZE),
                output=output(), identity=identity(),
                timing=dict(unit="nanoseconds", warmup_iterations=1, sample_count=3,
                            min=90, median=median, p90=120, median_absolute_deviation=5, max=130))


def section(median=100, ident=None, pixels=PIXELS, filename="paired-j2k.jp2",
            pdfium_runtime=None):
    result = dict(
        correctness=dict(record="iris.preview_correctness.v1", passed=True,
                         fixtures=[dict(filename=filename, sha256=m.FIXTURE_SHA256,
                                        size_bytes=m.FIXTURE_SIZE, output=output(pixels=pixels))],
                         identity=ident or identity()),
        timings=[timing(m.J2K_OPERATION, median, filename)],
    )
    if pdfium_runtime is not None:
        result['pdfium_runtime'] = pdfium_runtime
    return result


def pdfium_runtime(path="/opt/pdfium/libpdfium.so", required=True):
    return dict(path=path, required=required)


def text(section):
    lines = [
        'PREVIEW_CORRECTNESS ' + json.dumps(section['correctness']),
        'PREVIEW_LATENCY ' + json.dumps(section['timings'][0]),
    ]
    if section.get('pdfium_runtime') is not None:
        lines.append('PREVIEW_PDFIUM_RUNTIME ' + json.dumps(section['pdfium_runtime']))
    return '\n'.join(lines) + '\n'


class ParitySourceCompareTests(unittest.TestCase):
    def test_parse_records_accepts_exact_set_and_rejects_bad_streams(self):
        parsed = m.parse_records(text(section()), {m.J2K_OPERATION}, 'synthetic')
        self.assertEqual(parsed['correctness']['record'], 'iris.preview_correctness.v1')
        self.assertEqual(parsed['timings'][0]['operation'], m.J2K_OPERATION)
        original = text(section())
        bad_streams = [
            original + 'PREVIEW_CORRECTNESS ' + json.dumps(section()['correctness']) + '\n',
            original.replace('iris.preview_latency.v1', 'iris.preview_latency.v2'),
            original.replace('"passed": true', '"passed": false'),
            original + 'PREVIEW_LATENCY {bad json}\n',
            original.replace(json.dumps(section()['timings'][0]),
                             json.dumps(timing('unexpected::operation'))),
        ]
        for stream in bad_streams:
            with self.subTest(stream=stream), self.assertRaises(ValueError):
                m.parse_records(stream, {m.J2K_OPERATION}, 'synthetic')

    def test_paired_records_require_matching_inputs_outputs_and_budgets(self):
        baseline, candidate = section(100), section(130)
        pairs = m.paired_records(baseline, candidate, 'J2K')
        self.assertEqual(len(pairs), 1)
        self.assertEqual(pairs[0]['baseline_median_ns'], 100)
        self.assertEqual(pairs[0]['candidate_median_ns'], 130)
        self.assertEqual(pairs[0]['candidate_minus_baseline_ns'], 30)
        self.assertEqual(pairs[0]['output_dimensions'], [128, 128])

        for mutate in (
            lambda value: value['correctness']['fixtures'][0]['output'].update(width=64),
            lambda value: value['correctness']['fixtures'][0]['output'].update(
                decoded_rgb_sha256="33" * 32),
            lambda value: value['correctness']['fixtures'][0].update(size_bytes=394),
            lambda value: value['timings'][0]['output'].update(decoded_rgb_sha256="33" * 32),
            lambda value: value['timings'][0]['input'].update(sha256="44" * 32),
            lambda value: value['timings'][0]['timing'].update(sample_count=4),
            lambda value: value['timings'][0].update(operation='other::operation'),
        ):
            changed = copy.deepcopy(candidate)
            mutate(changed)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                m.paired_records(baseline, changed, 'J2K')

        without_pixels = output()
        del without_pixels['decoded_rgb_sha256']
        left, right = section(), section()
        left['timings'][0]['output'] = copy.deepcopy(without_pixels)
        right['timings'][0]['output'] = copy.deepcopy(without_pixels)
        m.paired_records(left, right, 'J2K')
        right['timings'][0]['output']['decoded_rgb_sha256'] = PIXELS
        with self.assertRaises(ValueError):
            m.paired_records(left, right, 'J2K')

    def test_identity_pair_ignores_source_but_checks_runner_cpu_and_rustc(self):
        baseline = identity()
        candidate = identity(source_revision="b" * 40, cargo_lock_sha256="22" * 32)
        self.assertTrue(m.identity_pair(baseline, candidate))
        for change in ({'rustc': 'rustc 1.98'}, {'cpu_model': 'other cpu'},
                       {'runner_name': 'other runner'}, {'target_arch': 'aarch64'}):
            with self.subTest(change=change):
                self.assertFalse(m.identity_pair(baseline, identity(**change)))
        changed = section(ident=identity(rustc='rustc 0'))
        with self.assertRaises(ValueError):
            m.paired_records(section(), changed, 'J2K')

    def test_parse_records_validates_single_pdfium_runtime_marker(self):
        base = text(section())
        good = base + 'PREVIEW_PDFIUM_RUNTIME ' + json.dumps(pdfium_runtime()) + '\n'
        parsed = m.parse_records(good, {m.J2K_OPERATION}, 'synthetic')
        self.assertEqual(parsed['pdfium_runtime'], pdfium_runtime())
        self.assertIsNone(m.parse_records(base, {m.J2K_OPERATION}, 'synthetic')['pdfium_runtime'])

        def stream(*payloads):
            return base + ''.join('PREVIEW_PDFIUM_RUNTIME ' + payload + '\n'
                                  for payload in payloads)

        marker = json.dumps(pdfium_runtime())
        bad_streams = [
            stream(marker, marker),
            stream('{bad json}'),
            stream(json.dumps({"path": 1, "required": True})),
            stream(json.dumps({"path": "/opt/pdfium/libpdfium.so", "required": "true"})),
            stream(json.dumps({"path": "/opt/pdfium/libpdfium.so", "required": 1})),
            stream(json.dumps({"path": "/opt/pdfium/libpdfium.so"})),
            stream(json.dumps({"path": "/opt/pdfium/libpdfium.so", "required": True,
                               "extra": 1})),
            stream(json.dumps(["/opt/pdfium/libpdfium.so", True])),
        ]
        for payload in bad_streams:
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                m.parse_records(payload, {m.J2K_OPERATION}, 'synthetic')

    def test_paired_records_rejects_build_and_runtime_drift(self):
        baseline = section(pdfium_runtime=pdfium_runtime())
        m.paired_records(baseline, section(pdfium_runtime=pdfium_runtime()), 'J2K')
        for change in ({'build_profile': 'debug'}, {'release_opt_level': 'z'},
                       {'features': dict(video=False, pdf=True, raw_full=False)}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(**change),
                                                   pdfium_runtime=pdfium_runtime()), 'J2K')
        for runtime in (pdfium_runtime(path='/other/libpdfium.so'),
                        pdfium_runtime(required=False), None):
            with self.subTest(runtime=runtime), self.assertRaises(ValueError):
                m.paired_records(baseline, section(pdfium_runtime=runtime), 'J2K')
        with self.assertRaises(ValueError):
            m.paired_records(section(), baseline, 'J2K')

    def test_paired_records_rejects_ffmpeg_runtime_drift(self):
        baseline = section()
        m.paired_records(baseline, section(), 'J2K')
        bumped = dict(codec_version=618101, format_version=618100,
                      util_version=591100)
        with self.subTest(change='codec_version bump'):
            with self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(ffmpeg_runtime=bumped)),
                                 'J2K')
            drifted = section()
            drifted['timings'][0]['identity'] = identity(ffmpeg_runtime=bumped)
            with self.assertRaises(ValueError):
                m.paired_records(baseline, drifted, 'J2K')
        for runtime in (dict(codec_version=618100, format_version=618200,
                             util_version=591100),
                        dict(codec_version=618100, format_version=618100,
                             util_version=591200),
                        None):
            with self.subTest(runtime=runtime), self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(ffmpeg_runtime=runtime)),
                                 'J2K')

    def test_paired_records_rejects_uses_ffmpeg_drift(self):
        baseline = section()
        for changed in (True, None, "false", 1):
            candidate = section()
            candidate['timings'][0]['uses_ffmpeg'] = changed
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                m.paired_records(baseline, candidate, 'J2K')
        ffmpeg = section()
        ffmpeg['timings'][0]['uses_ffmpeg'] = True
        with self.assertRaises(ValueError):
            m.paired_records(baseline, ffmpeg, 'J2K')
        both_ffmpeg = section()
        both_ffmpeg['timings'][0]['uses_ffmpeg'] = True
        m.paired_records(ffmpeg, both_ffmpeg, 'J2K')

    def test_install_example_skips_same_file_and_stages_other_checkout(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            candidate = root / 'candidate'
            baseline = root / 'baseline'
            generator = root / 'repo'
            for checkout in (candidate, baseline):
                (checkout / m.EXAMPLES).mkdir(parents=True)
            harness = candidate / m.EXAMPLES / m.HARNESS
            harness.write_text('shared harness\n', encoding='utf-8')
            source = generator / m.EXAMPLES / m.GENERATOR
            source.parent.mkdir(parents=True)
            source.write_text('generator\n', encoding='utf-8')
            m.install_example(harness, candidate, m.HARNESS)
            m.install_example(source, candidate, m.GENERATOR)
            m.install_example(harness, baseline, m.HARNESS)
            m.install_example(source, baseline, m.GENERATOR)
            self.assertEqual((baseline / m.EXAMPLES / m.HARNESS).read_text(),
                             harness.read_text())
            self.assertEqual((candidate / m.EXAMPLES / m.GENERATOR).read_text(),
                             source.read_text())
            self.assertEqual((baseline / m.EXAMPLES / m.GENERATOR).read_text(),
                             source.read_text())
            with self.assertRaises(ValueError):
                m.install_example(root / 'missing.rs', baseline, 'missing.rs')

    def test_report_cap_and_claim(self):
        report = dict(schema=m.PARITY_CLAIM, pin=m.FIXTURE_SHA256, size=m.FIXTURE_SIZE)
        data = m.report_bytes(report)
        self.assertEqual(json.loads(data), report)
        self.assertEqual(m.PARITY_CLAIM, 'none; report-only diagnostic, release hold unchanged')
        original = m.MAX_REPORT_BYTES
        self.addCleanup(setattr, m, 'MAX_REPORT_BYTES', original)
        m.MAX_REPORT_BYTES = len(data.encode()) - 1
        with self.assertRaises(ValueError):
            m.report_bytes(report)


if __name__ == '__main__':
    unittest.main()
