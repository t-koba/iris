#!/usr/bin/env python3
"""Offline policy check only; never legal/compliance approval or a package builder."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tomllib

from release_evidence import OBLIGATIONS, inventory, load_json, parse_json, require, strings, text, validate_closure

ROOT = Path(__file__).resolve().parent.parent
AUDIT_RECORD_KINDS = frozenset({
    "build", "cargo-graph", "cargo-metadata-command", "cargo-tree-command",
    "command", "file", "homebrew", "missing", "provenance-check",
    "provenance-match", "scope", "system-dependency", "windows-payload",
    "staged-file", "native-file", "native-closure",
})


def load_policy(path, max_bytes):
    policy = load_json(path, max_bytes)
    require(isinstance(policy, dict) and type(policy.get("schema")) is int
            and policy["schema"] == 1, "unsupported policy schema")
    for key in ("allowed_licenses", "blocked_licenses", "review_licenses",
                "required_cargo_flags", "required_pdfium_attestation", "ffmpeg_components", "forbidden_ffmpeg_flags"):
        require(strings(policy.get(key)), "invalid policy " + key)
    groups = [set(policy[key]) for key in ("allowed_licenses", "blocked_licenses", "review_licenses")]
    require(not any(groups[i] & groups[j] for i in range(3) for j in range(i)),
            "overlapping license decisions")
    require(isinstance(policy.get("allowed_with"), list) and all(
        strings(pair) and len(pair) == 2 and pair[0] in groups[0]
        for pair in policy["allowed_with"]), "invalid WITH policy")
    require(isinstance(policy.get("lgpl_legal_approvals"), list), "invalid legal reviews")
    for review in policy["lgpl_legal_approvals"]:
        require(isinstance(review, dict) and set(review) == {"domain", "subject", "license", "review"}
                and all(text(value) for value in review.values())
                and review["domain"] in ("cargo", "ffmpeg", "native")
                and review["license"] in groups[2], "invalid scoped LGPL legal review")
    require(isinstance(policy.get("required_variants"), dict)
            and set(policy["required_variants"]) == {"linux", "macos", "windows"}
            and all(text(v) for v in policy["required_variants"].values()), "invalid variants")
    require(isinstance(policy.get("ffmpeg_license_names"), dict) and all(
        text(k) and v in groups[2] for k, v in policy["ffmpeg_license_names"].items()),
        "invalid FFmpeg license mapping")
    require(type(policy.get("require_complete_native_license_closure")) is bool,
            "invalid native closure policy")
    exceptions = policy.get("native_license_exceptions")
    require(isinstance(exceptions, list), "invalid native license exceptions")
    for row in exceptions:
        require(isinstance(row, dict) and set(row) == {"license", "name", "platform", "modes",
                                                    "artifact_scope", "system_package", "required_obligations"}
                and text(row["license"]) and re.fullmatch(
                    r"[A-Za-z0-9][A-Za-z0-9.+-]* WITH [A-Za-z0-9][A-Za-z0-9.+-]*", row["license"])
                and text(row["name"]) and "/" not in row["name"] and "\\" not in row["name"]
                and row["platform"] in policy["required_variants"]
                and row["artifact_scope"] == "external" and text(row["system_package"])
                and strings(row["required_obligations"])
                and len(row["required_obligations"]) == len(set(row["required_obligations"]))
                and set(row["required_obligations"]) <= set(OBLIGATIONS)
                and strings(row["modes"]) and row["modes"]
                and len(row["modes"]) == len(set(row["modes"]))
                and set(row["modes"]) <= {"static", "embedded", "dynamic", "dlopen"},
                "invalid scoped native license exception")
    exception_scopes = [(row["license"], row["name"], row["platform"], mode)
                        for row in exceptions for mode in row["modes"]]
    require(len(exception_scopes) == len(set(exception_scopes)),
            "overlapping native license exceptions")
    obligations = policy.get("native_required_obligations")
    require(isinstance(obligations, dict) and set(obligations) == {"allowed", "review"}
            and all(strings(v) and len(v) == len(set(v)) and set(v) <= set(OBLIGATIONS)
                    for v in obligations.values()), "invalid native obligations policy")
    return policy


def reviewed(policy, domain, subject, license_id):
    return any(row["domain"] == domain and row["subject"] == subject
               and row["license"] == license_id for row in policy["lgpl_legal_approvals"])


def spdx(expression, policy, subject):
    """Recursive descent: WITH binds atoms, then AND, then OR. Parse every branch."""
    require(text(expression), "missing third-party license metadata")
    tokens = re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*|[()]|\S", expression)
    position = 0
    known = set().union(*(policy[k] for k in ("allowed_licenses", "blocked_licenses", "review_licenses")))

    def take():
        nonlocal position
        require(position < len(tokens), "incomplete SPDX expression")
        token = tokens[position]
        position += 1
        return token

    def atom():
        token = take()
        if token == "(":
            value = either()
            require(take() == ")", "unbalanced SPDX parentheses")
            return value
        require(token in known, "unknown/unsupported SPDX identifier: " + token)
        value = token in policy["allowed_licenses"] or (
            token in policy["review_licenses"] and reviewed(policy, "cargo", subject, token))
        if position < len(tokens) and tokens[position] == "WITH":
            take()
            exception = take()
            require([token, exception] in policy["allowed_with"], "unsupported SPDX WITH pair")
        return value

    def both():
        value = atom()
        while position < len(tokens) and tokens[position] == "AND":
            take()
            right = atom()
            value = value and right
        return value

    def either():
        value = both()
        while position < len(tokens) and tokens[position] == "OR":
            take()
            right = both()
            value = value or right
        return value

    result = either()
    require(position == len(tokens), "invalid/unsupported SPDX syntax (including legacy slash forms)")
    return result


def workspace_packages():
    """Use the checked-out manifests, not an invented license or an iris-* prefix."""
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
    result = {}
    for member in workspace["members"]:
        package = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
        result[package["name"]] = member
    return result


def cargo_graph(graph, policy, workspace):
    require(graph.get("status") == "captured" and graph.get("truncated") is False,
            "selected Cargo graph absent/partial/truncated")
    require(text(graph.get("target")) and strings(graph.get("cargo_flags")), "invalid Cargo selection")
    packages, edges = graph.get("packages"), graph.get("edges")
    require(isinstance(packages, list) and packages and isinstance(edges, list), "missing Cargo packages/edges")
    by_id = {}
    for package in packages:
        require(isinstance(package, dict), "invalid Cargo package")
        require(all(text(package.get(k)) for k in ("id", "name", "version", "source_kind")), "invalid package identity")
        pid = package["id"]
        require(pid not in by_id and "status" not in package, "duplicate/partial package")
        for key in ("features", "contexts", "target_kinds", "missing_metadata"):
            require(strings(package.get(key)), "missing/invalid package " + key)
        require(package["contexts"] and set(package["contexts"]) <= {"runtime", "build", "proc-macro"}
                and package["target_kinds"], "invalid package contexts/targets")
        require(all(key in package and (package[key] is None or text(package[key]))
                    for key in ("license", "license_file", "source")), "missing package metadata fields")
        source = package["source"]
        require((package["source_kind"] == "path" and source is None) or (
            package["source_kind"] == "registry-or-git" and text(source)
            and source.startswith(("registry+", "git+"))), "unknown package source")
        by_id[pid] = package
    root = graph.get("root")
    require(root in by_id and by_id[root]["name"] == "iris-ui-shell"
            and by_id[root]["source_kind"] == "path", "missing selected Cargo workspace root")
    contexts = {pid: set() for pid in by_id}
    contexts[root].add("runtime")
    outgoing = {pid: [] for pid in by_id}
    seen = set()
    for edge in edges:
        require(isinstance(edge, dict), "invalid Cargo edge")
        src, dst = edge.get("source"), edge.get("destination")
        require(src in by_id and dst in by_id and (src, dst) not in seen, "dangling/duplicate Cargo edge")
        seen.add((src, dst))
        kinds = edge.get("dependency_kinds")
        require(strings(kinds) and len(kinds) == len(set(kinds)) and set(kinds) in ({"normal"}, {"build"}, {"normal", "build"})
                and edge.get("dependency_kind") == (kinds[0] if len(kinds) == 1 else "ambiguous")
                and type(edge.get("proc_macro")) is bool
                and edge["proc_macro"] == ("proc-macro" in by_id[dst]["target_kinds"]), "invalid Cargo edge kinds")
        outgoing[src].append(edge)
    pending = [(root, "runtime")]
    for pid, usage in pending:
        for edge in outgoing[pid]:
            roles = {"proc-macro"} if edge["proc_macro"] else (
                ({"runtime" if usage == "runtime" else "build"} if "normal" in edge["dependency_kinds"] else set())
                | ({"build"} if "build" in edge["dependency_kinds"] else set()))
            dst = edge["destination"]
            for role in roles - contexts[dst]:
                contexts[dst].add(role)
                pending.append((dst, role))
    errors = []
    for pid, package in by_id.items():
        require(contexts[pid] and contexts[pid] == set(package["contexts"]), "unreachable package or inconsistent Cargo contexts")
        if "runtime" not in contexts[pid]:
            continue
        if package["source_kind"] == "path":
            member = workspace.get(package["name"])
            require(member is not None and pid == f'path+$WORKSPACE/{member}#{package["name"]}@{package["version"]}',
                    "path package not established as an Iris workspace member")
            continue
        try:
            require(spdx(package["license"], policy, pid), "runtime license blocked or LGPL legal review pending")
        except ValueError as error:
            errors.append(package["name"] + ": " + str(error))
    return errors


def native(records, policy):
    errors = []
    files = {row.get("path"): row for row in records if row["kind"] == "file"}
    probes = {}
    for row in records:
        argv = row.get("argv", [])
        if row["kind"] != "command" or "probe-library" not in argv:
            continue
        try:
            require(len(argv) == 4 and argv[2] == "probe-library" and argv[1].replace("\\", "/").endswith("/release-audit.py")
                    and type(row.get("status")) is int and row["status"] == 0 and row.get("truncated") is False, "failed/invalid exact-library probe")
            result = parse_json(row.get("text", ""))
            path = argv[3]
            require(isinstance(result, dict) and result.get("library") == path and path in files
                    and re.fullmatch(r"[0-9a-f]{64}", files[path].get("sha256", "")), "probe lacks exact file/hash binding")
            match = re.match(r"lib(avcodec|avformat|avutil|avdevice|avfilter|swscale|swresample)[.-]", path.rsplit("/", 1)[-1])
            require(match is not None and path not in probes, "unknown/duplicate exact library")
            component = match[1]
            probes[path] = component
            components = {key.rsplit("_", 1)[0] for key in result
                          if key.endswith(("_configuration", "_license"))} | {component}
            for prefix in components:
                configuration, license_name = result.get(prefix + "_configuration"), result.get(prefix + "_license")
                require(text(configuration) and text(license_name), "absent native license/configuration evidence")
                flags = {token.split("=", 1)[0] for token in shlex.split(configuration)}
                require(not flags.intersection(policy["forbidden_ffmpeg_flags"]), "forbidden FFmpeg GPL/version3/nonfree flags")
                license_id = policy["ffmpeg_license_names"].get(license_name)
                require(license_id is not None, "unknown/blocked native FFmpeg license")
                subject = path + "#sha256=" + files[path]["sha256"]
                require(reviewed(policy, "ffmpeg", subject, license_id), "FFmpeg LGPL legal review pending")
        except (ValueError, TypeError) as error:
            errors.append("FFmpeg: " + str(error))
    expected = {path for path in files if isinstance(path, str) and re.search(
        r"(?:^|/)lib(?:avcodec|avformat|avutil|avdevice|avfilter|swscale|swresample)[.-]", path)}
    if expected - probes.keys() or set(policy["ffmpeg_components"]) - set(probes.values()):
        errors.append("FFmpeg: absent exact-library native license/configuration evidence")
    return errors


def validate(report, policy, workspace):
    require(isinstance(report, dict) and type(report.get("schema")) is int and report["schema"] in (1, 2),
            "unsupported audit schema")
    platform = report.get("platform")
    require(platform in policy["required_variants"], "unknown audit platform")
    require(type(report.get("omitted_records")) is int and report["omitted_records"] == 0, "omitted audit records")
    limits = report.get("limits")
    require(isinstance(limits, dict) and limits.get("platform") == platform and all(
        type(limits.get(key)) is int and limits[key] > 0 for key in (
            "max_report_bytes", "max_command_bytes", "max_metadata_bytes", "max_cargo_packages",
            "max_native_files", "command_seconds")), "invalid/missing audit limits")
    records = report.get("records")
    require(isinstance(records, list) and records and all(
        isinstance(row, dict) and text(row.get("kind")) and row["kind"] in AUDIT_RECORD_KINDS
        for row in records), "invalid/unknown audit record kind")
    file_paths = []
    for row in records:
        for key in ("truncated", "stderr_truncated"):
            require(key not in row or row[key] is False, "truncated/invalid audit evidence")
        require(row["kind"] != "missing", "audit has missing evidence")
        if row["kind"] == "file":
            require(text(row.get("path")) and type(row.get("bytes")) is int and row["bytes"] >= 0
                    and isinstance(row.get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", row["sha256"]),
                    "invalid file evidence")
            file_paths.append(row["path"])
        if row["kind"] == "command":
            require(strings(row.get("argv")) and row["argv"] and "status" in row
                    and isinstance(row.get("text"), str) and row.get("truncated") is False,
                    "invalid command evidence")
    require(len(file_paths) == len(set(file_paths)), "duplicate file evidence")
    def one(kind):
        rows = [row for row in records if row["kind"] == kind]
        require(len(rows) == 1, "missing/duplicate " + kind)
        return rows[0]
    graph, build = one("cargo-graph"), one("build")
    errors = []
    variant = policy["required_variants"][platform]
    if (build.get("variant") != variant or graph.get("variant") != variant
            or graph.get("cargo_flags") != policy["required_cargo_flags"]
            or build.get("cargo_flags", policy["required_cargo_flags"]) != policy["required_cargo_flags"]):
        errors.append(platform + ": required build/graph variant " + variant)
    for kind in ("cargo-metadata-command", "cargo-tree-command"):
        command = one(kind)
        require(type(command.get("status")) is int and command["status"] == 0
                and command.get("truncated") is False and command.get("stderr_truncated") is False,
                "incomplete " + kind)
    errors.extend(cargo_graph(graph, policy, workspace))
    if platform in policy["required_pdfium_attestation"]:
        rows = [row for row in records if row["kind"] == "provenance-check" and row.get("subject") == "PDFium"]
        if len(rows) != 1 or rows[0].get("attestation_verified") is not True:
            errors.append("Linux PDFium: verified provenance-check attestation required")
    errors.extend(native(records, policy))
    closures = [row for row in records if row["kind"] == "native-closure"]
    if policy["require_complete_native_license_closure"] and report["schema"] == 1 and not closures:
        errors.append("schema 1 cannot establish complete native license closure without schema-2 evidence")
        return errors
    if policy["require_complete_native_license_closure"] or closures:
        require(report["schema"] == 2 and len(closures) == 1,
                "complete native license closure requires one schema-2 evidence record")
        require(type(limits.get("max_native_components")) is int and limits["max_native_components"] > 0,
                "invalid native component bound")
        components = validate_closure(closures[0].get("evidence"), records,
                                      limits["max_native_files"], limits["max_native_components"])
        staged, native_files = inventory(records, "staged-file"), inventory(records, "native-file")
        for component in components:
            license_id = component["license"]["id"]
            disposition = component["license"]["disposition"]
            group = "allowed_licenses" if disposition == "allowed" else "review_licenses"
            artifact = component["artifact"]
            # OS runtime exceptions never cover shipped files or ambiguous package ownership.
            system_owners = [owner for owner in records
                             if owner["kind"] == "system-dependency" and owner.get("path") == artifact["path"]]
            exception = next((row for row in policy["native_license_exceptions"]
                if disposition == "allowed" and row["license"] == license_id
                and row["name"] == component["name"] == artifact["path"].rsplit("/", 1)[-1]
                and row["platform"] == platform and component["mode"] in row["modes"]
                and row["artifact_scope"] == "external" and artifact["path"] not in staged
                and native_files.get(artifact["path"]) == artifact["sha256"]
                and len(system_owners) == 1 and system_owners[0].get("platform") == platform
                and system_owners[0].get("sha256") == artifact["sha256"]
                and system_owners[0].get("package") == row["system_package"]
                and isinstance(system_owners[0].get("owner"), str) and re.fullmatch(
                    re.escape(row["system_package"]) + r"(?::[a-z0-9][a-z0-9-]*)?", system_owners[0]["owner"])), None)
            require(license_id in policy[group] or exception is not None,
                    "blocked/unknown native license or mismatched disposition")
            subject = artifact["path"] + "#sha256=" + artifact["sha256"] + "#component=" + component["id"]
            require(disposition != "review" or reviewed(policy, "native", subject, license_id),
                    "native LGPL legal review pending")
            required = (exception["required_obligations"] if exception is not None
                        else policy["native_required_obligations"][disposition])
            require(all(component["obligations"][key]["status"] == "provided" for key in required),
                    "required native notice/source/relinking evidence absent")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--policy", type=Path, default=ROOT / "scripts/release-policy.json")
    parser.add_argument("--max-report-bytes", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES", "2097152"))
    parser.add_argument("--max-policy-bytes", type=int,
                        default=os.environ.get("IRIS_RELEASE_POLICY_MAX_BYTES", "65536"))
    parser.add_argument("reports", type=Path, nargs=3, help="one report for each release platform")
    args = parser.parse_args()
    if min(args.max_report_bytes, args.max_policy_bytes) < 1:
        parser.error("byte limits must be positive")
    errors = []
    try:
        policy, workspace = load_policy(args.policy, args.max_policy_bytes), workspace_packages()
        platforms = []
        for path in args.reports:
            try:
                report = load_json(path, args.max_report_bytes)
                errors.extend(f"{path.name}: {error}" for error in validate(report, policy, workspace))
                platforms.append(report["platform"])
            except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
                errors.append(f"{path.name}: invalid/incomplete report: {error}")
        require(sorted(platforms) == sorted(policy["required_variants"]), "exactly one valid report for each release platform required")
    except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
        errors.append(str(error))
    print("policy check only — not legal/compliance approval: " + ("BLOCKED" if errors else "PASS"))
    for error in errors:
        print("- " + error)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
