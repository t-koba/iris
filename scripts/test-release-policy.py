#!/usr/bin/env python3
"""Bounded offline synthetic tests: no Cargo build, native load or network."""
import argparse
import copy
import hashlib
import os
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release_policy", ROOT / "scripts/release-policy.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
audit_spec = importlib.util.spec_from_file_location("release_audit", ROOT / "scripts/release-audit.py")
audit_module = importlib.util.module_from_spec(audit_spec)
audit_spec.loader.exec_module(audit_module)
POLICY = module.load_policy(ROOT / "scripts/release-policy.json", 65536)
WORKSPACE = module.workspace_packages()


def report(platform="linux", license_id="MIT OR Apache-2.0"):
    def package(pid, name, source, contexts, license_id):
        return dict(id=pid, name=name, version="0.4.0", source=source,
                    source_kind="path" if source is None else "registry-or-git",
                    features=[], contexts=contexts, target_kinds=["lib"],
                    license=license_id, license_file=None,
                    missing_metadata=["license_file"] + (["license"] if license_id is None else []))
    root = "path+$WORKSPACE/crates/ui-shell#iris-ui-shell@0.4.0"
    packages = [package(root, "iris-ui-shell", None, ["runtime"], None),
                package("registry+example#third@0.4.0", "third", "registry+example", ["runtime"], license_id)]
    graph = dict(kind="cargo-graph", status="captured", variant="product", cargo_flags=[],
                 target="x86_64-unknown-linux-gnu", root=root, truncated=False, packages=packages,
                 edges=[dict(source=root, destination=packages[1]["id"], name="third",
                             dependency_kind="normal", dependency_kinds=["normal"], proc_macro=False)])
    records = [dict(kind=kind, status=0, truncated=False, stderr_truncated=False)
               for kind in ("cargo-metadata-command", "cargo-tree-command")]
    records += [graph, dict(kind="build", variant="product"),
                dict(kind="provenance-check", subject="PDFium", attestation_verified=True)]
    for component in POLICY["ffmpeg_components"]:
        path = f"/native/lib{component}.so.1"
        records += [dict(kind="file", path=path, bytes=12, sha256="a" * 64),
                    dict(kind="command", argv=["python", "$WORKSPACE/scripts/release-audit.py", "probe-library", path],
                         status=0, truncated=False, text=json.dumps(dict(
                             library=path, **{component + "_license": "LGPL version 2.1 or later",
                                              component + "_configuration": "--enable-shared"})))]
    return dict(schema=1, platform=platform, omitted_records=0,
                limits=dict(platform=platform, max_report_bytes=2097152, max_command_bytes=32768,
                            max_metadata_bytes=8388608, max_cargo_packages=1024,
                            max_native_files=256, command_seconds=30), records=records)


def graph(value):
    return next(row for row in value["records"] if row["kind"] == "cargo-graph")


def component_by_id(value, cid):
    return next(c for c in value["records"][-1]["evidence"]["components"] if c["id"] == cid)


def set_component_mode(value, cid, mode):
    """Move a synthetic component between closure coverage modes."""
    component = component_by_id(value, cid)
    coverage = next(a["coverage"] for a in value["records"][-1]["evidence"]["artifacts"]
                    if a["file"] == component["artifact"])
    coverage[component["mode"]]["components"].remove(cid)
    component["mode"] = mode
    coverage[mode]["components"].append(cid)
    return component


def scoped_native_review(value, policy, cid):
    """Synthetic scoped entry for static/embedded LGPL; production policy has none."""
    component = component_by_id(value, cid)
    ref = component["artifact"]
    review = dict(domain="native",
                  subject=ref["path"] + "#sha256=" + ref["sha256"] + "#component=" + cid,
                  license=component["license"]["id"],
                  review="synthetic test review; NOT production approval")
    policy["lgpl_legal_approvals"].append(review)
    return review


def complete_report(platform="linux"):
    """Wholly synthetic closure: no production artifacts, licenses or approval claims."""
    value = report(platform)
    value["schema"] = 2
    value["limits"]["max_native_components"] = 4096
    proof = dict(path="/stage/SYNTHETIC-EVIDENCE.txt", sha256="b" * 64)
    files = [row for row in value["records"] if row["kind"] == "file"]
    value["records"].append(dict(kind="staged-file", bytes=100, **proof))
    for row in files:
        value["records"].append(dict(row, kind="native-file"))
    artifacts, components = [], []
    for index, file in enumerate(files + [proof]):
        ref = {key: file[key] for key in ("path", "sha256")}
        coverage = {mode: dict(components=[], evidence=[proof])
                    for mode in ("static", "embedded", "dynamic", "dlopen")}
        artifacts.append(dict(file=ref, coverage=coverage))
        if file is proof:
            continue
        for mode in coverage if index == 0 else ("dynamic",):
            cid = str(index) + "-" + mode
            coverage[mode]["components"].append(cid)
            components.append(dict(id=cid, artifact=ref, mode=mode, name="synthetic-" + cid,
                source=dict(origin="https://example.invalid/synthetic", revision="test-revision", evidence=[proof]),
                modifications=dict(status="modified" if mode == "embedded" else "unmodified", evidence=[proof]),
                license=dict(id="LGPL-2.1-or-later" if mode == "dynamic" else "MIT",
                             disposition="review" if mode == "dynamic" else "allowed", evidence=[proof]),
                obligations={key: dict(status="provided", evidence=[proof])
                             for key in ("notice", "source", "relinking")}))
    value["records"].append(dict(kind="native-closure", evidence=dict(schema=1, artifacts=artifacts, components=components)))
    return value


def gcc_runtime(value, mode="dynamic"):
    """Synthetic import of the OS runtime, not production license evidence."""
    closure = value["records"][-1]["evidence"]
    component = copy.deepcopy(next(c for c in closure["components"] if c["mode"] == "dynamic"))
    ref = dict(path="/usr/lib/synthetic/libgcc_s.so.1", sha256="c" * 64)
    copyright_ref = dict(path="/usr/share/doc/libgcc-s1/copyright", sha256="d" * 64)
    value["records"][-1:-1] = [dict(kind=kind, bytes=12, **ref) for kind in ("file", "native-file")] + [
        dict(kind="system-dependency", platform="linux", package="libgcc-s1", owner="libgcc-s1:amd64", **ref),
        dict(kind="file", bytes=100, **copyright_ref)]
    coverage = {key: dict(components=["gcc-runtime"] if key == mode else [],
                          evidence=copy.deepcopy(component["license"]["evidence"]))
                for key in ("static", "embedded", "dynamic", "dlopen")}
    closure["artifacts"].append(dict(file=ref, coverage=coverage))
    component.update(id="gcc-runtime", artifact=ref, mode=mode, name="libgcc_s.so.1")
    component["license"].update(id="GPL-3.0-or-later WITH GCC-exception-3.1", disposition="allowed",
                                evidence=[copyright_ref])
    for obligation in component["obligations"].values():
        # The staged synthetic rationale documents external OS provision, not redistribution.
        obligation["status"] = "not-applicable"
    closure["components"].append(component)
    return component


class Policy(unittest.TestCase):
    def test_spdx_precedence_alternatives_and_full_parse(self):
        for expression, expected in (
            ("MIT OR Apache-2.0", True), ("MIT AND Apache-2.0", True),
            ("Unlicense", True), ("WTFPL", True),
            ("GPL-3.0-only OR MIT", True), ("GPL-2.0+ OR MIT", True), ("MIT OR GPL-3.0-only", True),
            ("MIT AND GPL-3.0-only", False), ("AGPL-3.0-or-later", False),
            ("GPL-3.0-only OR MIT AND GPL-2.0-only", False),
            ("(GPL-3.0-only OR MIT) AND Apache-2.0", True),
            ("Apache-2.0 WITH LLVM-exception OR MIT", True),
            ("LGPL-2.1-or-later", False), ("MIT AND LGPL-2.1-or-later", False)):
            with self.subTest(expression=expression):
                self.assertEqual(module.spdx(expression, POLICY, "third"), expected)
        for expression in (None, "", "MIT/Apache-2.0", "MIT OR Unknown", "Unknown OR MIT",
                           "MIT OR", "MIT AND ()", "(MIT", "MIT)", "MIT Apache-2.0",
                           "MIT WITH Unknown-exception", "(MIT OR Apache-2.0) WITH LLVM-exception",
                           "MIT or Apache-2.0", "MIT OR LicenseRef-private"):
            with self.subTest(expression=expression), self.assertRaises(ValueError):
                module.spdx(expression, POLICY, "third")

    def test_scoped_lgpl_review(self):
        policy = copy.deepcopy(POLICY)
        policy["lgpl_legal_approvals"] = [dict(domain="cargo", subject="third", license="LGPL-2.1-or-later", review="test only")]
        self.assertTrue(module.spdx("LGPL-2.1-or-later", policy, "third"))
        self.assertFalse(module.spdx("LGPL-2.1-or-later", policy, "other"))
        self.assertFalse(module.spdx("GPL-3.0-only", policy, "third"))
        # Cargo LGPL has no evidence of absent contamination: no entry, no release.
        self.assertFalse(module.spdx("LGPL-2.1-or-later", POLICY, "third"))
        value = report(license_id="LGPL-2.1-or-later")
        self.assertIn("LGPL legal review pending", " ".join(module.cargo_graph(graph(value), POLICY, WORKSPACE)))

    def test_runtime_build_macro_and_workspace(self):
        value = report()
        selected = graph(value)
        self.assertEqual(module.cargo_graph(selected, POLICY, WORKSPACE), [])
        third = selected["packages"][1]
        for source in ("registry+example", "git+https://example.invalid/repo#rev"):
            third["source"] = source
            third["license"] = None
            third["license_file"] = "LICENSE"  # A filename is not license expression evidence.
            self.assertIn("missing third-party", " ".join(module.cargo_graph(selected, POLICY, WORKSPACE)))
        third["license"] = "GPL-3.0-only"
        edge = selected["edges"][0]
        for contexts, kinds, macro in ((["build"], ["build"], False), (["proc-macro"], ["normal"], True)):
            third["contexts"], third["target_kinds"] = contexts, ["proc-macro" if macro else "lib"]
            edge.update(dependency_kinds=kinds, dependency_kind=kinds[0], proc_macro=macro)
            self.assertEqual(module.cargo_graph(selected, POLICY, WORKSPACE), [])
        third.update(contexts=["build", "runtime"], target_kinds=["lib"])
        edge.update(dependency_kinds=["build", "normal"], dependency_kind="ambiguous", proc_macro=False)
        self.assertIn("runtime license blocked", " ".join(module.cargo_graph(selected, POLICY, WORKSPACE)))
        third.update(source=None, source_kind="path", license=None)
        with self.assertRaisesRegex(ValueError, "not established"):
            module.cargo_graph(selected, POLICY, WORKSPACE)

    def test_missing_partial_and_malformed_reports(self):
        mutations = [lambda r: r.update(schema=3), lambda r: r.update(omitted_records=1),
                     lambda r: r.update(omitted_records=False), lambda r: r.update(limits={}),
                     lambda r: r.update(records=[]), lambda r: r["records"].append({"kind": "missing"}),
                     lambda r: r["records"].append({"kind": "future-record"}),
                     lambda r: r["records"].append(dict(next(row for row in r["records"] if row["kind"] == "file"), sha256="0" * 64)),
                     lambda r: r["records"].append(copy.deepcopy(graph(r))),
                     lambda r: graph(r).update(status="absent"), lambda r: graph(r).update(status="partial"),
                     lambda r: graph(r).update(truncated=True), lambda r: graph(r).pop("packages"),
                     lambda r: graph(r)["packages"][1].pop("license"),
                     lambda r: graph(r)["packages"][1].pop("contexts"),
                     lambda r: graph(r)["packages"][1].update(contexts=["build"]),
                     lambda r: graph(r)["edges"].clear(),
                     lambda r: graph(r)["edges"][0].update(destination="omitted"),
                     lambda r: r["records"][0].update(status=1),
                     lambda r: r["records"][0].update(stderr_truncated=True)]
        for mutate in mutations:
            value = report()
            mutate(value)
            with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                module.validate(value, POLICY, WORKSPACE)

    def test_native_flags_unknown_and_exact_probe_binding(self):
        value, policy = report(), copy.deepcopy(POLICY)
        # FFmpeg LGPL needs no attestation; the missing schema-2 closure still blocks.
        self.assertEqual(module.native(value["records"], policy), [])
        errors = module.validate(value, policy, WORKSPACE)
        self.assertEqual(len(errors), 1)
        self.assertIn("schema 1 cannot establish complete native license closure", errors[0])
        baseline = copy.deepcopy(value)
        probe_index = next(i for i, row in enumerate(value["records"]) if row["kind"] == "command")
        for configuration, license_name, expected in (
            ("--enable-gpl", "LGPL version 2.1 or later", "forbidden"),
            ("--enable-version3", "LGPL version 2.1 or later", "forbidden"),
            ("--enable-nonfree=yes", "LGPL version 2.1 or later", "forbidden"),
            ("--enable-shared", "GPL version 3 or later", "unknown/blocked"),
            ("--enable-shared", "LGPL version 3 or later", "unknown/blocked"),
            ("--enable-shared", "MIT", "unknown/blocked"),
            ("", "LGPL version 2.1 or later", "absent native")):
            value = copy.deepcopy(baseline)
            row = value["records"][probe_index]
            data = json.loads(row["text"])
            data.update(avcodec_configuration=configuration, avcodec_license=license_name)
            row["text"] = json.dumps(data)
            self.assertIn(expected, " ".join(module.native(value["records"], policy)))
        for mutation in (lambda row: row.update(status=1), lambda row: row.update(truncated=True),
                         lambda row: row.update(text="{}"), lambda row: row["argv"].__setitem__(1, "/arbitrary/ffmpeg"),
                         lambda row: row["argv"].__setitem__(3, "/other/libavcodec.so.1")):
            value = copy.deepcopy(baseline)
            mutation(value["records"][probe_index])
            self.assertTrue(any("FFmpeg:" in error for error in module.native(value["records"], policy)))
        value = copy.deepcopy(baseline)
        row = value["records"][probe_index]
        data = json.loads(row["text"])
        data.update(avutil_license="LGPL version 2.1 or later", avutil_configuration="--enable-gpl")
        row["text"] = json.dumps(data)
        self.assertIn("forbidden", " ".join(module.native(value["records"], policy)))
        value = copy.deepcopy(baseline)
        value["records"] = [row for row in value["records"] if row["kind"] != "command"]
        self.assertIn("absent exact-library", " ".join(module.native(value["records"], policy)))

    def test_lgpl_acceptance_requires_dynamic_linkage_and_complete_evidence(self):
        """No attestation is available: only absent contamination plus evidence passes."""
        policy = copy.deepcopy(POLICY)
        self.assertEqual(policy["lgpl_legal_approvals"], [])
        for mode in ("dynamic", "dlopen"):
            value = complete_report()
            if mode != "dynamic":
                set_component_mode(value, "0-dynamic", mode)
            with self.subTest(mode=mode):
                self.assertEqual(module.validate(value, policy, WORKSPACE), [])
        for mode in ("static", "embedded"):
            value = complete_report()
            set_component_mode(value, "0-dynamic", mode)
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, "review pending"):
                module.validate(value, policy, WORKSPACE)
            # Static/embedded linkage is still the scoped-review domain, not an
            # attestation produced by this checker.
            scoped = copy.deepcopy(policy)
            scoped_native_review(value, scoped, "0-dynamic")
            self.assertEqual(module.validate(value, scoped, WORKSPACE), [])
        for obligation in POLICY["native_required_obligations"]["review"]:
            value = complete_report()
            component_by_id(value, "0-dynamic")["obligations"][obligation]["status"] = "not-applicable"
            with self.subTest(obligation=obligation), self.assertRaisesRegex(ValueError, "evidence absent"):
                module.validate(value, policy, WORKSPACE)
        # The remaining FFmpeg checks still block an otherwise complete report.
        value = complete_report()
        probe = next(row for row in value["records"] if row["kind"] == "command")
        data = json.loads(probe["text"])
        data["avcodec_configuration"] = "--enable-gpl"
        probe["text"] = json.dumps(data)
        self.assertIn("forbidden", " ".join(module.validate(value, policy, WORKSPACE)))

    def test_pdfium_and_windows_minimal(self):
        for verified in (False, None, "true", 1):
            value = report()
            value["records"][4]["attestation_verified"] = verified
            self.assertIn("PDFium", " ".join(module.validate(value, POLICY, WORKSPACE)))
        value = report("windows")
        graph(value)["variant"] = "minimal"
        value["records"][3]["variant"] = "minimal"
        self.assertIn("windows: required build/graph variant product", module.validate(value, POLICY, WORKSPACE))
        graph(value)["variant"] = value["records"][3]["variant"] = "product"
        graph(value)["cargo_flags"] = ["--no-default-features", "--features", "minimal"]
        self.assertIn("windows: required build/graph variant product", module.validate(value, POLICY, WORKSPACE))

    def test_complete_native_report_and_cli_pass_without_disabling_policy(self):
        policy = copy.deepcopy(POLICY)
        self.assertEqual(policy["lgpl_legal_approvals"], [])
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for platform in POLICY["required_variants"]:
                value = complete_report(platform)
                self.assertEqual(module.validate(value, policy, WORKSPACE), [])
                path = Path(directory) / (platform + ".json")
                path.write_text(json.dumps(value))
                paths.append(str(path))
            policy_path = Path(directory) / "synthetic-policy.json"
            policy_path.write_text(json.dumps(policy))
            result = subprocess.run([sys.executable, str(ROOT / "scripts/release-policy.py"),
                                     "--policy", str(policy_path)] + paths,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("not legal/compliance approval: PASS", result.stdout)
            self.assertTrue(policy["require_complete_native_license_closure"])

    def test_identical_inventory_observations_preserved_and_conflicts_rejected(self):
        baseline, policy = complete_report("macos"), copy.deepcopy(POLICY)
        for kind in ("file", "native-file", "staged-file"):
            value = copy.deepcopy(baseline)
            original = next(row for row in value["records"] if row["kind"] == kind)
            value["records"].append(copy.deepcopy(original))
            observations = copy.deepcopy(value["records"])
            with self.subTest(kind=kind):
                self.assertEqual(module.validate(value, policy, WORKSPACE), [])
                self.assertEqual(value["records"], observations)
            for field, changed in (("sha256", "0" * 64), ("bytes", original["bytes"] + 1)):
                conflicting = copy.deepcopy(value)
                conflicting["records"][-1][field] = changed
                with self.subTest(kind=kind, field=field), self.assertRaisesRegex(
                        ValueError, "conflicting " + kind + " hash/size binding"):
                    module.validate(conflicting, policy, WORKSPACE)

    def test_path_only_manifest_collected_hashes_reach_policy(self):
        # Synthetic documents only: exercise the production collector/validator interface offline.
        for platform in ("macos", "linux", "windows"):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory, patch.dict(
                    os.environ, {"GITHUB_WORKSPACE": directory}):
                stage = Path(directory) / "dist" / platform
                template = complete_report(platform)
                external = set()
                if platform == "linux":
                    component = gcc_runtime(template)
                    external = {component["artifact"]["path"], component["license"]["evidence"][0]["path"]}
                files = [row for row in template["records"] if row["kind"] in ("file", "staged-file")]
                paths = {}
                for row in files:
                    relative = ("evidence/" if row["kind"] == "staged-file" else "native/") + row["path"].rsplit("/", 1)[-1]
                    path = (Path(directory) / "os" / row["path"].rsplit("/", 1)[-1]
                            if row["path"] in external else stage / relative)
                    if row["path"] in external:
                        relative = str(path)
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"SYNTHETIC test bytes, NOT license evidence: " + relative.encode())
                    paths[row["path"]] = (relative, path)
                manifest_path = Path(directory) / "manifest.json"

                def manifest_refs(item):
                    if isinstance(item, list):
                        return [manifest_refs(child) for child in item]
                    if isinstance(item, dict):
                        if set(item) == {"path", "sha256"}:
                            return dict(path=paths[item["path"]][0])
                        return {key: manifest_refs(child) for key, child in item.items()}
                    return item

                manifest = manifest_refs(template["records"][-1]["evidence"])
                manifest_path.write_text(json.dumps(manifest))
                settings = dict(template["limits"])

                def collect():
                    collected = audit_module.Audit(argparse.Namespace(**settings))
                    for old_path, (relative, path) in paths.items():
                        if relative.startswith("native/") or old_path in external:
                            collected.file(path, native=path.name != "copyright")
                        if path.name == "libgcc_s.so.1":
                            collected.add("system-dependency", platform="linux", path=str(path),
                                          sha256=collected.sha256_file(path), package="libgcc-s1", owner="libgcc-s1:amd64")
                    collected.native_evidence(stage, manifest_path)
                    return collected

                collected = collect()
                self.assertFalse(collected.omitted)
                for row in collected.records:
                    if row["kind"] in ("file", "native-file", "staged-file"):
                        path = next(p for _, p in paths.values() if collected.clean(str(p)) == row["path"])
                        self.assertEqual(row["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
                        self.assertEqual(row["bytes"], path.stat().st_size)
                # Keep the existing synthetic Cargo, FFmpeg and PDFium checks, with probe paths
                # rebound to the same exact files that the real collector just hashed.
                for row in template["records"]:
                    if row["kind"] == "command":
                        old_path = row["argv"][-1]
                        new_path = collected.clean(str(paths[old_path][1]))
                        row["argv"][-1] = new_path
                        row["text"] = row["text"].replace(old_path, new_path)
                template["records"] = [row for row in template["records"] if row["kind"] not in (
                    "file", "staged-file", "native-file", "native-closure", "system-dependency")] + collected.records
                policy = copy.deepcopy(POLICY)
                # A serialized retained report, not the manifest, is the policy input.
                retained = json.loads(json.dumps(template))
                self.assertNotIn(directory, json.dumps(retained))
                self.assertEqual(module.validate(retained, policy, WORKSPACE), [])
                closure = retained["records"][-1]["evidence"]
                closure["artifacts"][0]["file"]["sha256"] = "0" * 64
                with self.assertRaisesRegex(ValueError, "mismatched native evidence reference"):
                    module.validate(retained, policy, WORKSPACE)
                closure["artifacts"][0]["file"].pop("sha256")
                with self.assertRaises(ValueError):
                    module.validate(retained, policy, WORKSPACE)
                incomplete = copy.deepcopy(manifest)
                incomplete["components"][0]["source"]["evidence"] = []
                for content in (dict(manifest, components=[]), dict(manifest, artifacts=[]), incomplete):
                    manifest_path.write_text(json.dumps(content))
                    failed = collect()
                    self.assertTrue(any(row["kind"] == "missing" for row in failed.records))
                    self.assertFalse(any(row["kind"] == "native-closure" for row in failed.records))
                    broken = dict(template, records=template["records"][:-len(collected.records)] + failed.records)
                    with self.assertRaisesRegex(ValueError, "missing evidence"):
                        module.validate(broken, policy, WORKSPACE)
                manifest_path.unlink()
                failed = collect()
                self.assertTrue(any("manifest absent" in row.get("reason", "") for row in failed.records))
                broken = dict(template, records=template["records"][:-len(collected.records)] + failed.records)
                with self.assertRaisesRegex(ValueError, "missing evidence"):
                    module.validate(broken, policy, WORKSPACE)

    def test_native_gcc_exception_scope_and_evidence(self):
        for mode in ("dynamic", "dlopen"):
            baseline, policy = complete_report(), copy.deepcopy(POLICY)
            component = gcc_runtime(baseline, mode)
            self.assertEqual(module.validate(baseline, policy, WORKSPACE), [])
            for license_id in (
                "GPL-3.0-or-later", "GPL-3.0-only", "AGPL-3.0-or-later",
                "GPL-3.0-only WITH GCC-exception-3.1",
                "GPL-3.0-or-later WITH GCC-exception-2.0",
                "GPL-3.0-or-later WITH LLVM-exception",
                "MIT WITH GCC-exception-3.1",
                "GPL-3.0-or-later WITH GCC-exception-3.1 OR MIT",
            ):
                value = copy.deepcopy(baseline)
                row = next(c for c in value["records"][-1]["evidence"]["components"]
                           if c["id"] == component["id"])
                row["license"]["id"] = license_id
                with self.subTest(mode=mode, license=license_id), self.assertRaisesRegex(ValueError, "native license"):
                    module.validate(value, policy, WORKSPACE)
            for mutate in (
                lambda c: c.update(name="other-library"),
                lambda c: c["license"].update(disposition="review"),
                lambda c: c["license"].update(evidence=[]),
                lambda c: c["license"]["evidence"][0].update(sha256="0" * 64),
                lambda c: c["source"].update(evidence=[]),
                lambda c: c["modifications"].update(evidence=[]),
                lambda c: c["obligations"]["notice"].update(status="unknown"),
                lambda c: c["obligations"].pop("notice"),
                lambda c: c["obligations"]["source"].update(evidence=[]),
                lambda c: c["obligations"]["relinking"]["evidence"][0].update(sha256="0" * 64),
            ):
                value = copy.deepcopy(baseline)
                row = next(c for c in value["records"][-1]["evidence"]["components"]
                           if c["id"] == component["id"])
                mutate(row)
                with self.subTest(mode=mode, mutation=mutate), self.assertRaises(ValueError):
                    module.validate(value, policy, WORKSPACE)
            # Ownership is exact-path/hash Linux evidence, not a name or license assertion.
            for mutation in (None, dict(package="libgcc-s10"), dict(owner="libgcc-s10:amd64"),
                             dict(owner=None), dict(path="/other/libgcc_s.so.1"),
                             dict(sha256="0" * 64), dict(platform="macos")):
                value = copy.deepcopy(baseline)
                owner = next(r for r in value["records"] if r["kind"] == "system-dependency")
                if mutation is None:
                    value["records"].remove(owner)
                else:
                    owner.update(mutation)
                with self.subTest(owner=mutation), self.assertRaisesRegex(ValueError, "native license"):
                    module.validate(value, policy, WORKSPACE)
            # Multiple package-owner records make provenance ambiguous even if one matches.
            value = copy.deepcopy(baseline)
            owner = next(r for r in value["records"] if r["kind"] == "system-dependency")
            value["records"].append(dict(owner, package="libgcc-s2", owner="libgcc-s2:amd64"))
            with self.assertRaisesRegex(ValueError, "native license"):
                module.validate(value, policy, WORKSPACE)
            # Correct OS evidence cannot license a staged/bundled copy, even at the same path.
            for bundled_path in (component["artifact"]["path"], "/stage/libgcc_s.so.1"):
                value = copy.deepcopy(baseline)
                closure = value["records"][-1]["evidence"]
                ref = next(c for c in closure["components"] if c["id"] == component["id"])["artifact"]
                old_path = ref["path"]
                ref["path"] = bundled_path
                for record in value["records"]:
                    if record.get("path") == old_path:
                        record["path"] = bundled_path
                value["records"].insert(-1, dict(kind="staged-file", bytes=12, **ref))
                with self.subTest(bundled=bundled_path), self.assertRaisesRegex(ValueError, "native license"):
                    module.validate(value, policy, WORKSPACE)
            # Component name alone is insufficient if the native artifact basename differs.
            value = copy.deepcopy(baseline)
            ref = next(c for c in value["records"][-1]["evidence"]["components"]
                       if c["id"] == component["id"])["artifact"]
            old_path = ref["path"]
            ref["path"] = "/usr/lib/synthetic/other.so"
            for record in value["records"]:
                if record.get("path") == old_path:
                    record["path"] = ref["path"]
            with self.assertRaisesRegex(ValueError, "native license"):
                module.validate(value, policy, WORKSPACE)
            scoped = copy.deepcopy(policy)
            scoped["native_license_exceptions"][0]["required_obligations"] = ["notice"]
            with self.assertRaisesRegex(ValueError, "required native"):
                module.validate(baseline, scoped, WORKSPACE)
            policy["native_license_exceptions"] = []
            with self.assertRaisesRegex(ValueError, "native license"):
                module.validate(baseline, policy, WORKSPACE)
        for platform, mode in (("macos", "dynamic"), ("windows", "dynamic"),
                               ("linux", "static"), ("linux", "embedded")):
            value, policy = complete_report(platform), copy.deepcopy(POLICY)
            gcc_runtime(value, mode)
            with self.subTest(platform=platform, mode=mode), self.assertRaisesRegex(ValueError, "native license"):
                module.validate(value, policy, WORKSPACE)
        # The native-only allowance cannot change Cargo's SPDX or FFmpeg policies.
        with self.assertRaisesRegex(ValueError, "unsupported SPDX WITH pair"):
            module.spdx("GPL-3.0-or-later WITH GCC-exception-3.1", POLICY, "third")
        self.assertFalse(module.spdx("GPL-3.0-or-later", POLICY, "third"))
        self.assertNotIn("GPL-3.0-or-later WITH GCC-exception-3.1", POLICY["ffmpeg_license_names"])

    def test_native_exception_policy_structure(self):
        for mutation in (None, {}, [dict(POLICY["native_license_exceptions"][0], modes=[])],
                         [dict(POLICY["native_license_exceptions"][0], artifact_scope="staged")],
                         [dict(POLICY["native_license_exceptions"][0], system_package="")],
                         [dict(POLICY["native_license_exceptions"][0], required_obligations=["unknown"])],
                         [POLICY["native_license_exceptions"][0],
                          dict(POLICY["native_license_exceptions"][0], modes=["dynamic"])],
                         [dict(POLICY["native_license_exceptions"][0], platform="unknown")],
                         [dict(POLICY["native_license_exceptions"][0], license="GPL-3.0-or-later")]):
            policy = copy.deepcopy(POLICY)
            policy["native_license_exceptions"] = mutation
            with self.subTest(policy=mutation), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "policy.json"
                path.write_text(json.dumps(policy))
                with self.assertRaisesRegex(ValueError, "native license exception"):
                    module.load_policy(path, 65536)

    def test_native_closure_tamper_omission_and_unsupported_evidence(self):
        baseline, policy = complete_report(), copy.deepcopy(POLICY)
        mutations = [
            lambda e: e.update(schema=True), lambda e: e.update(schema=2),
            lambda e: e.update(complete=True), lambda e: e.pop("components"),
            lambda e: e["artifacts"].pop(), lambda e: e["artifacts"].append(e["artifacts"][0]),
            lambda e: e["artifacts"].__setitem__(-1, e["artifacts"][0]),
            lambda e: e["components"].pop(), lambda e: e["components"].append(e["components"][0]),
            lambda e: e["artifacts"][0]["coverage"].pop("dlopen"),
            lambda e: e["artifacts"][0]["coverage"]["static"].update(evidence=[]),
            lambda e: e["artifacts"][0]["coverage"]["dynamic"]["components"].append("omitted"),
            lambda e: e["components"][0]["artifact"].update(sha256="0" * 64),
            lambda e: e["components"][0].update(mode="unknown"),
            lambda e: e["components"][0]["source"].pop("revision"),
            lambda e: e["components"][0]["modifications"].update(status="unknown"),
            lambda e: e["components"][0]["modifications"].update(evidence=[]),
            lambda e: e["components"][0]["modifications"]["evidence"].append(e["components"][0]["modifications"]["evidence"][0]),
            lambda e: e["components"][0]["license"].update(id="GPL-3.0-only"),
            lambda e: e["components"][0]["license"].update(id="AGPL-3.0-or-later"),
            lambda e: e["components"][0]["license"].update(id="LicenseRef-nonfree"),
            lambda e: e["components"][0]["license"].update(disposition="review"),
            lambda e: e["components"][0]["obligations"].pop("source"),
            lambda e: e["components"][0]["obligations"]["notice"].update(status="not-applicable"),
            lambda e: e["components"][0]["obligations"]["source"]["evidence"][0].update(sha256="0" * 64),
            lambda e: e["components"][0]["obligations"]["source"].update(evidence=[True]),
        ]
        for mutate in mutations:
            value = copy.deepcopy(baseline)
            mutate(value["records"][-1]["evidence"])
            with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                module.validate(value, policy, WORKSPACE)
        for replacement in (True, {}, {"schema": 1, "complete": True}):
            value = copy.deepcopy(baseline)
            value["records"][-1]["evidence"] = replacement
            with self.subTest(replacement=replacement), self.assertRaises(ValueError):
                module.validate(value, policy, WORKSPACE)
        for mutate in (
            lambda r: r["records"].append(copy.deepcopy(r["records"][-1])),
            lambda r: r["records"].append(dict(kind="staged-file", path="/stage/extra", bytes=1, sha256="c" * 64)),
            lambda r: next(row for row in r["records"] if row["kind"] == "staged-file").update(bytes=0),
            lambda r: next(row for row in r["records"] if row["kind"] == "native-file").update(sha256="0" * 64),
            lambda r: r["limits"].update(max_native_components=1),
            lambda r: r["limits"].update(max_native_files=1),
        ):
            value = copy.deepcopy(baseline)
            mutate(value)
            with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                module.validate(value, policy, WORKSPACE)
        # A static LGPL review must bind component identity AND artifact digest.
        for changes in ({"domain": "ffmpeg"}, {"subject": "different"}, {"license": "LGPL-3.0-only"}):
            value = copy.deepcopy(baseline)
            set_component_mode(value, "0-dynamic", "static")
            scoped = copy.deepcopy(policy)
            scoped_native_review(value, scoped, "0-dynamic").update(changes)
            with self.subTest(changes=changes), self.assertRaisesRegex(ValueError, "review pending"):
                module.validate(value, scoped, WORKSPACE)
        for key in ("notice", "source", "relinking"):
            value = copy.deepcopy(baseline)
            component = next(c for c in value["records"][-1]["evidence"]["components"] if c["license"]["disposition"] == "review")
            component["obligations"][key]["status"] = "not-applicable"
            with self.subTest(obligation=key), self.assertRaisesRegex(ValueError, "evidence absent"):
                module.validate(value, policy, WORKSPACE)

    def test_cli_closed_and_workflow_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for platform in POLICY["required_variants"]:
                path = Path(directory) / (platform + ".json")
                path.write_text(json.dumps(report(platform)))
                paths.append(str(path))
            command = [sys.executable, str(ROOT / "scripts/release-policy.py")]
            result = subprocess.run(command + paths, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("policy check only", result.stdout)
            self.assertNotIn("FFmpeg LGPL legal review", result.stdout)
            self.assertIn("schema 1 cannot establish", result.stdout)
            result = subprocess.run(command + ["--max-report-bytes", "128"] + paths,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("byte limit", result.stdout)
            result = subprocess.run(command + ["--max-policy-bytes", "1"] + paths,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("byte limit", result.stdout)
            # Test-only policy isolates the represented checks; production keeps the closure hold.
            policy = copy.deepcopy(POLICY)
            policy["require_complete_native_license_closure"] = False
            policy_path = Path(directory) / "test-policy.json"
            policy_path.write_text(json.dumps(policy))
            test_command = command + ["--policy", str(policy_path)]
            result = subprocess.run(test_command + paths, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("policy check only", result.stdout)
            result = subprocess.run(test_command + paths[:-1], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            result = subprocess.run(test_command + paths + paths[:1], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            Path(paths[0]).write_text('{"schema":1,"schema":1}')
            result = subprocess.run(command + paths, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("duplicate JSON key", result.stdout)
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        preflight, publish = workflow.split("  publish:\n")
        self.assertIn("python scripts/test-release-policy.py", preflight)
        self.assertIn("github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')", publish)
        # A tag push is releasable only when its exact commit is reachable from main.
        self.assertIn("fetch-depth: 0", preflight)
        guard_start = preflight.index("      - name: Verify tag commit belongs to main\n")
        guard_end = preflight.index("\n      - name:", guard_start + 1)
        guard = preflight[guard_start:guard_end]
        self.assertIn("if: github.event_name == 'push'", guard)
        self.assertIn('git merge-base --is-ancestor "$GITHUB_SHA" refs/remotes/origin/main', guard)
        self.assertIn("exit 1", guard)
        self.assertLess(guard_start, preflight.index("  package-macos:\n"))
        for job in ("package-macos", "package-linux", "package-windows"):
            self.assertIn(f"  {job}:\n    needs: release-preflight", preflight)
        for platform in POLICY["required_variants"]:
            self.assertIn("name: audit-" + platform, publish)
            self.assertIn(f"audit/{platform}/{platform}.json", publish)
        self.assertLess(publish.index("python scripts/release-policy.py"), publish.index("softprops/action-gh-release"))
        self.assertNotIn("continue-on-error", publish)
        self.assertNotIn("always()", publish)


if __name__ == "__main__":
    unittest.main()
