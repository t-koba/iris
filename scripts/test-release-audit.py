#!/usr/bin/env python3
"""Light offline regressions; no native package build or dependency installation."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release_audit", ROOT / "scripts/release-audit.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def audit(**overrides):
    settings = dict(max_report_bytes=2097152, max_command_bytes=32768,
                    max_native_files=256, command_seconds=5, platform="linux",
                    max_metadata_bytes=8388608, max_cargo_packages=1024)
    settings.update(overrides)
    return module.Audit(argparse.Namespace(**settings))


class Reports(unittest.TestCase):
    def test_exact_bytes_bounds_missing_and_private_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Iris"
            path.write_bytes(b"actual staged binary")
            report = audit(max_command_bytes=8, max_report_bytes=8192)
            with patch.dict(os.environ, {"GITHUB_WORKSPACE": directory}):
                report.file(path, text=True)
                report.file(path.parent / "absent")
            record = report.records[0]
            self.assertEqual(record["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(record["path"], "$WORKSPACE/Iris")
            self.assertTrue(record["truncated"])
            self.assertEqual(report.records[1]["kind"], "missing")
            report.command(sys.executable, "-c", "print('x'*100)")
            self.assertTrue(report.records[-1]["truncated"])
            report.command("iris-nonexistent-inspector")
            self.assertEqual(report.records[-1]["status"], "FileNotFoundError")
            for _ in range(100):
                report.add("evidence", text="x" * 512)
            self.assertGreater(report.omitted, 0)
            self.assertLess(len(json.dumps(report.records).encode()), 8192)
            with patch.dict(os.environ, {"USERPROFILE": r"C:\Users\private"}):
                report = audit()
                report.add("path", text=r"C:\Users\private\binary")
                self.assertNotIn("private", json.dumps(report.records))

    def test_cargo_root_target_features_and_dependency_contexts(self):
        def metadata(minimal):
            # Metadata catalog; resolve features intentionally overstate the product
            # (workspace-wide unification) to prove the graph uses tree features.
            names = ["iris-ui-shell", "common", "video", "build-tool", "macro", "helper",
                     "git-dep", "dev-only", "unrelated-workspace", "other-target"]
            packages = [dict(id=name, name=name, version="1.2.3",
                             source=(None if name == "iris-ui-shell" else
                                     "git+https://example.invalid/dependency#abc" if name == "git-dep" else
                                     "registry+https://example.invalid/index"),
                             manifest_path=str(ROOT / "crates/ui-shell/Cargo.toml") if name == "iris-ui-shell" else f"/cache/{name}/Cargo.toml",
                             license=None if name == "iris-ui-shell" else "MIT OR Apache-2.0",
                             license_file=None if name != "common" else "/cache/common/LICENSE",
                             targets=[dict(kind=["proc-macro" if name == "macro" else "lib"])])
                        for name in names]
            def dep(name, kind=None):
                return dict(pkg=name, name=name.replace("-", "_"),
                            dep_kinds=[dict(kind=kind, target=None)])
            # Metadata can retain inactive target declarations for the same package edge.
            common = dict(pkg="common", name="common", dep_kinds=[
                dict(kind=None, target=None),
                dict(kind=None, target='cfg(target_family = "wasm")'),
                dict(kind="build", target=None),
                dict(kind="build", target="cfg(windows)"),
            ])
            root_deps = [common, dep("macro"), dep("git-dep"), dep("dev-only", "dev"),
                         dict(pkg="other-target", name="other",
                              dep_kinds=[dict(kind=None, target='cfg(target_arch = "wasm32")')])]
            root_deps.append(dep("build-tool", "build"))
            if not minimal:
                root_deps.append(dep("video"))
            nodes = [dict(id=name, features=(["minimal", "leaked"] if minimal else ["default", "video", "leaked"])
                          if name == "iris-ui-shell" else ["leaked"] if name in ("common", "video") else [],
                          deps=root_deps if name == "iris-ui-shell" else
                          [dep("helper")] if name in ("macro", "build-tool") else [])
                     for name in names]
            return dict(packages=packages, resolve=dict(root=None, nodes=nodes),
                        workspace_members=["iris-ui-shell", "unrelated-workspace"])

        def tree_text(minimal):
            lines = ["iris-ui-shell v1.2.3 (/workspace/crates/ui-shell)|" + ("minimal" if minimal else "default,video"),
                     "|-- common v1.2.3|",
                     "|-- macro v1.2.3 (proc-macro)|",
                     "|   `-- helper v1.2.3|",
                     "|-- git-dep v1.2.3 (https://example.invalid/dependency#abc)|",
                     "[build-dependencies]",
                     "|-- common v1.2.3| (*)",
                     "`-- build-tool v1.2.3|",
                     "    `-- helper v1.2.3| (*)"]
            if not minimal:
                lines.insert(2, "|-- video v1.2.3|")
            return "\n".join(lines) + "\n"

        for platform, host in (("linux", "x86_64-unknown-linux-gnu"),
                               ("macos", "aarch64-apple-darwin"),
                               ("windows", "x86_64-pc-windows-msvc")):
            with self.subTest(platform=platform):
                report = audit(platform=platform)
                minimal = platform == "windows"
                with patch.object(report, "cargo_metadata", return_value=metadata(minimal)) as meta_cmd, \
                     patch.object(report, "cargo_tree", return_value=tree_text(minimal)) as tree_cmd:
                    report.cargo_graph(f"rustc version\nhost: {host}\n")
                meta_argv = meta_cmd.call_args.args[0]
                expected_meta = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline",
                            "--filter-platform", host, "--manifest-path", "crates/ui-shell/Cargo.toml"]
                self.assertEqual(meta_argv, expected_meta + (["--no-default-features", "--features", "minimal"] if minimal else []))
                tree_argv = tree_cmd.call_args.args[0]
                expected_tree = ["cargo", "tree", "-p", "iris-ui-shell", "--target", host,
                                 "--offline", "--locked", "--prefix", "indent", "--charset", "ascii",
                                 "--format", "{p}|{f}", "-e", "normal,build"]
                self.assertEqual(tree_argv, expected_tree + (["--no-default-features", "--features", "minimal"] if minimal else []))
                graph = report.records[-1]
                self.assertEqual(graph["status"], "captured")
                self.assertEqual(graph["root"], "iris-ui-shell")
                selected = {p["id"]: p for p in graph["packages"]}
                self.assertEqual(set(selected), {"iris-ui-shell", "common", "build-tool", "macro", "helper", "git-dep"}
                                 | (set() if minimal else {"video"}))
                # Tree features win; workspace-wide leaked features are absent.
                self.assertEqual(selected["iris-ui-shell"]["features"], ["minimal"] if minimal else ["default", "video"])
                self.assertEqual(selected["common"]["features"], [])
                for pkg in selected.values():
                    self.assertNotIn("leaked", pkg["features"])
                self.assertEqual(selected["macro"]["contexts"], ["proc-macro"])
                self.assertEqual(selected["helper"]["contexts"], ["build"])
                self.assertEqual(selected["build-tool"]["contexts"], ["build"])
                self.assertEqual(selected["common"]["contexts"], ["build", "runtime"])
                self.assertEqual(selected["git-dep"]["source"], "git+https://example.invalid/dependency#abc")
                self.assertEqual(selected["common"]["license"], "MIT OR Apache-2.0")
                self.assertEqual(selected["common"]["license_file"], "/cache/common/LICENSE")
                self.assertEqual(selected["iris-ui-shell"]["missing_metadata"], ["license", "license_file"])
                self.assertIsNone(selected["iris-ui-shell"]["source"])
                self.assertTrue(any(e["dependency_kind"] == "build" for e in graph["edges"]))
                self.assertTrue(any(e["proc_macro"] for e in graph["edges"]))
                common_edges = [e for e in graph["edges"] if e["destination"] == "common"]
                self.assertEqual(len(common_edges), 1)
                self.assertEqual(common_edges[0]["dependency_kind"], "ambiguous")
                self.assertEqual(common_edges[0]["dependency_kinds"], ["build", "normal"])
                self.assertNotIn("target", common_edges[0])
                self.assertTrue(all(e["source"] in selected and e["destination"] in selected for e in graph["edges"]))
                # Dev-only, unrelated workspace and other-target nodes never enter the product closure.
                self.assertNotIn("dev-only", selected)
                self.assertNotIn("unrelated-workspace", selected)
                self.assertNotIn("other-target", selected)

        report = audit(max_cargo_packages=2)
        with patch.object(report, "cargo_metadata", return_value=metadata(False)), \
             patch.object(report, "cargo_tree", return_value=tree_text(False)):
            report.cargo_graph("host: test-host\n")
        graph = report.records[-1]
        self.assertEqual(graph["status"], "partial")
        self.assertTrue(graph["truncated"])
        self.assertEqual(len(graph["packages"]), 2)
        self.assertTrue(all(e["destination"] in {p["id"] for p in graph["packages"]} for e in graph["edges"]))
        report = audit(max_report_bytes=1024)
        with patch.object(report, "cargo_metadata", return_value=metadata(False)), \
             patch.object(report, "cargo_tree", return_value=tree_text(False)):
            report.cargo_graph("host: test-host\n")
        self.assertEqual(report.records[-1]["status"], "absent")
        self.assertTrue(report.records[-1]["truncated"])
        self.assertIn("report byte budget", report.records[-1]["reason"])
        for rustc, response in (("", metadata(False)), ("host: test-host\n", {})):
            report = audit()
            with patch.object(report, "cargo_metadata", return_value=response), \
                 patch.object(report, "cargo_tree", return_value=tree_text(False)):
                report.cargo_graph(rustc)
            self.assertEqual(report.records[-1]["status"], "absent")

    def test_tree_features_win_over_workspace_wide_metadata_leak(self):
        # Exact shortfall from the checkout: metadata unifies image/avif,
        # tokio/test-util and iris-thumb-preview/test-fixtures from dev/bench
        # members, while the product tree has none of them.
        packages = [
            dict(id="iris-ui-shell", name="iris-ui-shell", version="0.4.0", source=None,
                 manifest_path=str(ROOT / "crates/ui-shell/Cargo.toml"), license=None, license_file=None,
                 targets=[dict(kind=["lib"])]),
            dict(id="image", name="image", version="0.25.10", source="registry+https://example.invalid/index",
                 manifest_path="/cache/image/Cargo.toml", license="MIT OR Apache-2.0", license_file=None,
                 targets=[dict(kind=["lib"])]),
            dict(id="tokio", name="tokio", version="1.54.1", source="registry+https://example.invalid/index",
                 manifest_path="/cache/tokio/Cargo.toml", license="MIT", license_file=None,
                 targets=[dict(kind=["lib"])]),
            dict(id="iris-thumb-preview", name="iris-thumb-preview", version="0.4.0", source=None,
                 manifest_path=str(ROOT / "crates/thumb-preview/Cargo.toml"), license=None, license_file=None,
                 targets=[dict(kind=["lib"])]),
        ]
        def dep(name):
            return dict(pkg=name, name=name.replace("-", "_"), dep_kinds=[dict(kind=None, target=None)])
        nodes = [
            dict(id="iris-ui-shell", features=["default", "video", "leaked"],
                 deps=[dep("image"), dep("tokio"), dep("iris-thumb-preview")]),
            dict(id="image", features=["avif", "bmp", "gif"], deps=[]),
            dict(id="tokio", features=["test-util", "rt"], deps=[]),
            dict(id="iris-thumb-preview", features=["test-fixtures", "video"], deps=[]),
        ]
        leaked = dict(packages=packages, resolve=dict(root=None, nodes=nodes), workspace_members=["iris-ui-shell"])
        tree = ("iris-ui-shell v0.4.0 (/workspace/crates/ui-shell)|default,video\n"
                "|-- image v0.25.10|bmp,gif\n"
                "|-- tokio v1.54.1|rt\n"
                "`-- iris-thumb-preview v0.4.0 (/workspace/crates/thumb-preview)|video\n")
        report = audit(platform="linux")
        with patch.object(report, "cargo_metadata", return_value=leaked), \
             patch.object(report, "cargo_tree", return_value=tree):
            report.cargo_graph("host: x86_64-unknown-linux-gnu\n")
        graph = report.records[-1]
        self.assertEqual(graph["status"], "captured")
        selected = {p["id"]: p for p in graph["packages"]}
        self.assertEqual(set(selected), {"iris-ui-shell", "image", "tokio", "iris-thumb-preview"})
        self.assertEqual(selected["image"]["features"], ["bmp", "gif"])
        self.assertNotIn("avif", selected["image"]["features"])
        self.assertEqual(selected["tokio"]["features"], ["rt"])
        self.assertNotIn("test-util", selected["tokio"]["features"])
        self.assertEqual(selected["iris-thumb-preview"]["features"], ["video"])
        self.assertNotIn("test-fixtures", selected["iris-thumb-preview"]["features"])
        self.assertEqual(selected["iris-ui-shell"]["features"], ["default", "video"])

    def test_workspace_home_paths_are_normalized_without_actions_env(self):
        # Outside GitHub Actions GITHUB_WORKSPACE/RUNNER_TEMP are absent, but
        # HOME is still set. The report must still hide the local workspace.
        workspace = str(Path(__file__).resolve().parent.parent)
        home = os.environ.get("HOME") or str(Path.home())
        with patch.dict(os.environ, clear=False):
            os.environ.pop("GITHUB_WORKSPACE", None)
            os.environ.pop("RUNNER_TEMP", None)
            self.assertNotIn("GITHUB_WORKSPACE", os.environ)
            report = audit()
            report.add("evidence", path=workspace + "/crates/ui-shell/Cargo.toml",
                       home_path=home + "/.cargo/registry/cache",
                       package_id="path+file://" + workspace + "/crates/ui-shell#iris-ui-shell@0.4.0")
            dumped = json.dumps(report.records)
            self.assertNotIn(workspace, dumped)
            if home and home != "/":
                self.assertNotIn(home, dumped)
            self.assertIn("$WORKSPACE", dumped)
            self.assertIn("$HOME", dumped)
        win_workspace = r"C:\Users\runner\work\iris\iris"
        with patch.dict(os.environ, {"GITHUB_WORKSPACE": win_workspace}, clear=False):
            report = audit()
            report.add("evidence", package_id="path+file:///C:/Users/runner/work/iris/iris/crates/ui-shell#iris-ui-shell@0.4.0")
            self.assertNotIn("runner", json.dumps(report.records))

    def test_metadata_bytes_failure_and_timeout_are_not_partial_json(self):
        report = audit(max_metadata_bytes=128, command_seconds=1)
        self.assertEqual(report.cargo_metadata([sys.executable, "-c", "print('{}')"]), {})
        for script in ("print('x'*129)", "import sys; sys.stderr.write('x'*129)",
                       "print('{}'); raise SystemExit(1)", "print('not-json')",
                       "import time; time.sleep(2)"):
            with self.subTest(script=script), self.assertRaises(ValueError):
                report.cargo_metadata([sys.executable, "-c", script])
        with self.assertRaises(ValueError):
            report.cargo_metadata(["iris-nonexistent-cargo"])
        commands = [r for r in report.records if r["kind"] == "cargo-metadata-command"]
        self.assertTrue(any(r["truncated"] for r in commands))
        self.assertTrue(any(r["status"] == "TimeoutExpired" for r in commands))
        self.assertTrue(all(r["stdout_bytes"] <= 128 for r in commands))

    def test_linux_closure_package_and_exact_ffmpeg_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "Iris"
            library = Path(directory) / "libavcodec.so.99"
            root.write_bytes(b"exe")
            library.write_bytes(b"native")
            report = audit()
            calls = []

            def command(*argv):
                calls.append(argv)
                if argv[0] == "ldd":
                    return f"libavcodec.so.99 => {library} (0x123)\nlibmissing.so => not found\n"
                if argv[:2] == ("dpkg-query", "-S"):
                    return f"libavcodec99:amd64: {library}\n" if argv[-1] == str(library) else ""
                return ""

            with patch.object(report, "command", side_effect=command):
                report.linux([root])
            files = [r for r in report.records if r["kind"] == "file"]
            self.assertTrue({str(root), str(library)}.issubset({r["path"] for r in files}))
            self.assertTrue(any(c[:2] == ("readelf", "-d") and c[-1] == str(library) for c in calls))
            self.assertTrue(any("probe-library" in c and c[-1] == str(library) for c in calls))
            self.assertIn(("apt-cache", "policy", "libavcodec99:amd64"), calls)
            self.assertTrue(any(c[:2] == ("dpkg-query", "-W") and c[-1] == "libavcodec99:amd64" for c in calls))
            self.assertTrue(any(r.get("dependency") == "libmissing.so => not found" for r in report.records))
            self.assertFalse(any(Path(c[0]).name == "ffmpeg" for c in calls))
            limited = audit(max_native_files=1)
            with patch.object(limited, "command", side_effect=command):
                list(limited.closure([root], "linux"))
            self.assertTrue(any(r.get("reason") == "native closure scan limit reached" for r in limited.records))

    def test_ffmpeg_probe_selects_each_exact_linked_library(self):
        report = audit()
        with patch.object(report, "command") as command:
            for name in ("libavcodec.62.dylib", "libavformat.62.dylib", "libavdevice.62.dylib",
                         "libavfilter.11.dylib", "libavutil.60.dylib", "libswscale.9.dylib",
                         "libswresample.6.dylib"):
                report.ffmpeg(Path(name))
        self.assertEqual(command.call_count, 7)
        self.assertTrue(all(call.args[2] == "probe-library" for call in command.call_args_list))

    def test_macos_load_commands_and_ambiguous_brew_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            exe = base / "Iris.app/Contents/MacOS/Iris"
            lib = base / "Iris.app/Contents/Frameworks/libavcodec.99.dylib"
            for path in (exe, lib):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"staged bytes")
            for formula in ("ffmpeg", "ffmpeg-other"):
                source = base / "Cellar" / formula / "1.0/lib" / lib.name
                source.parent.mkdir(parents=True)
                source.write_bytes(b"original pre-install-name/signing bytes")
            report = audit()
            calls = []

            def command(*argv):
                calls.append(argv)
                if argv[:2] == ("otool", "-L"):
                    if argv[-1] == str(exe):
                        return (f"{argv[-1]}:\n\t@executable_path/../Frameworks/{lib.name} (compatibility version 1.0)\n"
                                f"\t/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation (compatibility version 1.0)\n")
                    return f"{lib}:\n\t@rpath/missing.dylib (compatibility version 1.0)\n"
                if argv == ("brew", "--cellar"):
                    return str(base / "Cellar")
                return ""

            with patch.object(report, "command", side_effect=command):
                report.macos([exe, lib])
            matches = [r for r in report.records if r["kind"] == "provenance-match"]
            self.assertEqual(matches[0]["status"], "ambiguous")
            self.assertEqual(len(matches[0]["candidates"]), 2)
            self.assertIn(("brew", "info", "--json=v2", "ffmpeg"), calls)
            self.assertTrue(any(r.get("dependency") == "@rpath/missing.dylib" for r in report.records))
            self.assertTrue(any(r["kind"] == "system-dependency" and "Foundation.framework" in r["resolved"] for r in report.records))
            self.assertFalse(any(call[:2] == ("otool", "-L") and "Foundation.framework" in call[-1] for call in calls))
            self.assertTrue(any("probe-library" in c and c[-1] == str(lib) for c in calls))

    def test_windows_minimal_and_import_inspector(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Iris.exe"
            path.write_bytes(b"actual portable executable")
            report = audit()
            with patch.object(module.shutil, "which", return_value="llvm-readobj"), patch.object(report, "command") as command:
                report.windows(path)
            command.assert_called_once_with("llvm-readobj", "--coff-imports", str(path))
            build = next(r for r in report.records if r["kind"] == "build")
            self.assertEqual(build["variant"], "minimal")
            self.assertEqual(build["cargo_flags"], ["--no-default-features", "--features", "minimal"])
            self.assertIn("not selected", build["native_preview"])
            report = audit()
            with patch.object(module.shutil, "which", return_value=None), patch.object(report, "command", return_value=""):
                report.windows(path)
            self.assertTrue(any(r["kind"] == "missing" for r in report.records))

    def test_pdfium_attestation_requires_matching_staged_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            docs = Path(directory) / "usr/share/doc/iris/pdfium"
            library = Path(directory) / "usr/lib/iris/libpdfium.so"
            docs.mkdir(parents=True)
            library.parent.mkdir(parents=True)
            library.write_bytes(b"staged PDFium bytes")
            asset_sha256 = hashlib.sha256(b"pinned archive").hexdigest()
            library_sha256 = hashlib.sha256(library.read_bytes()).hexdigest()
            record = {
                "schema": 1,
                "archive": {"asset": "pdfium-linux-x64.tgz", "bytes": 1234,
                            "sha256": asset_sha256},
                "runtime": {"path": "usr/lib/iris/libpdfium.so",
                            "sha256": library_sha256},
                "attestation": {"status": "verified", "pin_sha256": "1" * 64,
                                "subject_sha256": asset_sha256},
            }
            provenance = docs / "PROVENANCE.json"
            report = audit(max_command_bytes=4096)
            provenance.write_text(json.dumps(record), encoding="utf-8")
            verified = report.pdfium_attestation(docs, library)
            self.assertTrue(verified["attestation_verified"])
            self.assertEqual(verified["asset_sha256"], asset_sha256)
            self.assertEqual(verified["library_sha256"], library_sha256)
            self.assertEqual(verified["attestation_pin_sha256"], "1" * 64)

            record["attestation"]["status"] = "not_attempted"
            provenance.write_text(json.dumps(record), encoding="utf-8")
            manual = report.pdfium_attestation(docs, library)
            self.assertFalse(manual["attestation_verified"])
            self.assertEqual(manual["attestation_status"], "not_attempted")

            record["attestation"]["status"] = "failed"
            provenance.write_text(json.dumps(record), encoding="utf-8")
            failed = report.pdfium_attestation(docs, library)
            self.assertFalse(failed["attestation_verified"])
            self.assertEqual(failed["attestation_status"], "unknown")

            record["attestation"]["status"] = "verified"
            record["runtime"]["sha256"] = "0" * 64
            provenance.write_text(json.dumps(record), encoding="utf-8")
            mismatch = report.pdfium_attestation(docs, library)
            self.assertFalse(mismatch["attestation_verified"])
            self.assertEqual(mismatch["attestation_status"], "unknown")

    def test_cli_missing_outputs_is_bounded_evidence_not_approval(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "audit.json"
            with patch.dict(os.environ, {"IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES": "8192",
                                         "IRIS_PDFIUM_ATTESTATION_VERIFIED": "true"}):
                subprocess.run([sys.executable, str(ROOT / "scripts/release-audit.py"), "linux",
                                "--output", str(output)], cwd=directory, check=True)
            self.assertLessEqual(output.stat().st_size, 8192)
            report = json.loads(output.read_text())
            self.assertTrue(any(r["kind"] == "missing" for r in report["records"]))
            self.assertIn("NOT approval", report["records"][0]["note"])
            pdfium = next(r for r in report["records"] if r["kind"] == "provenance-check")
            self.assertFalse(pdfium["attestation_verified"])
            self.assertEqual(pdfium["attestation_status"], "unknown")


class Workflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = (ROOT / ".github/workflows/release.yml").read_text()
        cls.jobs = dict(re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)",
                                  cls.source.split("jobs:\n", 1)[1], re.M | re.S))

    def test_manual_never_publishes_even_when_selected_ref_is_tag(self):
        # Assert the actual job-level guard, then exercise its event/ref truth table.
        expression = re.search(r"^    if: (.+)$", self.jobs["publish"], re.M).group(1)
        match = re.fullmatch(r"github.event_name == '([^']+)' && startsWith\(github.ref, '([^']+)'\)", expression)
        self.assertIsNotNone(match)
        event, prefix = match.groups()
        for actual_event, ref, expected in (("workflow_dispatch", "refs/tags/v0.4.0", False),
                                            ("workflow_dispatch", "refs/heads/dev", False),
                                            ("push", "refs/heads/main", False),
                                            ("push", "refs/tags/v0.4.0", True)):
            self.assertEqual(actual_event == event and ref.startswith(prefix), expected)
        self.assertIn("  workflow_dispatch:\n    inputs:\n      source_ref:", self.source)
        self.assertIn("ref: ${{ inputs.source_ref || github.sha }}", self.jobs["release-preflight"])
        self.assertIn("pattern: release-*", self.jobs["publish"])
        for platform in ("macos", "linux", "windows"):
            job = self.jobs["package-" + platform]
            self.assertIn("ref: ${{ needs.release-preflight.outputs.source_sha }}", job)
            self.assertIn("name: release-" + platform, job)
            self.assertIn("name: audit-" + platform, job)
            self.assertIn("python scripts/release-audit.py " + platform, job)
        self.assertEqual(self.source.count("contents: write"), 1)
        self.assertIn("github.event_name == 'push' && secrets.IRIS_MACOS_SIGN_IDENTITY", self.source)
        linux = self.jobs["package-linux"]
        manual = linux.split("      - name: Package Linux AppDir for manual audit\n", 1)[1].split("      - name:", 1)[0]
        tagged = linux.split("      - name: Package Linux AppDir for tagged release\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch'", manual)
        self.assertIn('IRIS_VERIFY_PDFIUM_PROVENANCE: "0"', manual)
        self.assertNotIn("GH_TOKEN", manual)
        self.assertIn("run: ./scripts/package-linux-appdir.sh", manual)
        self.assertIn("if: github.event_name == 'push'", tagged)
        self.assertIn('IRIS_VERIFY_PDFIUM_PROVENANCE: "1"', tagged)
        self.assertIn("GH_TOKEN: ${{ github.token }}", tagged)
        self.assertIn("run: ./scripts/package-linux-appdir.sh", tagged)
        self.assertEqual(self.source.count("GH_TOKEN:"), 1)
        self.assertNotIn("IRIS_PDFIUM_ATTESTATION_VERIFIED", self.source)
        self.assertNotIn("pdfium_attestation_verified", self.source)
        for limit in ("MAX_REPORT_BYTES", "MAX_METADATA_BYTES", "MAX_CARGO_PACKAGES"):
            self.assertIn("vars.IRIS_RELEASE_AUDIT_" + limit, self.source)

    def test_failed_pdfium_verification_cannot_be_written_as_verified(self):
        fetch = (ROOT / "scripts/fetch-pdfium-runtime.sh").read_text()
        self.assertIn("set -euo pipefail", fetch)
        self.assertIn("[[ -n ${GH_TOKEN:-} ]]", fetch)
        verify = fetch.index('gh attestation verify "$tarball" --repo bblanchon/pdfium-binaries')
        success = fetch.index("attestation_status=verified")
        metadata = fetch.index('cat > "$doc_dir/PROVENANCE.json"')
        self.assertLess(verify, success)
        self.assertLess(success, metadata)

    def test_tag_ancestry_and_version_checks_still_fail_closed(self):
        preflight = self.jobs["release-preflight"]
        def script(name):
            block = preflight.split("      - name: " + name + "\n", 1)[1].split("      - name:", 1)[0]
            self.assertIn("if: github.event_name == 'push'", block)
            return "\n".join(line[10:] for line in block.split("run: |\n", 1)[1].splitlines())
        ancestry = script("Verify tag commit belongs to main")
        version = script("Check tag matches crate version")
        with tempfile.TemporaryDirectory() as directory:
            def run(command, **env):
                return subprocess.run(command, cwd=directory, env=dict(os.environ, **env),
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
            self.assertEqual(run(["git", "init", "-q"]), 0)
            self.assertEqual(run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "base"]), 0)
            run(["git", "update-ref", "refs/remotes/origin/main", "HEAD"])
            self.assertEqual(run(["bash", "-eu", "-c", ancestry], GITHUB_SHA="HEAD"), 0)
            self.assertNotEqual(run(["bash", "-eu", "-c", ancestry], GITHUB_SHA="missing"), 0)
            Path(directory, "Cargo.toml").write_text('[workspace.package]\nversion = "0.4.0"\n\n[workspace]\n')
            self.assertEqual(run(["bash", "-eu", "-c", version], GITHUB_REF_NAME="v0.4.0"), 0)
            for ref in ("v0.4.1", "main", "v"):
                self.assertNotEqual(run(["bash", "-eu", "-c", version], GITHUB_REF_NAME=ref), 0)


if __name__ == "__main__":
    unittest.main()
