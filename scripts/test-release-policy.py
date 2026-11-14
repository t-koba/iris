#!/usr/bin/env python3
"""Bounded offline synthetic tests: no Cargo build, native load or network."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release_policy", ROOT / "scripts/release-policy.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
POLICY = module.load_policy(ROOT / "scripts/release-policy.json")
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


def legal_reviews(value, policy):
    for row in value["records"]:
        if row["kind"] == "file":
            policy["lgpl_legal_approvals"].append(dict(
                domain="ffmpeg", subject=row["path"] + "#sha256=" + row["sha256"],
                license="LGPL-2.1-or-later", review="synthetic test review; NOT production approval"))


class Policy(unittest.TestCase):
    def test_spdx_precedence_alternatives_and_full_parse(self):
        for expression, expected in (
            ("MIT OR Apache-2.0", True), ("MIT AND Apache-2.0", True),
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
        mutations = [lambda r: r.update(schema=2), lambda r: r.update(omitted_records=1),
                     lambda r: r.update(omitted_records=False), lambda r: r.update(limits={}),
                     lambda r: r.update(records=[]), lambda r: r["records"].append({"kind": "missing"}),
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

    def test_native_reviews_flags_unknown_and_exact_probe_binding(self):
        value, policy = report(), copy.deepcopy(POLICY)
        self.assertIn("LGPL legal review pending", " ".join(module.native(value["records"], policy)))
        legal_reviews(value, policy)
        # Even reviewed FFmpeg leaves the unrepresented complete native closure blocked.
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
            self.assertIn("LGPL legal review pending", result.stdout)
            self.assertIn("schema 1 cannot establish", result.stdout)
            # Test-only policy isolates the represented checks; production keeps the closure hold.
            policy = copy.deepcopy(POLICY)
            policy["require_complete_native_license_closure"] = False
            legal_reviews(report(), policy)
            policy_path = Path(directory) / "test-policy.json"
            policy_path.write_text(json.dumps(policy))
            test_command = command + ["--policy", str(policy_path)]
            result = subprocess.run(test_command + paths, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("policy check only", result.stdout)
            result = subprocess.run(test_command + paths[:-1], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("each release platform required", result.stdout)
            result = subprocess.run(test_command + paths + paths[:1], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            Path(paths[0]).write_text('{"schema":1,"schema":1}')
            result = subprocess.run(command + paths, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("duplicate JSON key", result.stdout)
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        preflight, publish = workflow.split("  publish:\n")
        self.assertIn("python scripts/test-release-policy.py", preflight)
        self.assertIn("github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')", publish)
        for platform in POLICY["required_variants"]:
            self.assertIn("name: audit-" + platform, publish)
            self.assertIn(f"audit/{platform}/{platform}.json", publish)
        self.assertLess(publish.index("python scripts/release-policy.py"), publish.index("softprops/action-gh-release"))
        self.assertNotIn("continue-on-error", publish)
        self.assertNotIn("always()", publish)


if __name__ == "__main__":
    unittest.main()
