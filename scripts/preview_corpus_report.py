#!/usr/bin/env python3
"""Validate bounded records from the fixture-backed preview corpus smoke."""
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]

# This is a fixed contract with preview_corpus_smoke, not an operator scan policy.
PREVIEW_OPERATIONS = {
    'image_generic::extract_image_thumbnail': ('landscape.jpg', False),
    'raw::extract_raw_thumbnail': ('canon-eos-r.cr3', False),
    'video::extract_video_thumbnail': ('bunny.mp4', True),
    'pdf::extract_pdf_thumbnail': ('one-page.pdf', False),
}
PDF_RUNTIMES = {
    'macos': dict(renderer='CoreGraphics', runtime='macOS system framework'),
    'linux': dict(
        renderer='PDFium',
        runtime='dynamically loaded PDFium; this diagnostic does not establish provenance or attestation',
    ),
}
PDF_VALIDATION = (
    'decoded RGB equality to correctness baseline on every warmup/sample; validation excluded from timing'
)
PDF_COMPARISON = 'renderer-specific diagnostic only; not an FFmpeg effect or cross-platform comparison'


def require(ok, message):
    if not ok:
        raise ValueError(message)


def setting(name, default):
    value = int(os.environ.get(name, default))
    require(value > 0, f'{name} must be positive')
    return value


def parse_preview(text, source, target_os='macos'):
    """Accept complete, bounded smoke records; never retain command logs."""
    require(target_os in PDF_RUNTIMES, 'unsupported preview platform')
    require(len(text.encode()) <= setting('IRIS_FFMPEG_COMMAND_BYTES', 32768),
            'preview output exceeds command budget')
    correctness, timings = [], []
    for line in text.splitlines():
        for marker, records in (('PREVIEW_CORRECTNESS ', correctness), ('PREVIEW_LATENCY ', timings)):
            if line.startswith(marker):
                records.append(json.loads(line[len(marker):]))
    require(len(correctness) == 1 and len(timings) == len(PREVIEW_OPERATIONS),
            'missing or duplicate preview records')
    correct = correctness[0]
    require(correct['record'] == 'iris.preview_correctness.v1' and correct['passed'] is True,
            'preview correctness failed')
    identity = correct['identity']
    required_identity = (
        'source_revision', 'workflow_run_id', 'cargo_lock_sha256', 'rustc', 'target_os', 'target_arch',
        'runner_name', 'runner_os', 'runner_arch', 'runner_image_os', 'runner_image_version', 'cpu_model',
        'available_parallelism', 'build_profile', 'release_opt_level', 'crate', 'crate_version',
    )
    for key in required_identity:
        require(identity.get(key) not in (None, ''), f'missing preview identity: {key}')
    require(identity['source_revision'] == source and identity['target_os'] == target_os
            and identity['features'] == dict(video=True, pdf=True, raw_full=True)
            and identity['build_profile'] == 'release', 'preview source/features/profile mismatch')

    pins = {
        item['filename']: item
        for item in json.loads((ROOT / 'tests/preview-corpus/fixtures.json').read_text())['files']
    }
    fixtures = correct['fixtures']
    require(len(fixtures) == len(pins) and {item['filename'] for item in fixtures} == set(pins),
            'preview correctness fixture set mismatch')
    for fixture in fixtures:
        pin = pins[fixture['filename']]
        require(fixture['sha256'] == pin['sha256'] and fixture['size_bytes'] == pin['size'],
                'preview fixture pin mismatch')
        validate_preview_output(fixture['output'])
    by_name = {fixture['filename']: fixture for fixture in fixtures}
    require({timing['operation'] for timing in timings} == set(PREVIEW_OPERATIONS),
            'preview operation set mismatch')

    warmups = int(os.environ['IRIS_PREVIEW_LATENCY_WARMUPS'])
    samples = int(os.environ['IRIS_PREVIEW_LATENCY_SAMPLES'])
    require(warmups >= 0 and samples > 0, 'invalid preview iteration budget')
    for timing in timings:
        name, uses_ffmpeg = PREVIEW_OPERATIONS[timing['operation']]
        fixture = by_name[name]
        require(timing['record'] == 'iris.preview_latency.v1' and timing['identity'] == identity
                and timing['uses_ffmpeg'] is uses_ffmpeg, 'preview timing identity mismatch')
        require(timing['input'] == dict(filename=name, sha256=fixture['sha256'], size_bytes=fixture['size_bytes']),
                'preview timing fixture mismatch')
        require({key: timing['output'][key] for key in fixture['output']} == fixture['output'],
                'preview timing output mismatch')
        if name == 'one-page.pdf':
            pixel_digest = fixture['output'].get('decoded_rgb_sha256', '')
            require(re.fullmatch(r'[0-9a-f]{64}', pixel_digest) is not None
                    and timing.get('pdf_runtime') == PDF_RUNTIMES[target_os]
                    and timing.get('repeated_output_validation') == PDF_VALIDATION
                    and timing.get('comparison') == PDF_COMPARISON,
                    'invalid PDF renderer/pixel validation record')
        stats = timing['timing']
        require(stats['unit'] == 'nanoseconds' and stats['warmup_iterations'] == warmups
                and stats['sample_count'] == samples, 'preview timing budget mismatch')
        values = [stats[key] for key in ('min', 'median', 'p90', 'max', 'median_absolute_deviation')]
        require(all(type(value) is int and 0 <= value <= (1 << 64) - 1 for value in values)
                and values[:4] == sorted(values[:4]), 'invalid preview timing statistics')
    return dict(correctness=correct, timings=timings)


def validate_preview_output(output):
    require(output['format'] == 'jpeg' and isinstance(output['source'], str) and output['source']
            and all(type(output[key]) is int and 0 < output[key] <= 256 for key in ('width', 'height')),
            'invalid preview output dimensions/format/source')
