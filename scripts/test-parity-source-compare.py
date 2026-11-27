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


def compiled_settings_function(example):
    """Extract the real compiled-build-settings helper from a smoke example."""
    source = (ROOT / m.EXAMPLES / example).read_text(encoding='utf-8')
    marker = 'fn compiled_build_settings()'
    begin = source.find(marker)
    m.require(begin >= 0, f'{example} has no compiled_build_settings helper')
    opening = source.index('{', begin)
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == '{':
            depth += 1
        elif source[index] == '}':
            depth -= 1
            if depth == 0:
                return source[begin:index + 1]
    raise ValueError(f'{example} has an unterminated compiled_build_settings helper')


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
