#!/usr/bin/env python3
"""Offline unit tests for the report-only paired-source diagnostic."""

import copy
import importlib.util
import json
from pathlib import Path
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


def section(median=100, ident=None, pixels=PIXELS, filename="paired-j2k.jp2"):
    return dict(
        correctness=dict(record="iris.preview_correctness.v1", passed=True,
                         fixtures=[dict(filename=filename, sha256=m.FIXTURE_SHA256,
                                        size_bytes=m.FIXTURE_SIZE, output=output(pixels=pixels))],
                         identity=ident or identity()),
        timings=[timing(m.J2K_OPERATION, median, filename)],
    )


def text(section):
    return '\n'.join([
        'PREVIEW_CORRECTNESS ' + json.dumps(section['correctness']),
        'PREVIEW_LATENCY ' + json.dumps(section['timings'][0]),
    ]) + '\n'


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
        candidate = identity(source_revision="b" * 40)
        self.assertTrue(m.identity_pair(baseline, candidate))
        for change in ({'rustc': 'rustc 1.98'}, {'cpu_model': 'other cpu'},
                       {'runner_name': 'other runner'}, {'target_arch': 'aarch64'}):
            with self.subTest(change=change):
                self.assertFalse(m.identity_pair(baseline, identity(**change)))
        changed = section(ident=identity(rustc='rustc 0'))
        with self.assertRaises(ValueError):
            m.paired_records(section(), changed, 'J2K')

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
