#!/usr/bin/env python3
"""Report-only paired-source preview diagnostic; no parity claim or release-gate change.

Builds detached worktrees for two exact source revisions, runs the pinned corpus
smoke and the shared JPEG 2000 smoke on the same inputs, and writes a bounded
JSON report. It never uploads bytes, changes release policy or asserts parity.
Baseline dependencies may need network; run with CARGO_NET_OFFLINE=1 only when
both dependency sets are already cached.
"""

import argparse
from contextlib import ExitStack, contextmanager
import hashlib
import json
import ntpath
import os
from pathlib import Path
import posixpath
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BASELINE_SHA = "8e55c491f27b3cb9ca7c7245610812d3e7dcb5cc"
EXAMPLES = Path("crates/thumb-preview/examples")
CORPUS_HARNESS = "preview_corpus_smoke.rs"
HARNESS = "j2k_corpus_smoke.rs"
GENERATOR = "generate_j2k_fixture.rs"
BUILD_SCRIPT = Path("crates/thumb-preview/build.rs")
STAGED_EXAMPLES = (CORPUS_HARNESS, HARNESS, GENERATOR)
PDFIUM_REQUIRE_ENV = "IRIS_PDFIUM_REQUIRE_LIBRARY"
PDFIUM_LIBRARY_NAMES = {
    "linux": "libpdfium.so", "darwin": "libpdfium.so", "win32": "pdfium.dll",
}
FIXTURE = "paired-j2k.jp2"
FIXTURE_SHA256 = "e44d3480025d87433287c64775c0486cc2e6f7369c6df5b8a249d89f6d2a2caa"
FIXTURE_SIZE = 393
J2K_OPERATION = "image_generic::extract_image_thumbnail_from_bytes"
PREVIEW_OPERATIONS = {
    "image_generic::extract_image_thumbnail",
    "raw::extract_raw_thumbnail",
    "video::extract_video_thumbnail",
    "video::extract_hevc_thumbnail",
    "pdf::extract_pdf_thumbnail",
}
# Source revision and Cargo.lock legitimately differ; runner/toolchain identity must not.
IDENTITY_SCHEMA = "iris.preview_identity.v2"
IDENTITY_KEYS = (
    "identity_schema", "rustc", "target_os", "target_arch", "runner_name", "runner_os",
    "runner_arch", "runner_image_os", "runner_image_version", "cpu_model",
    "available_parallelism", "build_profile", "release_opt_level", "features",
    "ffmpeg_runtime",
)
MAX_COMMAND_BYTES = 65536
MAX_ERROR_BYTES = 4096
MAX_REPORT_BYTES = 266240
COMMAND_SECONDS = int(os.environ.get("IRIS_PARITY_SOURCE_COMMAND_SECONDS", "1800"))
SHA_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}\Z")
CORPUS_MANIFEST = Path("tests/preview-corpus/fixtures.json")
PARITY_CLAIM = "none; report-only diagnostic, release hold unchanged"
FFMPEG_POLICY = Path("packaging/linux/ffmpeg.json")
PINNED_FFMPEG_LICENSE = "LGPL version 2.1 or later"
# Windows uses the pinned vcpkg port, whose MSVC/platform flags necessarily differ
# from the Linux/macOS upstream recipe. Require the same ABI/license and the
# pinned port's safety invariants instead of the POSIX configure string.
WINDOWS_FFMPEG_REQUIRED_FLAGS = (
    "--disable-ffmpeg", "--disable-autodetect", "--enable-w32threads",
    "--toolchain=msvc", "--target-os=win32",
)
WINDOWS_FFMPEG_FORBIDDEN_FLAGS = (
    "--enable-gpl", "--enable-version3", "--enable-nonfree", "--enable-ffmpeg",
)


WINDOWS_SEPARATOR = '\\'


def require(ok, message):
    if not ok:
        raise ValueError(message)


def report_path_module(value, platform=None):
    """Select the path syntax that produced a report path.

    An explicit platform pins the syntax; otherwise a drive prefix, UNC or
    extended prefix, or a backslash without a leading POSIX slash, means
    Windows. All other paths use POSIX syntax.
    """
    require(platform is None or platform in ('linux', 'macos', 'windows'),
            'invalid report path platform')
    windows = (value.startswith(WINDOWS_SEPARATOR * 2) or re.match(r'[A-Za-z]:', value) is not None
               or (WINDOWS_SEPARATOR in value and not value.startswith('/')))
    return ntpath if platform == 'windows' or (platform is None and windows) else posixpath


def report_path_parts(value, platform=None):
    """Return (parent, name) in the producing platform's path syntax.

    Windows paths are normalized with ntpath, lose an extended-length prefix
    and compare case-insensitively; POSIX paths are normalized with posixpath
    and keep their case. Invalid paths fail closed.
    """
    require(type(value) is str and value.strip(), 'invalid report path')
    value = value.strip()
    module = report_path_module(value, platform)
    prefix = WINDOWS_SEPARATOR * 2 + '?' + WINDOWS_SEPARATOR
    if module is ntpath and value.startswith(prefix):
        value = value[len(prefix):]
    require(value.strip(), 'invalid report path')
    parent, name = module.split(module.normpath(value))
    if module is ntpath:
        return parent.casefold(), name.casefold()
    return parent, name


def report_path(value, platform=None):
    """Normalize a report path using the producing platform's syntax."""
    parent, name = report_path_parts(value, platform)
    return report_path_module(value, platform).join(parent, name)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pinned_ffmpeg_identity(repo=ROOT):
    """Derive the runtime identity shipped from the pinned upstream source.

    Only the current verifier's own pin is trusted; a paired worktree cannot
    redefine what counts as the pinned runtime by changing its copy of the
    policy file.
    """
    policy = json.loads((Path(repo) / FFMPEG_POLICY).read_text(encoding='utf-8'))
    version = policy.get('avcodec_version')
    flags = policy.get('configure_flags')
    require(type(version) is list and len(version) == 3
            and all(type(part) is int and 0 <= part <= 255 for part in version),
            'invalid pinned avcodec_version')
    require(type(flags) is list and flags
            and all(type(flag) is str and flag for flag in flags),
            'invalid pinned configure_flags')
    major, minor, micro = version
    return dict(codec_version=(major << 16) | (minor << 8) | micro,
                configuration=' '.join(flags), license=PINNED_FFMPEG_LICENSE)


def ffmpeg_configuration(value):
    """Normalize the configure command recorded by libavcodec.

    Production builds add their own prefix/libdir; those paths are not part of
    the runtime identity. Every other flag is compared exactly.
    """
    require(type(value) is str and value.strip(), 'invalid FFmpeg configuration')
    flags = shlex.split(value)
    selected = [flag for flag in flags
                if not flag.startswith(('--prefix=', '--libdir='))]
    return ' '.join(selected)


def run(command, cwd, env, label):
    """Bound retained diagnostics; fail closed on oversized success output."""
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen([str(item) for item in command], cwd=cwd, env=env,
                                   stdout=stdout, stderr=stderr, start_new_session=True)
        try:
            code = process.wait(timeout=COMMAND_SECONDS)
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            raise
        stderr.seek(0, os.SEEK_END)
        stderr.seek(max(0, stderr.tell() - MAX_ERROR_BYTES))
        tail = stderr.read().decode('utf-8', 'replace').strip()
        stdout.seek(0, os.SEEK_END)
        size = stdout.tell()
        require(code == 0, f"{label} failed (exit {code}): {tail}")
        require(size <= MAX_COMMAND_BYTES, f"{label} stdout exceeds command budget")
        stdout.seek(0)
        return stdout.read().decode('utf-8'), tail


def parse_records(text, operations, label):
    """Accept exactly one correctness record, the latency set and an optional runtime."""
    correctness, timings, runtimes = [], [], []
    for line in text.splitlines():
        if line.startswith('PREVIEW_PDFIUM_RUNTIME '):
            record = json.loads(line[len('PREVIEW_PDFIUM_RUNTIME '):])
            require(type(record) is dict and set(record) == {'path', 'required'}
                    and type(record['path']) is str and type(record['required']) is bool,
                    f'{label}: invalid pdfium runtime record')
            require(not runtimes, f'{label}: duplicate pdfium runtime record')
            runtimes.append(record)
            continue
        for marker, records, record_type in (
            ('PREVIEW_CORRECTNESS ', correctness, 'iris.preview_correctness.v1'),
            ('PREVIEW_LATENCY ', timings, 'iris.preview_latency.v1'),
        ):
            if line.startswith(marker):
                record = json.loads(line[len(marker):])
                require(record.get('record') == record_type, f'{label}: unexpected record type')
                records.append(record)
    require(len(correctness) == 1, f'{label}: expected one correctness record')
    require({item['operation'] for item in timings} == set(operations)
            and len(timings) == len(operations), f'{label}: latency operation set mismatch')
    require(all(item['passed'] is True for item in correctness), f'{label}: correctness failed')
    return dict(correctness=correctness[0], timings=timings,
                pdfium_runtime=runtimes[0] if runtimes else None)


def pinned_corpus_fixtures():
    """Read the verifier's own pinned corpus manifest, never a candidate's.

    Only manifest fields needed for input pinning are accepted; a corrupted or
    incomplete pin file fails closed instead of silently narrowing coverage.
    """
    manifest = json.loads((ROOT / CORPUS_MANIFEST).read_text(encoding='utf-8'))
    require(type(manifest) is dict, 'invalid pinned corpus manifest')
    files = manifest.get('files')
    require(type(files) is list and files, 'invalid pinned corpus manifest files')
    pins = {}
    for entry in files:
        require(type(entry) is dict, 'invalid pinned corpus manifest entry')
        filename, sha256, size = entry.get('filename'), entry.get('sha256'), entry.get('size')
        require(type(filename) is str and filename and filename not in pins,
                'invalid or duplicate pinned corpus filename')
        require(type(sha256) is str and SHA256_PATTERN.fullmatch(sha256) is not None,
                f'invalid pinned corpus sha256 for {filename}')
        require(type(size) is int and size > 0, f'invalid pinned corpus size for {filename}')
        pins[filename] = (sha256, size)
    return pins


def validate_exported_section(section, operations, label):
    """Reapply the producer's record rules to an exported report section.

    ``operations`` is the exact timing set requested for this section. Corpus
    fixtures are checked against the verifier's own pinned manifest; a J2K
    section is checked against the single shared fixture pin. This boundary
    check cannot be satisfied by agreeing mutations on both sides of a pair.
    """
    require(type(section) is dict, f'{label}: exported section must be an object')
    require(type(operations) in (list, tuple, set) and operations
            and all(type(item) is str and item for item in operations),
            f'{label}: invalid requested operations')
    expected_operations = set(operations)
    pins = ({FIXTURE: (FIXTURE_SHA256, FIXTURE_SIZE)}
            if expected_operations == {J2K_OPERATION} else pinned_corpus_fixtures())

    correctness = section.get('correctness')
    require(type(correctness) is dict, f'{label}: invalid correctness record')
    require(correctness.get('record') == 'iris.preview_correctness.v1',
            f'{label}: unexpected correctness record type')
    require(correctness.get('passed') is True,
            f'{label}: correctness.passed must be true')

    fixtures = correctness.get('fixtures')
    require(type(fixtures) is list and fixtures,
            f'{label}: correctness fixture coverage is empty or invalid')
    require(all(type(item) is dict for item in fixtures),
            f'{label}: invalid correctness fixture record')
    seen = set()
    for fixture in fixtures:
        filename, sha256, size = (fixture.get('filename'), fixture.get('sha256'),
                                  fixture.get('size_bytes'))
        require(type(filename) is str and filename and filename not in seen,
                f'{label}: invalid or duplicate fixture filename')
        seen.add(filename)
        require(type(sha256) is str and SHA256_PATTERN.fullmatch(sha256) is not None,
                f'{label}: invalid fixture sha256 for {filename}')
        require(type(size) is int and size > 0,
                f'{label}: invalid fixture size_bytes for {filename}')
        require(type(fixture.get('output')) is dict,
                f'{label}: invalid fixture output for {filename}')

    timings = section.get('timings')
    require(type(timings) is list and timings
            and all(type(item) is dict for item in timings),
            f'{label}: invalid or empty timing records')
    observed = []
    for record in timings:
        operation = record.get('operation')
        require(type(operation) is str and operation not in observed,
                f'{label}: duplicate or invalid timing operation')
        observed.append(operation)
        input_pin = record.get('input')
        require(type(input_pin) is dict, f'{label}: invalid timing input for {operation}')
        input_filename = input_pin.get('filename')
        require(type(input_filename) is str and input_filename in pins,
                f'{label}: timing input is not a pinned input for {operation}')
        require(input_pin.get('sha256') == pins[input_filename][0]
                and input_pin.get('size_bytes') == pins[input_filename][1],
                f'{label}: timing input pin mismatch for {operation}')
    require(set(observed) == expected_operations
            and len(observed) == len(expected_operations),
            f'{label}: timing operation set mismatch')

    require(seen == set(pins), f'{label}: fixture coverage does not match pinned inputs')
    for fixture in fixtures:
        sha256, size = pins[fixture['filename']]
        require(fixture['sha256'] == sha256 and fixture['size_bytes'] == size,
                f'{label}: {fixture["filename"]} fixture pin mismatch')


def identity_pair(baseline, candidate):
    """Runner, CPU and toolchain identity must match; source revision need not."""
    if baseline.get("identity_schema") != candidate.get("identity_schema"):
        return False
    return all(baseline.get(key) == candidate.get(key) for key in IDENTITY_KEYS)


def pdfium_library_name():
    """Platform-specific filename of the PDFium library staged for smokes."""
    return PDFIUM_LIBRARY_NAMES[sys.platform]


def stage_pdfium_library(source, checkout, label):
    """Copy the verified runtime into a worktree and return its exact path.

    The smoke's required-library gate accepts only a local candidate that
    canonicalizes to this file, so its runtime line identifies the staged bytes
    rather than a system search-path library.
    """
    source = Path(source).resolve()
    name = pdfium_library_name()
    require(source.is_file() and source.name == name,
            f"{label}: staged PDFium library must be a local {name} file")
    target = (Path(checkout) / name).resolve()
    shutil.copyfile(source, target)
    require(target.is_file() and digest(source) == digest(target),
            f"{label}: staged PDFium copy does not match the verified library")
    return target


def pdfium_runtime_pair(baseline, candidate, label):
    """Pair runtime identity while allowing one staged file per worktree."""
    if baseline is None or candidate is None:
        require(baseline == candidate, f'{label}: pdfium runtime mismatch')
        return
    require(type(baseline) is dict and type(candidate) is dict,
            f'{label}: invalid pdfium runtime record')
    for runtime in (baseline, candidate):
        require(set(runtime) == {'path', 'required'},
                f'{label}: invalid pdfium runtime record')
        require(type(runtime['path']) is str and runtime['path'].strip(),
                f'{label}: invalid pdfium runtime path')
        require(runtime['required'] is True,
                f'{label}: pdfium smoke did not require the staged library')
    _, baseline_name = report_path_parts(baseline['path'])
    _, candidate_name = report_path_parts(candidate['path'])
    require(baseline_name == candidate_name, f'{label}: pdfium runtime mismatch')
    for runtime, name in ((baseline, baseline_name), (candidate, candidate_name)):
        module = report_path_module(runtime['path'])
        expected = (PDFIUM_LIBRARY_NAMES['win32'] if module is ntpath else
                    PDFIUM_LIBRARY_NAMES['linux'])
        require(name == expected, f'{label}: pdfium runtime mismatch')


def compare_output(baseline, candidate, label):
    for key in ('width', 'height', 'decoded_rgb_sha256'):
        # Records without a pixel digest (for example passthrough outputs) are
        # paired on dimensions only; a digest present on one side is mandatory.
        require(baseline.get(key) == candidate.get(key), f'{label}: output {key} mismatch')


def paired_records(baseline, candidate, label):
    """Fail closed instead of comparing records from different experiments."""
    require(identity_pair(baseline['correctness']['identity'],
                          candidate['correctness']['identity']), f'{label}: identity mismatch')
    pdfium_runtime_pair(baseline.get('pdfium_runtime'),
                        candidate.get('pdfium_runtime'), label)
    left = {item['filename']: item for item in baseline['correctness']['fixtures']}
    right = {item['filename']: item for item in candidate['correctness']['fixtures']}
    require(set(left) == set(right), f'{label}: input set mismatch')
    for filename in sorted(left):
        before, after = left[filename], right[filename]
        require(before['sha256'] == after['sha256']
                and before['size_bytes'] == after['size_bytes'], f'{label}: input pin mismatch')
        compare_output(before['output'], after['output'], f'{label}: {filename}')

    before = {item['operation']: item for item in baseline['timings']}
    after = {item['operation']: item for item in candidate['timings']}
    require(set(before) == set(after), f'{label}: operation set mismatch')
    pairs = []
    for operation in sorted(before):
        first, second = before[operation], after[operation]
        require(first['input'] == second['input'], f'{label}: {operation} input mismatch')
        require(type(first.get('uses_ffmpeg')) is bool and type(second.get('uses_ffmpeg')) is bool
                and first['uses_ffmpeg'] == second['uses_ffmpeg'],
                f'{label}: {operation} uses_ffmpeg mismatch')
        require(identity_pair(first['identity'], second['identity']),
                f'{label}: {operation} identity mismatch')
        compare_output(first['output'], second['output'], f'{label}: {operation}')
        for key in ('unit', 'warmup_iterations', 'sample_count'):
            require(first['timing'][key] == second['timing'][key],
                    f'{label}: {operation} timing budget mismatch')
        b, c = first['timing']['median'], second['timing']['median']
        require(type(b) is int and type(c) is int and b >= 0 and c >= 0,
                f'{label}: {operation} invalid median')
        pairs.append(dict(operation=operation, input=first['input'],
                          output_dimensions=[first['output'].get('width'), first['output'].get('height')],
                          baseline_median_ns=b, candidate_median_ns=c,
                          candidate_minus_baseline_ns=c - b))
    return pairs


def identity_schema_matches(baseline_section, candidate_section):
    """True only when both sections use the v2 preview identity schema.

    The correctness identity and every timing identity on both sides must carry
    ``identity_schema == IDENTITY_SCHEMA``. PDFium runtime lines are not required
    here (the J2K smoke has none); strict pdfium_runtime equality stays inside
    paired_records().
    """
    for section in (baseline_section, candidate_section):
        if not isinstance(section, dict):
            return False
        correctness = section.get("correctness")
        if not isinstance(correctness, dict):
            return False
        identity = correctness.get("identity")
        if not isinstance(identity, dict):
            return False
        if identity.get("identity_schema") != IDENTITY_SCHEMA:
            return False
        timings = section.get("timings")
        if not isinstance(timings, list) or not timings:
            return False
        for item in timings:
            if not isinstance(item, dict):
                return False
            timing_identity = item.get("identity")
            if not isinstance(timing_identity, dict):
                return False
            if timing_identity.get("identity_schema") != IDENTITY_SCHEMA:
                return False
    return True


def pairing_plan(baseline, candidate):
    """Split paired medians from legacy-schema comparisons.

    Returns ``(paired_corpus, paired_j2k, unpaired)``. Each section is decided
    independently: a schema-incompatible pair emits no paired median and gets a
    bounded reason, while a matching v2 pair still uses the strict
    paired_records() gate, which is never swallowed here.
    """
    baseline = baseline if isinstance(baseline, dict) else {}
    candidate = candidate if isinstance(candidate, dict) else {}
    unpaired = []
    paired_corpus, paired_j2k = [], None
    if identity_schema_matches(baseline.get("corpus"), candidate.get("corpus")):
        paired_corpus = paired_records(baseline["corpus"], candidate["corpus"], "corpus")
    else:
        unpaired.append(dict(section="corpus",
                             reason="identity schema mismatch: no paired median is emitted"))
    if identity_schema_matches(baseline.get("j2k"), candidate.get("j2k")):
        paired_j2k = paired_records(baseline["j2k"], candidate["j2k"], "J2K")[0]
    else:
        unpaired.append(dict(section="J2K",
                             reason="identity schema mismatch: no paired median is emitted"))
    return paired_corpus, paired_j2k, unpaired


def report_bytes(report):
    data = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + '\n'
    require(len(data.encode()) <= MAX_REPORT_BYTES, 'report exceeds byte budget')
    return data


def source_file(candidate, repo, relative):
    """Prefer the candidate checkout; allow an uncommitted candidate working tree."""
    path = candidate / relative
    if not path.is_file():
        path = repo / relative
    require(path.is_file(), f'candidate has no {relative}')
    return path


def install_file(source, checkout, relative):
    """Stage a shared file without copying it onto itself."""
    require(source.is_file(), f'missing shared file {relative}')
    destination = checkout / relative
    if source.resolve() != destination.resolve():
        shutil.copy2(source, destination)
    require(destination.is_file(), f'{checkout} has no {relative}')


def source_example(candidate, repo, name):
    return source_file(candidate, repo, EXAMPLES / name)


def install_example(source, checkout, name):
    install_file(source, checkout, EXAMPLES / name)


def stage_common_examples(repo, baseline, candidate):
    """Share the candidate's v2 examples and build script on both sides."""
    sources = {name: source_example(candidate, repo, name) for name in STAGED_EXAMPLES}
    sources[str(BUILD_SCRIPT)] = source_file(candidate, repo, BUILD_SCRIPT)
    for checkout in (baseline, candidate):
        for name in STAGED_EXAMPLES:
            install_example(sources[name], checkout, name)
        install_file(sources[str(BUILD_SCRIPT)], checkout, BUILD_SCRIPT)
    return sources


@contextmanager
def worktree(repo, sha, scratch, name):
    path = scratch / name
    run(['git', '-C', repo, 'worktree', 'add', '--detach', path, sha], ROOT,
        os.environ.copy(), f'worktree add {name}')
    try:
        yield path
    finally:
        # Remove even when a smoke fails; TemporaryDirectory cannot clean a
        # registered worktree by itself.
        subprocess.run(['git', '-C', str(repo), 'worktree', 'remove', '--force', str(path)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        shutil.rmtree(path, ignore_errors=True)
        subprocess.run(['git', '-C', str(repo), 'worktree', 'prune'],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def cargo_env(sha, warmups, samples, pdfium_library=None):
    env = os.environ.copy()
    env.update(IRIS_PREVIEW_LATENCY_WARMUPS=str(warmups),
               IRIS_PREVIEW_LATENCY_SAMPLES=str(samples), GITHUB_SHA=sha)
    if pdfium_library is not None:
        env[PDFIUM_REQUIRE_ENV] = str(Path(pdfium_library).resolve())
    return env


def cargo_example_command(example, argument):
    """Build a Cargo run using the example target name, not its source file name."""
    require(example.endswith('.rs') and '/' not in example and WINDOWS_SEPARATOR not in example,
            f'invalid example source name {example}')
    return ['cargo', 'run', '--release', '--locked', '-p', 'iris-thumb-preview',
            '--example', example[:-3], '--', argument]


def smoke(checkout, example, argument, env, label):
    command = cargo_example_command(example, argument)
    text, _ = run(command, checkout, env, label)
    return parse_records(text, PREVIEW_OPERATIONS if example == CORPUS_HARNESS
                         else {J2K_OPERATION}, label)


def compare(args):
    repo, corpus, output = args.repo.resolve(), args.corpus.resolve(), args.output.resolve()
    require(repo.is_dir() and corpus.is_dir(), 'repo and corpus directories are required')
    require(SHA_PATTERN.fullmatch(args.baseline) and SHA_PATTERN.fullmatch(args.candidate),
            'baseline and candidate must be full SHA values')
    require(args.warmups >= 0 and args.samples > 0, 'invalid iteration budget')
    with tempfile.TemporaryDirectory(prefix='iris-parity-source-') as scratch, ExitStack() as stack:
        scratch = Path(scratch)
        baseline = stack.enter_context(worktree(repo, args.baseline, scratch, 'baseline'))
        candidate = stack.enter_context(worktree(repo, args.candidate, scratch, 'candidate'))
        sources = stage_common_examples(repo, baseline, candidate)
        harness = sources[HARNESS]
        harness_sha = digest(harness)
        build_script_sha = digest(sources[str(BUILD_SCRIPT)])
        require(digest(baseline / 'tests/preview-corpus/fixtures.json')
                == digest(candidate / 'tests/preview-corpus/fixtures.json'),
                'paired revisions pin different corpus manifests')

        fixture = scratch / FIXTURE
        run(cargo_example_command(GENERATOR, fixture), candidate,
            cargo_env(args.candidate, args.warmups, args.samples), 'J2K fixture generation')
        require(digest(fixture) == FIXTURE_SHA256 and fixture.stat().st_size == FIXTURE_SIZE,
                'generated J2K fixture does not match its pin')

        reports = {}
        for name, checkout, sha in (('baseline', baseline, args.baseline),
                                    ('candidate', candidate, args.candidate)):
            library = (stage_pdfium_library(args.pdfium_library, checkout,
                                            f'{name} PDFium staging')
                       if args.pdfium_library is not None else None)
            env = cargo_env(sha, args.warmups, args.samples, library)
            reports[name] = dict(
                worktree=str(checkout.resolve()),
                corpus=smoke(checkout, CORPUS_HARNESS, corpus, env, f'{name} corpus smoke'),
                j2k=smoke(checkout, HARNESS, fixture, env, f'{name} J2K smoke'),
            )
        paired_corpus, paired_j2k, unpaired = pairing_plan(reports['baseline'],
                                                            reports['candidate'])
        report = dict(
            schema='iris.parity_source_compare.v1', parity_claim=PARITY_CLAIM,
            source_revision=dict(baseline=args.baseline, candidate=args.candidate),
            cargo_lock_sha256=dict(baseline=digest(baseline / 'Cargo.lock'),
                                   candidate=digest(candidate / 'Cargo.lock')),
            harness_sha256=harness_sha,
            build_script_sha256=build_script_sha,
            fixture_pin=dict(filename=FIXTURE, sha256=FIXTURE_SHA256, size_bytes=FIXTURE_SIZE),
            baseline=reports['baseline'], candidate=reports['candidate'],
            paired_latency=paired_corpus, paired_j2k=paired_j2k, unpaired=unpaired,
            cargo_net_offline=os.environ.get('CARGO_NET_OFFLINE'),
            limits='Same-runner, sequential, crate-level source diagnostic; no UI latency, '
                   'release-gate, platform or feature-parity claim.',
        )
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(report_bytes(report), encoding='utf-8')
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=ROOT)
    parser.add_argument('--baseline', default=BASELINE_SHA)
    parser.add_argument('--candidate', required=True)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--warmups', type=int, default=2)
    parser.add_argument('--samples', type=int, default=21)
    parser.add_argument('--pdfium-library', type=Path, default=None,
                        help='verified PDFium library staged into both worktrees')
    args = parser.parse_args()
    try:
        print(f'wrote {compare(args)} (report-only; release hold unchanged)')
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError,
            json.JSONDecodeError) as error:
        parser.exit(1, f'parity source compare failed: {error}\n')


if __name__ == '__main__':
    main()
