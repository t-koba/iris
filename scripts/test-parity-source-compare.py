#!/usr/bin/env python3
"""Offline unit tests for the report-only paired-source diagnostic."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "parity_source_compare", ROOT / "scripts" / "parity_source_compare.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

PIXELS = "22" * 32
PINNED_FFMPEG = m.pinned_ffmpeg_identity()


def ffmpeg_runtime(**changes):
    base = dict(codec_version=PINNED_FFMPEG['codec_version'],
                format_version=618100, util_version=591100,
                configuration=PINNED_FFMPEG['configuration'],
                license=PINNED_FFMPEG['license'])
    base.update(changes)
    return base


def identity(**changes):
    base = dict(
        identity_schema=m.IDENTITY_SCHEMA,
        source_revision="a" * 40, rustc="rustc 1.97.1", target_os="linux",
        target_arch="x86_64", runner_name="runner", runner_os="Linux",
        runner_arch="X64", runner_image_os="ubuntu24", runner_image_version="1",
        cpu_model="cpu", available_parallelism=4,
        build_profile="release", release_opt_level="3",
        features=dict(video=True, pdf=True, raw_full=False),
        ffmpeg_runtime=ffmpeg_runtime(),
        cargo_lock_sha256="11" * 32,
    )
    base.update(changes)
    return base


def legacy_section(median=100, pixels=PIXELS, filename="paired-j2k.jp2",
                   pdfium_runtime=None):
    """Baseline section lacking the v2 identity schema and FFmpeg runtime."""
    result = section(median, None, pixels, filename, pdfium_runtime)
    for record in [result["correctness"]] + result["timings"]:
        record["identity"].pop("identity_schema", None)
        record["identity"].pop("ffmpeg_runtime", None)
    return result


def output(width=128, height=128, pixels=PIXELS):
    return dict(format="jpeg", width=width, height=height, bytes=2113,
                source="FullDecode", decoded_rgb_sha256=pixels)


def timing(operation, median=100, filename="paired-j2k.jp2", samples_ns=None):
    samples_ns = [median] * m.SAMPLES_PER_SIDE if samples_ns is None else list(samples_ns)
    base = dict(unit="nanoseconds", warmup_iterations=m.WARMUPS_PER_BLOCK,
                sample_count=len(samples_ns), samples_ns=list(samples_ns),
                **m.timing_statistics(samples_ns, 'test'))
    if len(samples_ns) == m.SAMPLES_PER_SIDE:
        base["rounds"] = m.ROUNDS
        base["round_medians_ns"] = list(m.round_block_medians(samples_ns, 'test'))
    # Short vectors mimic raw single-block harness output (no pooled medians);
    # the pairing layer fails closed downstream instead of inside this fixture.
    return dict(record="iris.preview_latency.v1", operation=operation, uses_ffmpeg=False,
                input=dict(filename=filename, sha256=m.FIXTURE_SHA256, size_bytes=m.FIXTURE_SIZE),
                output=output(), identity=identity(),
                timing=base)


def section(median=100, ident=None, pixels=PIXELS, filename="paired-j2k.jp2",
            pdfium_runtime=None, samples_ns=None):
    result = dict(
        correctness=dict(record="iris.preview_correctness.v1", passed=True,
                         fixtures=[dict(filename=filename, sha256=m.FIXTURE_SHA256,
                                        size_bytes=m.FIXTURE_SIZE, output=output(pixels=pixels))],
                         identity=ident or identity()),
        timings=[timing(m.J2K_OPERATION, median, filename, samples_ns)],
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
    def test_worktrees_are_created_without_eol_conversion(self):
        source = (ROOT / "scripts" / "parity_source_compare.py").read_text()
        self.assertIn("'-c', 'core.autocrlf=false', '-C', repo, 'worktree'", source)

        # Prove the flag actually keeps blob bytes identical when a runner has
        # implicit CRLF conversion enabled and the tree has no eol attribute.
        with tempfile.TemporaryDirectory(prefix="iris-parity-eol-") as temporary:
            repo = Path(temporary) / "repo"
            repo.mkdir()
            def git(*args):
                return subprocess.run(["git", "-C", str(repo), *args],
                                      capture_output=True, text=True, check=True)
            git("init", "-q")
            git("config", "core.autocrlf", "true")
            payload = repo / "sample.txt"
            payload.write_bytes(b"line one\nline two\n")
            git("-c", "user.email=test@example.invalid", "-c", "user.name=test",
                "add", "sample.txt")
            git("-c", "user.email=test@example.invalid", "-c", "user.name=test",
                "commit", "-qm", "fixture")
            sha = git("rev-parse", "HEAD").stdout.strip()
            converted = Path(temporary) / "converted"
            faithful = Path(temporary) / "faithful"
            git("worktree", "add", "-q", "--detach", str(converted), sha)
            subprocess.run(["git", "-c", "core.autocrlf=false", "-C", str(repo),
                            "worktree", "add", "-q", "--detach", str(faithful), sha],
                           capture_output=True, text=True, check=True)
            self.assertEqual((converted / "sample.txt").read_bytes(),
                             b"line one\r\nline two\r\n")
            self.assertEqual((faithful / "sample.txt").read_bytes(),
                             b"line one\nline two\n")

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
        self.assertEqual(pairs[0]['round_deltas_ns'], [30] * m.ROUNDS)
        self.assertEqual(pairs[0]['balanced_delta_sums_ns'], [60] * 4)
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

    def test_paired_records_reports_balanced_round_deltas(self):
        baseline_medians = [100] * m.ROUNDS
        candidate_medians = [105, 95, 105, 95, 105, 95, 105, 95]
        baseline_samples = [value for median in baseline_medians for value in [median - 1, median, median + 1]]
        candidate_samples = [value for median in candidate_medians for value in [median - 1, median, median + 1]]
        baseline = section(100, samples_ns=baseline_samples)
        candidate = section(100, samples_ns=candidate_samples)
        pair = m.paired_records(baseline, candidate, 'J2K')[0]
        self.assertEqual(pair['round_deltas_ns'], [5, -5, 5, -5, 5, -5, 5, -5])
        self.assertEqual(pair['balanced_delta_sums_ns'], [0, 0, 0, 0])
        # Pooled medians can diverge from the drift-free mean; the pooled delta
        # is reported only and must not decide the gate on its own.
        self.assertEqual(pair['candidate_minus_baseline_ns'],
                         candidate['timings'][0]['timing']['median']
                         - baseline['timings'][0]['timing']['median'])
        with self.assertRaises(ValueError):
            m.paired_records(baseline, section(105, samples_ns=candidate_samples[:-1]), 'J2K')

    def test_parse_records_rejects_missing_or_short_sample_arrays(self):
        good = text(section())
        without = copy.deepcopy(section())
        without['timings'][0]['timing'].pop('samples_ns')
        short = copy.deepcopy(section())
        short['timings'][0]['timing']['samples_ns'] = short['timings'][0]['timing'][
            'samples_ns'][:-1]
        for stream in (text(without), text(short)):
            with self.subTest(stream=stream), self.assertRaises(ValueError):
                m.parse_records(stream, {m.J2K_OPERATION}, 'synthetic')
        self.assertEqual(m.parse_records(good, {m.J2K_OPERATION}, 'synthetic')
                         ['timings'][0]['timing']['sample_count'], m.SAMPLES_PER_SIDE)

    def test_round_plan_alternates_sides_with_three_samples_per_block(self):
        self.assertEqual(m.ROUND_ORDER,
                         (('baseline', 'candidate'), ('candidate', 'baseline'),
                          ('baseline', 'candidate'), ('candidate', 'baseline'),
                          ('baseline', 'candidate'), ('candidate', 'baseline'),
                          ('baseline', 'candidate'), ('candidate', 'baseline')))
        self.assertEqual(m.WARMUPS_PER_BLOCK, 2)
        self.assertEqual(m.BLOCK_SAMPLES, 3)
        self.assertEqual(m.ROUNDS, 8)
        self.assertEqual(m.SAMPLES_PER_SIDE, 24)
        self.assertEqual(m.BLOCK_SAMPLES * len(m.ROUND_ORDER), m.SAMPLES_PER_SIDE)
        self.assertEqual(m.measurement_rounds(), [
            dict(round=number, order=list(order))
            for number, order in enumerate(m.ROUND_ORDER, 1)
        ])
        for first, second in zip(m.ROUND_ORDER, m.ROUND_ORDER[1:]):
            self.assertNotEqual(list(first), list(second))

    def test_measure_round_runs_full_smokes_in_declared_order(self):
        checkouts = dict(baseline=Path('/baseline'), candidate=Path('/candidate'))
        shas = dict(baseline='a' * 40, candidate='b' * 40)
        libraries = dict(baseline=None, candidate=None)
        with mock.patch.object(m, 'smoke',
                               side_effect=lambda checkout, example, argument, env, label:
                               dict(checkout=str(checkout), example=example,
                                    argument=str(argument), env=env, label=label)) as smoke:
            collected = m.measure_round(2, m.ROUND_ORDER[1], checkouts, shas, libraries,
                                        Path('/corpus'), Path('/fixture'), 2, 3)
        self.assertEqual(list(collected), ['candidate', 'baseline'])
        labels = [call.args[4] for call in smoke.call_args_list]
        self.assertEqual(labels, [
            'round 2 candidate corpus smoke', 'round 2 candidate J2K smoke',
            'round 2 baseline corpus smoke', 'round 2 baseline J2K smoke',
        ])
        for call in smoke.call_args_list:
            self.assertEqual(call.args[3]['IRIS_PREVIEW_LATENCY_SAMPLES'], '3')
            self.assertEqual(call.args[3]['IRIS_PREVIEW_LATENCY_WARMUPS'], '2')
            side = 'candidate' if 'candidate' in call.args[4] else 'baseline'
            self.assertEqual(call.args[3]['GITHUB_SHA'], shas[side])
            expected_argument = Path('/corpus' if 'corpus' in call.args[4] else '/fixture')
            self.assertEqual(call.args[2], expected_argument)
        self.assertEqual(collected['candidate']['corpus']['example'], m.CORPUS_HARNESS)
        self.assertEqual(collected['baseline']['j2k']['example'], m.HARNESS)

    def test_pool_round_sections_keeps_chronology_and_recomputes_statistics(self):
        blocks = [[index * 10 + offset for offset in (1, 2, 3)] for index in range(m.ROUNDS)]
        rounds = [dict(baseline=dict(corpus=section(samples_ns=block)))
                  for block in blocks]
        pooled = m.pool_round_sections(rounds, 'baseline', 'corpus',
                                       {m.J2K_OPERATION}, 'test')
        samples = [sample for block in blocks for sample in block]
        timing = pooled['timings'][0]['timing']
        self.assertEqual(timing['samples_ns'], samples)
        self.assertEqual(timing['sample_count'], m.SAMPLES_PER_SIDE)
        self.assertEqual(timing['rounds'], m.ROUNDS)
        self.assertEqual(timing['round_medians_ns'],
                         [sorted(block)[1] for block in blocks])
        self.assertEqual(timing['round_medians_ns'],
                         m.round_block_medians(samples, 'test'))
        for key, value in m.timing_statistics(samples, 'test').items():
            self.assertEqual(timing[key], value)
        self.assertEqual(pooled['correctness'], rounds[0]['baseline']['corpus']['correctness'])

        changed = copy.deepcopy(rounds)
        changed[1]['baseline']['corpus']['timings'][0]['timing']['samples_ns'] = blocks[1][:-1]
        with self.assertRaises(ValueError):
            m.pool_round_sections(changed, 'baseline', 'corpus', {m.J2K_OPERATION}, 'test')

    def test_pool_round_sections_records_eight_block_medians(self):
        medians = [100 + index * 7 for index in range(m.ROUNDS)]
        blocks = [[median - 1, median, median + 1] for median in medians]
        rounds = [dict(baseline=dict(corpus=section(samples_ns=block)))
                  for block in blocks]
        pooled = m.pool_round_sections(rounds, 'baseline', 'corpus',
                                       {m.J2K_OPERATION}, 'test')
        timing = pooled['timings'][0]['timing']
        self.assertEqual(timing['round_medians_ns'], medians)
        baseline = pooled
        candidate_blocks = [[median + 10 - 1, median + 10, median + 10 + 1]
                            for median in medians]
        candidate_rounds = [dict(baseline=dict(corpus=section(samples_ns=block)))
                            for block in candidate_blocks]
        candidate = m.pool_round_sections(candidate_rounds, 'baseline', 'corpus',
                                          {m.J2K_OPERATION}, 'test')
        pair = m.paired_records(baseline, candidate, 'J2K')[0]
        self.assertEqual(pair['round_deltas_ns'], [10] * m.ROUNDS)
        self.assertEqual(pair['balanced_delta_sums_ns'], [20] * 4)

    def test_paired_records_rejects_tampered_round_medians_and_deltas(self):
        baseline, candidate = section(100), section(110)
        pair = m.paired_records(baseline, candidate, 'J2K')[0]
        self.assertEqual(pair['round_deltas_ns'], [10] * m.ROUNDS)
        tampered = copy.deepcopy(candidate)
        tampered['timings'][0]['timing']['round_medians_ns'][0] += 1
        with self.assertRaises(ValueError):
            m.paired_records(baseline, tampered, 'J2K')
        tampered_pair = copy.deepcopy(pair)
        tampered_pair['round_deltas_ns'] = [11] + tampered_pair['round_deltas_ns'][1:]
        tampered_pair['balanced_delta_sums_ns'] = [
            tampered_pair['round_deltas_ns'][index]
            + tampered_pair['round_deltas_ns'][index + 1]
            for index in (0, 2, 4, 6)]
        # Producer pairs are recomputed by the verifier; a stale pooled delta
        # alone never decides the gate but tampered deltas fail closed.
        self.assertNotEqual(tampered_pair['round_deltas_ns'], pair['round_deltas_ns'])

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

    def test_paired_records_reject_release_opt_level_drift(self):
        baseline = section(ident=identity(release_opt_level="3"))
        for level in ("2", "z", None):
            with self.subTest(level=level, record="correctness"):
                with self.assertRaises(ValueError):
                    m.paired_records(baseline, section(ident=identity(release_opt_level=level)),
                                     "J2K")
            drifted = section()
            drifted["timings"][0]["identity"]["release_opt_level"] = level
            with self.subTest(level=level, record="timing"), self.assertRaises(ValueError):
                m.paired_records(baseline, drifted, "J2K")
        self.assertFalse(m.identity_pair(identity(), identity(release_opt_level="z")))
        reports = dict(
            baseline=dict(corpus=baseline, j2k=baseline),
            candidate=dict(corpus=section(ident=identity(release_opt_level="2")), j2k=section()),
        )
        with self.assertRaises(ValueError):
            m.pairing_plan(reports["baseline"], reports["candidate"])

    def test_staged_pdfium_library_binds_the_required_smoke_path(self):
        name = m.pdfium_library_name()
        with tempfile.TemporaryDirectory(prefix='iris-pdfium-staging-') as temporary:
            root = Path(temporary)
            source = root / 'verified' / name
            source.parent.mkdir()
            source.write_bytes(b'verified pdfium bytes')
            checkout = root / 'worktree'
            checkout.mkdir()
            staged = m.stage_pdfium_library(source, checkout, 'synthetic')
            self.assertEqual(staged, (checkout / name).resolve())
            self.assertEqual(staged.read_bytes(), source.read_bytes())
            env = m.cargo_env('a' * 40, 2, 21, staged)
            self.assertEqual(env[m.PDFIUM_REQUIRE_ENV], str(staged.resolve()))
            with self.assertRaises(ValueError):
                m.stage_pdfium_library(root / 'absent' / name, checkout, 'synthetic')

    def test_paired_records_rejects_build_and_runtime_drift(self):
        name = m.pdfium_library_name()
        baseline = section(pdfium_runtime=pdfium_runtime(path=f'/baseline/{name}'))
        other_worktree = section(pdfium_runtime=pdfium_runtime(path=f'/candidate/{name}'))
        m.paired_records(baseline, other_worktree, 'J2K')
        for change in ({'build_profile': 'debug'}, {'release_opt_level': 'z'},
                       {'features': dict(video=False, pdf=True, raw_full=False)}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(**change),
                                                   pdfium_runtime=pdfium_runtime(
                                                       path=f'/candidate/{name}')), 'J2K')
        wrong_name = 'wrong-library.so'
        for runtime in (pdfium_runtime(path=f'/candidate/{wrong_name}'),
                        pdfium_runtime(path=f'/candidate/{name}', required=False), None):
            with self.subTest(runtime=runtime), self.assertRaises(ValueError):
                m.paired_records(baseline, section(pdfium_runtime=runtime), 'J2K')
        with self.assertRaises(ValueError):
            m.paired_records(section(), baseline, 'J2K')

    def test_pdfium_runtime_pair_normalizes_windows_library_paths(self):
        baseline = pdfium_runtime(path=r'C:\a\baseline\pdfium.dll')
        candidate = pdfium_runtime(path=r'D:\a\candidate\PDFIUM.DLL')
        m.pdfium_runtime_pair(baseline, candidate, 'windows')
        for path in (r'D:\a\candidate\wrong.dll',
                     r'D:\a\candidate\libpdfium.so',
                     r'/candidate/pdfium.dll'):
            with self.subTest(path=path), self.assertRaises(ValueError):
                m.pdfium_runtime_pair(baseline, pdfium_runtime(path=path), 'windows')
        with self.assertRaises(ValueError):
            m.pdfium_runtime_pair(
                baseline,
                pdfium_runtime(path=r'D:\a\candidate\pdfium.dll', required=False),
                'windows')

    def test_paired_records_rejects_ffmpeg_runtime_drift(self):
        baseline = section()
        m.paired_records(baseline, section(), 'J2K')
        bumped = ffmpeg_runtime(codec_version=PINNED_FFMPEG['codec_version'] + 1)
        with self.subTest(change='codec_version bump'):
            with self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(ffmpeg_runtime=bumped)),
                                 'J2K')
            drifted = section()
            drifted['timings'][0]['identity'] = identity(ffmpeg_runtime=bumped)
            with self.assertRaises(ValueError):
                m.paired_records(baseline, drifted, 'J2K')
        for runtime in (ffmpeg_runtime(format_version=618200),
                        ffmpeg_runtime(util_version=591200),
                        ffmpeg_runtime(configuration='--enable-shared --enable-gpl'),
                        ffmpeg_runtime(license='GPL version 3 or later'),
                        None):
            with self.subTest(runtime=runtime), self.assertRaises(ValueError):
                m.paired_records(baseline, section(ident=identity(ffmpeg_runtime=runtime)),
                                 'J2K')

    def test_pinned_ffmpeg_identity_is_derived_from_the_packaging_pin(self):
        pin = m.pinned_ffmpeg_identity()
        self.assertEqual(pin['codec_version'], (62 << 16) | (28 << 8) | 103)
        self.assertEqual(pin['license'], m.PINNED_FFMPEG_LICENSE)
        self.assertEqual(pin['configuration'], ' '.join(
            json.loads((ROOT / m.FFMPEG_POLICY).read_text())['configure_flags']))
        self.assertEqual(m.ffmpeg_configuration(
            '--prefix=/tmp/other ' + pin['configuration']), pin['configuration'])

    def test_corpus_smoke_records_configuration_and_license(self):
        source = (ROOT / m.EXAMPLES / m.CORPUS_HARNESS).read_text(encoding='utf-8')
        self.assertIn('"configuration": ffmpeg_next::codec::configuration()', source)
        self.assertIn('"license": ffmpeg_next::codec::license()', source)
        self.assertEqual(m.IDENTITY_SCHEMA, 'iris.preview_identity.v2')

    def test_stage_pdfium_docstring_does_not_claim_runtime_provenance(self):
        """Staging declares the candidate gate, not provenance for both sides."""
        source = (ROOT / 'scripts' / 'parity_source_compare.py').read_text(encoding='utf-8')
        start = source.index('def stage_pdfium_library(')
        doc = source[start:source.index('    source = Path(source).resolve()', start)]
        self.assertNotIn('identifies the staged bytes', doc)
        self.assertNotIn('rather than a system search-path library', doc)
        self.assertIn("The candidate's required-library gate", doc)
        self.assertIn('module actually\n    loaded by each process', doc)
        self.assertIn('not by itself provenance', doc)

    def test_shared_corpus_harness_reports_loaded_pdfium_module(self):
        """The staged harness must attribute the loaded module, not the env value."""
        source = (ROOT / m.EXAMPLES / m.CORPUS_HARNESS).read_text(encoding='utf-8')
        # The pinned baseline has no required-library gate and may bind a system
        # PDFium while the staged file exists; printing the env value would hide
        # that fallback, so the helper must query the loader instead.
        self.assertNotIn('loaded_pdfium_library', source)
        self.assertNotIn('is_file()', source)
        self.assertNotIn('canonicalize', source)
        self.assertIn('IRIS_PDFIUM_REQUIRE_LIBRARY', source)
        start = source.index('fn print_pdfium_runtime()')
        helper = source[start:source.index('\n}\n', start) + 2]
        self.assertIn('IRIS_PDFIUM_REQUIRE_LIBRARY', helper)
        self.assertIn('loaded_pdfium_path', helper)
        self.assertIn('var_os("IRIS_PDFIUM_REQUIRE_LIBRARY")', helper)
        self.assertNotIn('std::env::var("IRIS_PDFIUM_REQUIRE_LIBRARY")', helper)
        # Per-OS loader queries: PDFium is opened RTLD_LOCAL, so dynamic symbol
        # lookup cannot see it; the same source must compile on Linux, macOS and
        # Windows with no new Cargo dependencies.
        for forbidden in ('dlsym', 'dladdr', 'FPDF_InitLibrary', 'RTLD_DEFAULT',
                          'link(name = "dl")'):
            self.assertNotIn(forbidden, source,
                             f'harness still relies on {forbidden!r}')
        for needle in ('"/proc/self/maps"', 'GetModuleHandleW',
                       'GetModuleFileNameW', 'pdfium.dll'):
            self.assertIn(needle, source, f'harness misses loader proof {needle!r}')
        for dyld in ('_dyld_get_image_count', '_dyld_get_image_name',
                     'extern "C"'):
            self.assertNotIn(dyld, source,
                             f'harness uses dyld image list {dyld!r}')
        helpers = rust_function_bodies(
            source, 'fn loaded_pdfium_path() -> Option<String> {')
        self.assertEqual(len(helpers), 4)  # Linux, macOS, Windows and fallback
        for body in helpers:
            self.assertNotIn('env', body,
                             'the path helper must not consult the environment')
        self.assertIn('file_name()', helpers[0])
        self.assertIn('starts_with("libpdfium.so.")', helpers[0])
        self.assertIn('"/proc/self/maps"', helpers[0])
        # macOS uses CoreGraphics: no PDFium is loaded, so no record is emitted.
        self.assertIn('None', helpers[1])
        self.assertNotIn('file_name', helpers[1])
        self.assertNotIn('dyld', helpers[1])
        self.assertIn('GetModuleFileNameW', helpers[2])
        # The single optional record keeps its shape; required stays true only
        # for the opt-in gate and nothing prints without a loaded module.
        self.assertIn('"required": true', helper)
        self.assertIn('PREVIEW_PDFIUM_RUNTIME', helper)

    def test_linux_pdfium_path_matches_map_file_names_not_the_gate_value(self):
        """Exercise the real Linux helper against controlled process mappings."""
        source = (ROOT / m.EXAMPLES / m.CORPUS_HARNESS).read_text(encoding='utf-8')
        helpers = rust_function_bodies(
            source, 'fn loaded_pdfium_path() -> Option<String> {')
        maps_helpers = [body for body in helpers if '"/proc/self/maps"' in body]
        self.assertEqual(len(maps_helpers), 1, 'helper must read /proc/self/maps')
        linux = maps_helpers[0]
        rustc = shutil.which('rustc')
        self.assertIsNotNone(rustc, 'rustc is required for the PDFium path test')
        with tempfile.TemporaryDirectory(prefix='iris-pdfium-path-') as temporary:
            root = Path(temporary)
            maps = root / 'maps'
            literal = '"/proc/self/maps"'
            patched = linux.replace(literal, json.dumps(str(maps)))
            self.assertNotEqual(patched, linux, 'helper lost its maps source')
            (root / 'main.rs').write_text(
                'use std::{fs, path::Path};\n\n' + patched + '\n'
                'fn main() {\n'
                '    println!("{}", loaded_pdfium_path().unwrap_or_default());\n'
                '}\n', encoding='utf-8')
            binary = root / 'helper'
            compiled = subprocess.run(
                [rustc, '--edition', '2021', '-o', str(binary), str(root / 'main.rs')],
                capture_output=True, text=True, check=False, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            decoy = '/decoy/from/env/libpdfium.so'
            env = dict(os.environ, IRIS_PDFIUM_REQUIRE_LIBRARY=decoy)

            def read_maps(lines):
                maps.write_text('\n'.join(lines) + '\n', encoding='utf-8')
                result = subprocess.run([str(binary)], env=env, capture_output=True,
                                        text=True, check=False, timeout=60)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return result.stdout.strip()

            noise = [
                '7f0000000000-7f0000001000 r--p 00000000 08:01 1 /opt/notlibpdfium.so',
                '7f0000001000-7f0000002000 r--p 00000000 08:01 2 /opt/libpdfium.so.txt/other',
                '7f0000002000-7f0000003000 r--p 00000000 08:01 3 /opt/libpdfium.so-backup',
                '7f0000003000-7f0000004000 r--p 00000000 00:00 0',
            ]
            self.assertEqual(read_maps(noise + [
                '7f0000004000-7f0000005000 r-xp 00000000 08:01 4 '
                '/staged pdfium/libpdfium.so.8179',
                '7f0000005000-7f0000006000 r-xp 00000000 08:01 5 /usr/lib/libpdfium.so',
            ]), '/staged pdfium/libpdfium.so.8179')
            self.assertEqual(read_maps(noise + [
                '7f0000004000-7f0000005000 r-xp 00000000 08:01 4 /opt/pdfium/libpdfium.so',
            ]), '/opt/pdfium/libpdfium.so')
            self.assertEqual(read_maps(noise), '')

    def test_pairing_plan_marks_legacy_schema_unpaired(self):
        baseline = dict(corpus=legacy_section(), j2k=legacy_section())
        candidate = dict(corpus=section(), j2k=section())
        paired_corpus, paired_j2k, unpaired = m.pairing_plan(baseline, candidate)
        self.assertEqual(paired_corpus, [])
        self.assertIsNone(paired_j2k)
        self.assertEqual(len(unpaired), 2)
        self.assertEqual({entry["section"] for entry in unpaired}, {"corpus", "J2K"})
        for entry in unpaired:
            self.assertIn("no paired median is emitted", entry["reason"])
        self.assertFalse(m.identity_schema_matches(baseline["corpus"],
                                                    candidate["corpus"]))
        self.assertFalse(m.identity_schema_matches(baseline["j2k"],
                                                    candidate["j2k"]))
        # Direct strict pairing still fails closed for the mixed-schema pair.
        with self.assertRaises(ValueError):
            m.paired_records(baseline["j2k"], candidate["j2k"], "J2K")

    def test_pairing_plan_decides_sections_independently(self):
        baseline = dict(corpus=legacy_section(), j2k=section(100))
        candidate = dict(corpus=section(100), j2k=section(130))
        paired_corpus, paired_j2k, unpaired = m.pairing_plan(baseline, candidate)
        self.assertEqual(paired_corpus, [])
        self.assertEqual(paired_j2k['baseline_median_ns'], 100)
        self.assertEqual(paired_j2k['candidate_median_ns'], 130)
        self.assertEqual([entry["section"] for entry in unpaired], ["corpus"])

    def test_pairing_plan_pairs_matching_v2(self):
        baseline = dict(corpus=section(100), j2k=section(100))
        candidate = dict(corpus=section(130), j2k=section(130))
        paired_corpus, paired_j2k, unpaired = m.pairing_plan(baseline, candidate)
        self.assertEqual(unpaired, [])
        self.assertEqual(len(paired_corpus), 1)
        self.assertEqual(paired_corpus[0]["baseline_median_ns"], 100)
        self.assertEqual(paired_corpus[0]["candidate_median_ns"], 130)
        self.assertEqual(paired_j2k["baseline_median_ns"], 100)
        self.assertEqual(paired_j2k["candidate_median_ns"], 130)
        self.assertTrue(m.identity_schema_matches(baseline["corpus"],
                                                   candidate["corpus"]))
        self.assertTrue(m.identity_schema_matches(baseline["j2k"],
                                                   candidate["j2k"]))
        # J2K smoke has no PDFium runtime line; schema match must not require it.
        self.assertIsNone(baseline["j2k"].get("pdfium_runtime"))
        self.assertIsNone(candidate["j2k"].get("pdfium_runtime"))

    def test_paired_records_rejects_identity_schema_drift(self):
        baseline = section()
        for schema in ("iris.preview_identity.v1", None):
            changed = copy.deepcopy(baseline)
            if schema is None:
                del changed["correctness"]["identity"]["identity_schema"]
            else:
                changed["correctness"]["identity"]["identity_schema"] = schema
            with self.subTest(schema=schema), self.assertRaises(ValueError):
                m.paired_records(baseline, changed, "J2K")
            drifted = copy.deepcopy(baseline)
            if schema is None:
                del drifted["timings"][0]["identity"]["identity_schema"]
            else:
                drifted["timings"][0]["identity"]["identity_schema"] = schema
            with self.subTest(schema=schema), self.assertRaises(ValueError):
                m.paired_records(baseline, drifted, "J2K")
        self.assertFalse(m.identity_pair(identity(),
                                         identity(identity_schema="other")))
        legacy = legacy_section()["correctness"]["identity"]
        self.assertFalse(m.identity_pair(legacy, identity()))

    def test_pairing_plan_keeps_pdfium_gate_for_v2(self):
        baseline = section(pdfium_runtime=pdfium_runtime())
        candidate = section()
        with self.assertRaises(ValueError):
            m.paired_records(baseline, candidate, "J2K")
        reports_baseline = dict(corpus=section(pdfium_runtime=pdfium_runtime()),
                                j2k=section())
        reports_candidate = dict(corpus=section(), j2k=section())
        with self.assertRaises(ValueError):
            m.pairing_plan(reports_baseline, reports_candidate)

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

    def test_cargo_example_commands_use_target_names(self):
        """Cargo --example takes a target name; the .rs file name is not one."""
        for source in (m.CORPUS_HARNESS, m.HARNESS, m.GENERATOR):
            with self.subTest(source=source):
                command = m.cargo_example_command(source, '/tmp/argument')
                self.assertEqual(command[command.index('--example') + 1], source[:-3])
                self.assertNotIn(source, command)
                self.assertEqual(command[-1], '/tmp/argument')
        with self.assertRaises(ValueError):
            m.cargo_example_command('preview_corpus_smoke', '/tmp/argument')
        source_text = (ROOT / 'scripts' / 'parity_source_compare.py').read_text(encoding='utf-8')
        self.assertEqual(source_text.count('cargo_example_command('), 3)
        self.assertNotIn("'--example', example,", source_text)
        self.assertNotIn("'--example', GENERATOR,", source_text)

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

    def test_common_examples_are_staged_on_both_sides(self):
        with tempfile.TemporaryDirectory(prefix='iris-parity-examples-') as temporary:
            root = Path(temporary)
            source_repo = root / 'source'
            baseline = root / 'baseline'
            candidate = root / 'candidate'
            for checkout in (source_repo, baseline, candidate):
                (checkout / m.EXAMPLES).mkdir(parents=True)
            sources = {}
            for name in m.STAGED_EXAMPLES:
                path = source_repo / m.EXAMPLES / name
                path.write_text(name, encoding='utf-8')
                sources[name] = path
            build_source = source_repo / m.BUILD_SCRIPT
            build_source.write_text('build script\n', encoding='utf-8')
            staged = m.stage_common_examples(source_repo, baseline, candidate)
            self.assertEqual(set(staged), set(m.STAGED_EXAMPLES) | {str(m.BUILD_SCRIPT)})
            for name in m.STAGED_EXAMPLES:
                expected = sources[name].read_bytes()
                for checkout in (baseline, candidate):
                    target = checkout / m.EXAMPLES / name
                    self.assertTrue(target.is_file(), f'{checkout} missing {name}')
                    self.assertEqual(target.read_bytes(), expected)
            for checkout in (baseline, candidate):
                target = checkout / m.BUILD_SCRIPT
                self.assertTrue(target.is_file(), f'{checkout} missing build script')
                self.assertEqual(target.read_bytes(), build_source.read_bytes())
        self.assertIn(m.CORPUS_HARNESS, m.STAGED_EXAMPLES)
        self.assertNotIn('tests/preview-corpus', m.CORPUS_HARNESS)


def rust_function_bodies(source, marker):
    """Extract every Rust function in `source` whose signature starts with marker."""
    bodies = []
    start = 0
    while True:
        begin = source.find(marker, start)
        if begin < 0:
            return bodies
        opening = source.index('{', begin + len(marker) - 1)
        depth = 0
        for index in range(opening, len(source)):
            if source[index] == '{':
                depth += 1
            elif source[index] == '}':
                depth -= 1
                if depth == 0:
                    bodies.append(source[begin:index + 1])
                    start = index + 1
                    break
        else:
            raise ValueError(f'unterminated Rust function at offset {begin}')


def compiled_settings_function(example):
    """Extract the real compiled-build-settings helper from a smoke example."""
    source = (ROOT / m.EXAMPLES / example).read_text(encoding='utf-8')
    bodies = rust_function_bodies(source, 'fn compiled_build_settings()')
    m.require(bool(bodies), f'{example} has no compiled_build_settings helper')
    return bodies[0]


class ReportPathEquivalenceTests(unittest.TestCase):
    """Path aliases accepted by the provenance fix must stay narrow."""

    LONG = r"C:\Users\runneradmin\AppData\Local\Temp\iris-parity\baseline"
    SHORT = r"C:\Users\RUNNER~1\AppData\Local\Temp\iris-parity\baseline"

    def test_windows_short_name_aliases_match_in_both_directions(self):
        equivalent = m.report_path_equivalent
        for left, right in ((self.SHORT, self.LONG), (self.LONG, self.SHORT),
                            ("\\\\?\\" + self.SHORT, self.LONG),
                            (self.SHORT.upper(), self.LONG)):
            with self.subTest(left=left, right=right):
                self.assertTrue(equivalent(left, right, 'windows'))
        self.assertTrue(equivalent(self.SHORT, self.LONG))  # syntax auto-detected

    def test_windows_aliases_reject_unrelated_or_extra_components(self):
        equivalent = m.report_path_equivalent
        for other in (
                r"D:\Users\RUNNER~1\AppData\Local\Temp\iris-parity\baseline",
                r"C:\Users\RUNNER~1\AppData\Local\Other\iris-parity\baseline",
                r"C:\Users\RUNNER~1\AppData\Local\Temp\extra\iris-parity\baseline",
                r"C:\Users\RUNNER~1\AppData\Local\Temp\iris-parity\wrong",
                r"C:\Users\robert~1\AppData\Local\Temp\iris-parity\baseline",
                r"C:\Users\runner~1\AppData\Local\Temp\iris-parity\runner~2",
                r"C:\Users\verylongprefix~1\AppData\Local\Temp\iris-parity\baseline"):
            with self.subTest(other=other):
                self.assertFalse(equivalent(self.SHORT, other, 'windows'))
                self.assertFalse(equivalent(other, self.SHORT, 'windows'))

    def test_posix_comparison_stays_exact_and_case_sensitive(self):
        equivalent = m.report_path_equivalent
        self.assertTrue(equivalent('/opt/iris/baseline', '/opt/iris/baseline', 'linux'))
        self.assertFalse(equivalent('/opt/iris/baseline', '/Opt/iris/baseline', 'linux'))
        self.assertFalse(equivalent('/opt/iris-s~1/baseline', '/opt/iris-source/baseline',
                                    'linux'))
        self.assertFalse(equivalent('/opt/iris/baseline', self.SHORT))


class BuildScriptContractTests(unittest.TestCase):
    def prepare_crate(self, temporary, main_source):
        crate = Path(temporary) / 'tiny'
        (crate / 'src').mkdir(parents=True)
        (crate / 'Cargo.toml').write_text(
            '[package]\nname = "tiny"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n',
            encoding='utf-8')
        shutil.copyfile(ROOT / m.BUILD_SCRIPT, crate / 'build.rs')
        (crate / 'src' / 'main.rs').write_text(main_source, encoding='utf-8')
        return crate

    def build(self, crate, level, flags=None):
        cargo = shutil.which('cargo')
        self.assertIsNotNone(cargo, 'cargo is required for the compile-time identity test')
        env = {key: value for key, value in os.environ.items()
               if key not in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')
               and not (key.startswith('CARGO_TARGET_') and key.endswith('_RUSTFLAGS'))}
        env['CARGO_NET_OFFLINE'] = 'true'
        settings = dict(env,
                        CARGO_TARGET_DIR=str(crate.parent / f'target-{level}-{flags is not None}'),
                        CARGO_PROFILE_RELEASE_OPT_LEVEL=level)
        if flags is not None:
            settings['RUSTFLAGS'] = flags
        return subprocess.run([cargo, 'run', '--offline', '--quiet', '--release'],
                              cwd=crate, env=settings, capture_output=True, text=True,
                              check=False, timeout=180)

    def test_rust_unit_tests_guard_rustflags_forms(self):
        rustc = shutil.which('rustc')
        self.assertIsNotNone(rustc, 'rustc is required to run build.rs unit tests')
        with tempfile.TemporaryDirectory(prefix='iris-build-script-tests-') as temporary:
            binary = Path(temporary) / 'build_script_tests'
            compiled = subprocess.run(
                [rustc, '--edition', '2021', '--test', '-o', str(binary),
                 str(ROOT / m.BUILD_SCRIPT)],
                capture_output=True, text=True, check=False, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True,
                                    check=False, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('test result: ok', result.stdout + result.stderr)

    def test_compile_time_opt_levels_are_distinct_and_overrides_fail_closed(self):
        with tempfile.TemporaryDirectory(prefix='iris-preview-build-') as temporary:
            crate = self.prepare_crate(
                temporary,
                'fn main() {\n'
                '    let profile = option_env!("IRIS_BUILD_PROFILE").unwrap_or("<missing>");\n'
                '    let opt = option_env!("IRIS_BUILD_OPT_LEVEL").unwrap_or("<missing>");\n'
                '    let release = if profile == "release" { opt } else { "none" };\n'
                '    println!("{profile}|{release}");\n'
                '}\n')
            first, second = self.build(crate, '1'), self.build(crate, '2')
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first.stdout.strip(), 'release|1')
            self.assertEqual(second.stdout.strip(), 'release|2')
            self.assertNotEqual(first.stdout, second.stdout)
            rejected = self.build(crate, '3', '-C opt-level=z')
            self.assertNotEqual(rejected.returncode, 0, rejected.stdout)
            self.assertIn('opt-level', rejected.stderr)
            shorthand = self.build(crate, '1', '-O')
            self.assertNotEqual(shorthand.returncode, 0, shorthand.stdout)
            self.assertIn('overrides opt-level', shorthand.stderr)

    def test_real_smokes_bind_identity_to_compiled_settings(self):
        """Reverting either smoke to runtime-env identity must fail this suite."""
        for example in (m.CORPUS_HARNESS, m.HARNESS):
            with self.subTest(example=example):
                source = (ROOT / m.EXAMPLES / example).read_text(encoding='utf-8')
                self.assertNotIn(
                    'std::env::var("CARGO_PROFILE_RELEASE_OPT_LEVEL")', source)
                self.assertIn('"build_profile": build_profile', source)
                self.assertIn('"release_opt_level": release_opt_level', source)
                helper = compiled_settings_function(example)
                self.assertIn('option_env!("IRIS_BUILD_PROFILE")', helper)
                self.assertIn('option_env!("IRIS_BUILD_OPT_LEVEL")', helper)
                with tempfile.TemporaryDirectory(prefix='iris-smoke-identity-') as temporary:
                    crate = self.prepare_crate(
                        temporary,
                        'use std::error::Error;\n' + helper + '\n'
                        'fn main() {\n'
                        '    let (profile, level) = compiled_build_settings().unwrap();\n'
                        '    println!("{profile}|{level:?}");\n'
                        '}\n')
                    outputs = []
                    for level in ('1', '2'):
                        result = self.build(crate, level)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        outputs.append(result.stdout.strip())
                    self.assertEqual(outputs, ['release|Some("1")', 'release|Some("2")'])


if __name__ == '__main__':
    unittest.main()
