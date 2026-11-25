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
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BASELINE_SHA = "12fb51a0ba07c299ea4e9b08890a895797148e11"
EXAMPLES = Path("crates/thumb-preview/examples")
HARNESS = "j2k_corpus_smoke.rs"
GENERATOR = "generate_j2k_fixture.rs"
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
IDENTITY_KEYS = (
    "rustc", "target_os", "target_arch", "runner_name", "runner_os", "runner_arch",
    "runner_image_os", "runner_image_version", "cpu_model", "available_parallelism",
)
MAX_COMMAND_BYTES = 65536
MAX_ERROR_BYTES = 4096
MAX_REPORT_BYTES = 266240
COMMAND_SECONDS = int(os.environ.get("IRIS_PARITY_SOURCE_COMMAND_SECONDS", "1800"))
SHA_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
PARITY_CLAIM = "none; report-only diagnostic, release hold unchanged"


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


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
    """Accept exactly one correctness record and the expected latency set."""
    correctness, timings = [], []
    for line in text.splitlines():
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
    return dict(correctness=correctness[0], timings=timings)


def identity_pair(baseline, candidate):
    """Runner, CPU and toolchain identity must match; source revision need not."""
    return all(baseline.get(key) == candidate.get(key) for key in IDENTITY_KEYS)


def compare_output(baseline, candidate, label):
    for key in ('width', 'height', 'decoded_rgb_sha256'):
        # Records without a pixel digest (for example passthrough outputs) are
        # paired on dimensions only; a digest present on one side is mandatory.
        require(baseline.get(key) == candidate.get(key), f'{label}: output {key} mismatch')


def paired_records(baseline, candidate, label):
    """Fail closed instead of comparing records from different experiments."""
    require(identity_pair(baseline['correctness']['identity'],
                          candidate['correctness']['identity']), f'{label}: identity mismatch')
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


def report_bytes(report):
    data = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + '\n'
    require(len(data.encode()) <= MAX_REPORT_BYTES, 'report exceeds byte budget')
    return data


def source_example(candidate, repo, name):
    """Prefer the candidate checkout; allow an uncommitted candidate working tree."""
    path = candidate / EXAMPLES / name
    if not path.is_file():
        path = repo / EXAMPLES / name
    require(path.is_file(), f'candidate has no {name} example')
    return path


def install_example(source, checkout, name):
    """Stage a shared example without copying a file onto itself."""
    require(source.is_file(), f'missing shared example {name}')
    destination = checkout / EXAMPLES / name
    if source.resolve() != destination.resolve():
        shutil.copy2(source, destination)
    require(destination.is_file(), f'{checkout} has no {name} example')


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


def cargo_env(sha, warmups, samples):
    env = os.environ.copy()
    env.update(IRIS_PREVIEW_LATENCY_WARMUPS=str(warmups),
               IRIS_PREVIEW_LATENCY_SAMPLES=str(samples), GITHUB_SHA=sha)
    return env


def smoke(checkout, example, argument, env, label):
    command = ['cargo', 'run', '--release', '--locked', '-p', 'iris-thumb-preview',
               '--example', example, '--', argument]
    text, _ = run(command, checkout, env, label)
    return parse_records(text, PREVIEW_OPERATIONS if example == 'preview_corpus_smoke'
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
        harness = source_example(candidate, repo, HARNESS)
        generator = source_example(candidate, repo, GENERATOR)
        harness_sha = digest(harness)
        # Examples absent from either revision are staged as untracked files.
        for checkout in (baseline, candidate):
            install_example(harness, checkout, HARNESS)
            install_example(generator, checkout, GENERATOR)
        require(digest(baseline / 'tests/preview-corpus/fixtures.json')
                == digest(candidate / 'tests/preview-corpus/fixtures.json'),
                'paired revisions pin different corpus manifests')

        fixture = scratch / FIXTURE
        run(['cargo', 'run', '--release', '--locked', '-p', 'iris-thumb-preview',
             '--example', GENERATOR, '--', fixture], candidate,
            cargo_env(args.candidate, args.warmups, args.samples), 'J2K fixture generation')
        require(digest(fixture) == FIXTURE_SHA256 and fixture.stat().st_size == FIXTURE_SIZE,
                'generated J2K fixture does not match its pin')

        reports = {}
        for name, checkout, sha in (('baseline', baseline, args.baseline),
                                    ('candidate', candidate, args.candidate)):
            env = cargo_env(sha, args.warmups, args.samples)
            reports[name] = dict(
                corpus=smoke(checkout, 'preview_corpus_smoke', corpus, env, f'{name} corpus smoke'),
                j2k=smoke(checkout, HARNESS, fixture, env, f'{name} J2K smoke'),
            )
        paired_corpus = paired_records(reports['baseline']['corpus'],
                                       reports['candidate']['corpus'], 'corpus')
        paired_j2k = paired_records(reports['baseline']['j2k'],
                                    reports['candidate']['j2k'], 'J2K')[0]
        report = dict(
            schema='iris.parity_source_compare.v1', parity_claim=PARITY_CLAIM,
            source_revision=dict(baseline=args.baseline, candidate=args.candidate),
            cargo_lock_sha256=dict(baseline=digest(baseline / 'Cargo.lock'),
                                   candidate=digest(candidate / 'Cargo.lock')),
            harness_sha256=harness_sha,
            fixture_pin=dict(filename=FIXTURE, sha256=FIXTURE_SHA256, size_bytes=FIXTURE_SIZE),
            baseline=reports['baseline'], candidate=reports['candidate'],
            paired_latency=paired_corpus, paired_j2k=paired_j2k,
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
    args = parser.parse_args()
    try:
        print(f'wrote {compare(args)} (report-only; release hold unchanged)')
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError,
            json.JSONDecodeError) as error:
        parser.exit(1, f'parity source compare failed: {error}\n')


if __name__ == '__main__':
    main()
