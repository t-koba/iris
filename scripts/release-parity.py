#!/usr/bin/env python3
"""Offline release gate for same-source paired parity evidence.

Consumes three bounded reports from scripts/parity_source_compare.py, one per
platform, and verifies source/revision/harness binding, strict identity, a
recomputed pairing with no unpaired section and no candidate slowdown. This
verifier makes no parity claim of its own, runs no builds and writes nothing.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from parity_source_compare import (  # noqa: E402
    BASELINE_SHA, BUILD_SCRIPT, FIXTURE, FIXTURE_SHA256, FIXTURE_SIZE, HARNESS,
    IDENTITY_SCHEMA, J2K_OPERATION, PARITY_CLAIM, PREVIEW_OPERATIONS,
    WINDOWS_FFMPEG_FORBIDDEN_FLAGS, WINDOWS_FFMPEG_REQUIRED_FLAGS, ffmpeg_configuration,
    pairing_plan, pinned_ffmpeg_identity, require,
)

ROOT = Path(__file__).resolve().parents[1]
REPORT_SCHEMA = "iris.parity_source_compare.v1"
PLATFORMS = ("linux", "macos", "windows")
PLATFORM_IDENTITY = {
    "linux": {"target_os": "linux", "runner_os": "Linux"},
    "macos": {"target_os": "macos", "runner_os": "macOS"},
    "windows": {"target_os": "windows", "runner_os": "Windows"},
}
POLICY_KEYS = {
    "schema", "baseline_sha", "required_platforms", "platform_identity",
    "required_operations", "warmups", "samples", "max_candidate_minus_baseline_ns",
    "max_report_bytes",
}
SHA_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
MAX_POLICY_BYTES = 65536


def exact_int(value, expected, label):
    require(type(value) is int and value == expected, f"invalid policy {label}")


def load_policy(path):
    data = path.read_bytes()
    require(len(data) <= MAX_POLICY_BYTES, "policy exceeds byte budget")
    policy = json.loads(data.decode("utf-8"))
    require(type(policy) is dict and set(policy) == POLICY_KEYS,
            "invalid release parity policy shape")
    exact_int(policy["schema"], 1, "schema")
    require(type(policy["baseline_sha"]) is str
            and SHA_PATTERN.fullmatch(policy["baseline_sha"]) is not None
            and policy["baseline_sha"] == BASELINE_SHA, "invalid policy baseline_sha")
    platforms = policy["required_platforms"]
    require(type(platforms) is list and len(platforms) == len(PLATFORMS)
            and all(type(item) is str for item in platforms)
            and set(platforms) == set(PLATFORMS), "invalid policy required_platforms")
    identity = policy["platform_identity"]
    require(type(identity) is dict and set(identity) == set(PLATFORMS),
            "invalid policy platform_identity")
    for platform in PLATFORMS:
        require(type(identity[platform]) is dict
                and set(identity[platform]) == {"target_os", "runner_os"}
                and identity[platform] == PLATFORM_IDENTITY[platform],
                f"invalid policy platform_identity for {platform}")
    operations = policy["required_operations"]
    require(type(operations) is list and len(operations) == len(PREVIEW_OPERATIONS)
            and all(type(item) is str for item in operations)
            and set(operations) == set(PREVIEW_OPERATIONS),
            "invalid policy required_operations")
    exact_int(policy["warmups"], 2, "warmups")
    exact_int(policy["samples"], 21, "samples")
    exact_int(policy["max_candidate_minus_baseline_ns"], 0,
              "max_candidate_minus_baseline_ns")
    exact_int(policy["max_report_bytes"], 266240, "max_report_bytes")
    return policy


def load_report(path, max_bytes, label):
    data = path.read_bytes()
    require(len(data) <= max_bytes, f"{label}: report exceeds byte budget")
    report = json.loads(data.decode("utf-8"), parse_constant=reject_constant)
    require(type(report) is dict, f"{label}: report must be a JSON object")
    return report


def reject_constant(value):
    raise ValueError(f"non-finite JSON constant {value}")


def section_of(report, side, name, label):
    parent = report.get(side)
    require(type(parent) is dict, f"{label}: missing {side} section")
    section = parent.get(name)
    require(type(section) is dict, f"{label}: missing {side} {name} section")
    return section


def timing_map(section, label):
    records = section.get("timings")
    require(type(records) is list and records
            and all(type(item) is dict for item in records),
            f"{label}: invalid timing records")
    result = {}
    for record in records:
        operation = record.get("operation")
        require(type(operation) is str and operation not in result,
                f"{label}: duplicate or invalid timing operation")
        result[operation] = record
    return result


def check_identities(report, policy, platform, expected, label, run_ids):
    spec = policy["platform_identity"][platform]
    for side, revision in (("baseline", policy["baseline_sha"]),
                           ("candidate", expected)):
        for name in ("corpus", "j2k"):
            section = section_of(report, side, name, label)
            correctness = section.get("correctness")
            require(type(correctness) is dict, f"{label}: invalid {side} {name} correctness")
            timings = section.get("timings")
            require(type(timings) is list and timings
                    and all(type(item) is dict for item in timings),
                    f"{label}: invalid {side} {name} timings")
            for record in [correctness, *timings]:
                require(type(record) is dict, f"{label}: invalid {side} {name} identity record")
                identity = record.get("identity")
                require(type(identity) is dict, f"{label}: invalid {side} {name} identity")
                require(identity.get("identity_schema") == IDENTITY_SCHEMA,
                        f"{label}: {side} {name} identity schema mismatch")
                profile, level = identity.get("build_profile"), identity.get("release_opt_level")
                require(profile in ("debug", "release"),
                        f"{label}: {side} {name} invalid build profile")
                require((profile == "debug" and level is None)
                        or (profile == "release" and type(level) is str
                            and level in ("0", "1", "2", "3", "s", "z")),
                        f"{label}: {side} {name} invalid release_opt_level")
                require(identity.get("source_revision") == revision,
                        f"{label}: {side} {name} source_revision mismatch")
                require(identity.get("target_os") == spec["target_os"]
                        and identity.get("runner_os") == spec["runner_os"],
                        f"{label}: {side} {name} platform identity mismatch")
                run_id = identity.get("workflow_run_id")
                require(type(run_id) is str and run_id.strip(),
                        f"{label}: {side} {name} missing workflow_run_id")
                run_ids.add(run_id)


def check_pdfium_attribution(report, platform, label):
    """Non-macOS corpus timings must name the PDFium library actually loaded."""
    if platform == "macos":
        return
    for side in ("baseline", "candidate"):
        runtime = section_of(report, side, "corpus", label).get("pdfium_runtime")
        require(type(runtime) is dict and type(runtime.get("path")) is str
                and runtime["path"].strip(),
                f"{label}: {side} corpus has no loaded PDFium library attribution")


def check_pinned_ffmpeg_runtime(report, platform, label, pinned):
    """FFmpeg-bearing corpus records must report the pinned bundled runtime.

    A system runtime can agree with itself on both sides of a pair, so strict
    pairing alone is not enough: the recorded ABI, configure flags and license
    must also match the FFmpeg pin used by the production packages. POSIX builds
    use one upstream configure recipe; Windows uses the pinned vcpkg port and is
    checked against its invariant MSVC/LGPL flags.
    """
    for side in ('baseline', 'candidate'):
        corpus = section_of(report, side, 'corpus', label)
        correctness = corpus.get('correctness')
        require(type(correctness) is dict, f'{label}: invalid {side} corpus correctness')
        records = [correctness, *(record for record in corpus['timings']
                                 if record.get('uses_ffmpeg') is True)]
        for record in records:
            identity = record.get('identity')
            runtime = identity.get('ffmpeg_runtime') if type(identity) is dict else None
            require(type(runtime) is dict, f'{label}: {side} corpus has no FFmpeg runtime identity')
            version = runtime.get('codec_version')
            require(type(version) is int and version == pinned['codec_version'],
                    f'{label}: {side} pinned FFmpeg codec_version mismatch')
            try:
                flags = shlex.split(ffmpeg_configuration(runtime.get('configuration')))
            except ValueError as error:
                raise ValueError(f'{label}: {side} {error}') from error
            if platform == 'windows':
                missing = [flag for flag in WINDOWS_FFMPEG_REQUIRED_FLAGS if flag not in flags]
                forbidden = [flag for flag in WINDOWS_FFMPEG_FORBIDDEN_FLAGS if flag in flags]
                require(not missing and not forbidden,
                        f'{label}: {side} pinned vcpkg FFmpeg flags mismatch')
            else:
                require(' '.join(flags) == pinned['configuration'],
                        f'{label}: {side} pinned FFmpeg configuration mismatch')
            require(runtime.get('license') == pinned['license'],
                    f'{label}: {side} pinned FFmpeg license mismatch')


def check_pairs(pairs, maps, section_name, policy, label):
    require(type(pairs) is list and pairs and all(type(pair) is dict for pair in pairs),
            f"{label}: missing {section_name} pairs")
    for pair in pairs:
        operation = pair.get("operation")
        require(type(operation) is str
                and all(operation in maps[section_name][side] for side in ("baseline", "candidate")),
                f"{label}: {section_name} pair has no source timing for {operation}")
        baseline = pair.get("baseline_median_ns")
        candidate = pair.get("candidate_median_ns")
        delta = pair.get("candidate_minus_baseline_ns")
        require(type(baseline) is int and baseline > 0,
                f"{label}: {section_name} {operation} invalid baseline median")
        require(type(candidate) is int and candidate >= 0,
                f"{label}: {section_name} {operation} invalid candidate median")
        require(type(delta) is int and delta == candidate - baseline,
                f"{label}: {section_name} {operation} median delta mismatch")
        require(delta <= policy["max_candidate_minus_baseline_ns"],
                f"{label}: {section_name} {operation} candidate slowdown {delta} ns")
        for side, median in (("baseline", baseline), ("candidate", candidate)):
            record = maps[section_name][side][operation]
            timing = record.get("timing") if type(record) is dict else None
            require(type(timing) is dict, f"{label}: {side} {operation} missing timing")
            require(timing.get("unit") == "nanoseconds",
                    f"{label}: {side} {operation} timing unit mismatch")
            require(type(timing.get("warmup_iterations")) is int
                    and timing["warmup_iterations"] == policy["warmups"],
                    f"{label}: {side} {operation} warmup count mismatch")
            require(type(timing.get("sample_count")) is int
                    and timing["sample_count"] == policy["samples"],
                    f"{label}: {side} {operation} sample count mismatch")
            require(timing.get("median") == median,
                    f"{label}: {side} {operation} paired median does not match source")


def validate_report(report, path, platform, expected, harness_digest, build_script_digest,
                   policy, run_ids, pinned):
    label = f"{platform} report {path}"
    require(report.get("schema") == REPORT_SCHEMA, f"{label}: unexpected schema")
    require(report.get("parity_claim") == PARITY_CLAIM, f"{label}: unexpected parity_claim")
    revision = report.get("source_revision")
    require(type(revision) is dict and set(revision) == {"baseline", "candidate"},
            f"{label}: invalid source_revision")
    require(revision["baseline"] == policy["baseline_sha"]
            and revision["candidate"] == expected, f"{label}: source revision mismatch")
    pin = report.get("fixture_pin")
    require(type(pin) is dict and set(pin) == {"filename", "sha256", "size_bytes"}
            and pin["filename"] == FIXTURE and pin["sha256"] == FIXTURE_SHA256
            and pin["size_bytes"] == FIXTURE_SIZE, f"{label}: fixture pin mismatch")
    require(report.get("harness_sha256") == harness_digest,
            f"{label}: harness digest mismatch")
    require(report.get("build_script_sha256") == build_script_digest,
            f"{label}: build script digest mismatch")

    try:
        paired, paired_j2k, unpaired = pairing_plan(report.get("baseline"),
                                                    report.get("candidate"))
    except Exception as error:
        raise ValueError(f"{label}: pairing recomputation failed: {error}") from error
    require(report.get("paired_latency") == paired, f"{label}: paired_latency was tampered with")
    require(report.get("paired_j2k") == paired_j2k, f"{label}: paired_j2k was tampered with")
    require(report.get("unpaired") == unpaired, f"{label}: unpaired summary was tampered with")
    require(unpaired == [], f"{label}: unpaired sections are present")
    require(type(paired_j2k) is dict and paired_j2k.get("operation") == J2K_OPERATION,
            f"{label}: missing paired J2K timing")
    require(type(paired) is list and all(type(pair) is dict for pair in paired),
            f"{label}: invalid paired_latency")
    operations = [pair["operation"] for pair in paired]
    require(len(operations) == len(set(operations))
            and set(operations) == set(policy["required_operations"]),
            f"{label}: paired_latency operation set mismatch")

    check_identities(report, policy, platform, expected, label, run_ids)
    check_pinned_ffmpeg_runtime(report, platform, label, pinned)
    check_pdfium_attribution(report, platform, label)
    maps = {}
    for name in ("corpus", "j2k"):
        maps[name] = {}
        for side in ("baseline", "candidate"):
            maps[name][side] = timing_map(section_of(report, side, name, label),
                                          f"{label}: {side} {name}")
    check_pairs(paired, maps, "corpus", policy, label)
    check_pairs([paired_j2k], maps, "j2k", policy, label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--expected-sha", required=True)
    parser.add_argument("--policy", type=Path, default=ROOT / "scripts" / "release-parity.json")
    parser.add_argument("reports", nargs=3, metavar=("linux-report", "macos-report", "windows-report"))
    args = parser.parse_args()
    try:
        require(args.repo.is_dir(), "repo directory is required")
        require(SHA_PATTERN.fullmatch(args.expected_sha) is not None,
                "expected SHA must be a full commit SHA")
        policy = load_policy(args.policy)
        pinned = pinned_ffmpeg_identity()
        harness_path = args.repo / "crates" / "thumb-preview" / "examples" / HARNESS
        require(harness_path.is_file(), "repo has no pinned J2K harness")
        harness_digest = hashlib.sha256(harness_path.read_bytes()).hexdigest()
        build_script_path = args.repo / BUILD_SCRIPT
        require(build_script_path.is_file(), "repo has no preview build script")
        build_script_digest = hashlib.sha256(build_script_path.read_bytes()).hexdigest()
        run_ids = set()
        for platform, path in zip(PLATFORMS, args.reports):
            report = load_report(Path(path), policy["max_report_bytes"], platform)
            validate_report(report, path, platform, args.expected_sha, harness_digest,
                            build_script_digest, policy, run_ids, pinned)
        require(len(run_ids) == 1, "reports come from different workflow runs")
    except Exception as error:
        parser.exit(1, f"release parity verification failed: {error}\n")
    print("release parity evidence accepted for linux, macos and windows")


if __name__ == "__main__":
    main()
