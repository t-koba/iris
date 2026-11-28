#!/usr/bin/env python3
"""Offline release gate for same-source paired parity evidence.

Consumes three bounded reports from scripts/parity_source_compare.py, one per
platform, and verifies source/revision/harness binding, strict identity, the
declared (B/C, C/B) x4 round alternation, recomputed statistics from 24
chronological samples per side with eight block medians, and a drift-invariant
magnitude-aware t test over four balanced round-pair sums. A candidate fails
only when the mean of the four drift-free estimates is positive, exceeds 2%
of the baseline median, with
t = effect/(sd/2) at or above the Bonferroni-calibrated df=3 threshold;
magnitude matters and monotone runner drift cancels in each opposite-order
pair sum; sub-2% shifts are treated as runner noise. The pooled median delta
is reported, not used as a gate. This
verifier makes no parity claim of its own, runs no builds and writes nothing.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from parity_source_compare import (  # noqa: E402
    BASELINE_SHA, BLOCK_SAMPLES, BUILD_SCRIPT, FIXTURE, FIXTURE_SHA256, FIXTURE_SIZE,
    HARNESS, IDENTITY_SCHEMA, J2K_OPERATION, PARITY_CLAIM, PREVIEW_OPERATIONS, ROUNDS,
    SAMPLES_PER_SIDE, WARMUPS_PER_BLOCK, WINDOWS_FFMPEG_FORBIDDEN_FLAGS,
    WINDOWS_FFMPEG_REQUIRED_FLAGS, ffmpeg_configuration, measurement_rounds,
    pairing_plan, percentile, pinned_ffmpeg_identity, report_path_equivalent,
    report_path_parts, require, timing_statistics, validate_exported_section,
)
from product_build_profile import load as load_product_profiles  # noqa: E402

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
    "required_operations", "warmups", "samples", "rounds", "alpha",
    "significance_family_size", "t_critical_df3", "min_relative_effect",
    "policy_rationale", "max_report_bytes",
}
SHA_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
MAX_POLICY_BYTES = 65536


def exact_int(value, expected, label):
    require(type(value) is int and value == expected, f"invalid policy {label}")


def student_t_survival_df3(value):
    """One-sided P[T3 >= value] for Student t with 3 degrees of freedom."""
    require(type(value) in (int, float) and value >= 0, "invalid t value")
    if isinstance(value, bool):
        raise ValueError("invalid t value")
    root3 = math.sqrt(3.0)
    return 0.5 - (math.atan(value / root3) + value * root3 / (value * value + 3.0)) / math.pi


def t_critical_df3(alpha, family_size):
    """Smallest t with survival <= alpha/family_size, found by bisection."""
    require(type(alpha) is float and 0 < alpha < 1
            and type(family_size) is int and family_size > 0,
            "invalid t policy inputs")
    target = alpha / family_size
    low, high = 0.0, 1.0
    while student_t_survival_df3(high) > target:
        high *= 2.0
        require(high < 1e6, "t critical search did not converge")
    for _ in range(100):
        mid = (low + high) / 2.0
        if student_t_survival_df3(mid) <= target:
            high = mid
        else:
            low = mid
    return high


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
    exact_int(policy["warmups"], WARMUPS_PER_BLOCK, "warmups")
    exact_int(policy["samples"], SAMPLES_PER_SIDE, "samples")
    exact_int(policy["rounds"], ROUNDS, "rounds")
    require(ROUNDS * BLOCK_SAMPLES == SAMPLES_PER_SIDE, "invalid paired sample budget")
    require(type(policy["alpha"]) is float and policy["alpha"] == 0.05,
            "invalid policy alpha")
    exact_int(policy["significance_family_size"], 18, "significance_family_size")
    require(type(policy["t_critical_df3"]) is float and policy["t_critical_df3"] > 0,
            "invalid policy t_critical_df3")
    expected_critical = t_critical_df3(policy["alpha"],
                                       policy["significance_family_size"])
    require(abs(policy["t_critical_df3"] - expected_critical) < 1e-9,
            "invalid policy t_critical_df3")
    require(type(policy["min_relative_effect"]) is float
            and policy["min_relative_effect"] == 0.02,
            "invalid policy min_relative_effect")
    require(type(policy["policy_rationale"]) is str and policy["policy_rationale"].strip(),
            "invalid policy policy_rationale")
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


def check_identities(report, policy, platform, expected, label, run_ids, product_profile):
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
                require(profile == product_profile["build_profile"]
                        and level == product_profile["release_opt_level"],
                        f"{label}: {side} {name} wrong product build profile")
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
    """Non-macOS corpus timings must name the exact verified staged library."""
    if platform == "macos":
        return
    filename = {"linux": "libpdfium.so", "windows": "pdfium.dll"}[platform]
    for side in ("baseline", "candidate"):
        runtime = section_of(report, side, "corpus", label).get("pdfium_runtime")
        require(type(runtime) is dict and type(runtime.get("path")) is str
                and runtime["path"].strip(),
                f"{label}: {side} corpus has no loaded PDFium library attribution")
        require(runtime.get("required") is True,
                f"{label}: {side} corpus did not require the staged PDFium library")
        parent, name = report_path_parts(runtime["path"], platform)
        require(name == filename,
                f"{label}: {side} corpus loaded an unexpected PDFium filename")
        side_report = report.get(side)
        worktree = side_report.get("worktree") if type(side_report) is dict else None
        require(type(worktree) is str and worktree.strip(),
                f"{label}: {side} worktree identity is missing")
        require(report_path_equivalent(parent, worktree, platform),
                f"{label}: {side} corpus PDFium path is not the staged worktree library")


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
    t_critical = policy["t_critical_df3"]
    for pair in pairs:
        operation = pair.get("operation")
        require(type(operation) is str
                and all(operation in maps[section_name][side] for side in ("baseline", "candidate")),
                f"{label}: {section_name} pair has no source timing for {operation}")
        require("paired_differences_ns" not in pair and "positive_differences" not in pair,
                f"{label}: {section_name} {operation} stale sign-test fields present")
        baseline = pair.get("baseline_median_ns")
        candidate = pair.get("candidate_median_ns")
        delta = pair.get("candidate_minus_baseline_ns")
        require(type(baseline) is int and baseline > 0,
                f"{label}: {section_name} {operation} invalid baseline median")
        require(type(candidate) is int and candidate >= 0,
                f"{label}: {section_name} {operation} invalid candidate median")
        require(type(delta) is int and delta == candidate - baseline,
                f"{label}: {section_name} {operation} median delta mismatch")
        medians = {}
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
            require(type(timing.get("rounds")) is int
                    and timing["rounds"] == policy["rounds"],
                    f"{label}: {side} {operation} round count mismatch")
            values = timing.get("samples_ns")
            require(type(values) is list and len(values) == policy["samples"]
                    and all(type(value) is int and value >= 0 for value in values),
                    f"{label}: {side} {operation} invalid chronological samples")
            blocks = timing.get("round_medians_ns")
            require(type(blocks) is list and len(blocks) == policy["rounds"]
                    and all(type(value) is int and value >= 0 for value in blocks),
                    f"{label}: {side} {operation} invalid round medians")
            for index in range(policy["rounds"]):
                chunk = values[index * BLOCK_SAMPLES:(index + 1) * BLOCK_SAMPLES]
                require(len(chunk) == BLOCK_SAMPLES,
                        f"{label}: {side} {operation} invalid round block size")
                require(blocks[index] == percentile(sorted(chunk), 50),
                        f"{label}: {side} {operation} round median mismatch")
            for key, value in timing_statistics(values, f"{label}: {side} {operation}").items():
                require(timing.get(key) == value,
                        f"{label}: {side} {operation} recomputed {key} mismatch")
            require(timing.get("median") == median,
                    f"{label}: {side} {operation} paired median does not match source")
            medians[side] = blocks
        expected_deltas = [candidate - baseline
                           for baseline, candidate in zip(medians["baseline"],
                                                          medians["candidate"])]
        require(pair.get("round_deltas_ns") == expected_deltas,
                f"{label}: {section_name} {operation} round deltas mismatch")
        expected_balanced = [expected_deltas[index] + expected_deltas[index + 1]
                             for index in (0, 2, 4, 6)]
        require(pair.get("balanced_delta_sums_ns") == expected_balanced,
                f"{label}: {section_name} {operation} balanced sums mismatch")
        balanced = pair.get("balanced_delta_sums_ns")
        require(type(balanced) is list and len(balanced) == 4
                and all(type(value) is int for value in balanced),
                f"{label}: {section_name} {operation} invalid balanced sums")
        effect = sum(balanced) / 8.0
        if effect <= 0:
            continue
        floor = policy["min_relative_effect"] * baseline
        if effect <= floor:
            continue
        estimates = [value / 2.0 for value in balanced]
        mean = effect
        if all(estimate == mean for estimate in estimates):
            raise ValueError(f"{label}: {section_name} {operation} paired candidate slowdown: "
                             f"effect {effect:.3f}ns with zero variance across 4 estimates")
        variance = sum((estimate - mean) ** 2 for estimate in estimates) / 3.0
        deviation = math.sqrt(variance)
        statistic = effect / (deviation / 2.0)
        require(statistic < t_critical,
                f"{label}: {section_name} {operation} paired candidate slowdown: "
                f"effect {effect:.3f}ns t {statistic:.3f} >= {t_critical:.3f}")


def validate_report(report, path, platform, expected, harness_digest, build_script_digest,
                   policy, run_ids, pinned, product_profile):
    label = f"{platform} report {path}"
    require(report.get("schema") == REPORT_SCHEMA, f"{label}: unexpected schema")
    require(report.get("parity_claim") == PARITY_CLAIM, f"{label}: unexpected parity_claim")
    require(report.get("measurement_rounds") == measurement_rounds(),
            f"{label}: measurement round alternation mismatch")
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

    for side in ('baseline', 'candidate'):
        for name, operations in (('corpus', policy['required_operations']),
                                 ('j2k', (J2K_OPERATION,))):
            validate_exported_section(section_of(report, side, name, label), operations,
                                      f'{label}: {side} {name}')

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

    check_identities(report, policy, platform, expected, label, run_ids, product_profile)
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
        product_profiles = load_product_profiles(
            ROOT / "scripts" / "product-build-profile.json")
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
                            build_script_digest, policy, run_ids, pinned,
                            product_profiles[platform])
        require(len(run_ids) == 1, "reports come from different workflow runs")
    except Exception as error:
        parser.exit(1, f"release parity verification failed: {error}\n")
    print("release parity evidence accepted for linux, macos and windows")


if __name__ == "__main__":
    main()
