#!/usr/bin/env python3
"""Light offline regressions; no native package build or dependency installation."""
import argparse
import contextlib
import copy
import io
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
            names = ["iris-ui-shell", "common", "common-child", "video", "build-tool", "macro", "helper",
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
                          [dep("common-child")] if name == "common" else
                          [dep("helper")] if name in ("macro", "build-tool") else [])
                     for name in names]
            return dict(packages=packages, resolve=dict(root=None, nodes=nodes),
                        workspace_members=["iris-ui-shell", "unrelated-workspace"])

        def tree_text(minimal):
            lines = ["iris-ui-shell v1.2.3 (/workspace/crates/ui-shell)|" + ("minimal" if minimal else "default,video"),
                     "|-- common v1.2.3|runtime",
                     "|   `-- common-child v1.2.3|runtime-child",
                     "|-- macro v1.2.3 (proc-macro)|",
                     "|   `-- helper v1.2.3|",
                     "|-- git-dep v1.2.3 (https://example.invalid/dependency#abc)|",
                     "[build-dependencies]",
                     "|-- common v1.2.3|host",
                     "|   `-- common-child v1.2.3|host-child",
                     "`-- build-tool v1.2.3|",
                     "    `-- helper v1.2.3| (*)"]
            if not minimal:
                lines.insert(1, "|-- video v1.2.3|")
            return "\n".join(lines) + "\n"

        for platform, host in (("linux", "x86_64-unknown-linux-gnu"),
                               ("macos", "aarch64-apple-darwin"),
                               ("windows", "x86_64-pc-windows-msvc")):
            with self.subTest(platform=platform):
                report = audit(platform=platform)
                minimal = False
                with patch.dict(os.environ, CARGO_TERM_COLOR="always"), \
                     patch.object(report, "cargo_metadata", return_value=metadata(minimal)) as meta_cmd, \
                     patch.object(report, "cargo_tree", return_value=tree_text(minimal)) as tree_cmd:
                    report.cargo_graph(f"rustc version\nhost: {host}\n")
                meta_argv = meta_cmd.call_args.args[0]
                expected_meta = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline",
                            "--filter-platform", host, "--manifest-path", "crates/ui-shell/Cargo.toml"]
                self.assertEqual(meta_argv, expected_meta + (["--no-default-features", "--features", "minimal"] if minimal else []))
                tree_argv = tree_cmd.call_args.args[0]
                expected_tree = ["cargo", "tree", "--color", "never", "-p", "iris-ui-shell", "--target", host,
                                 "--offline", "--locked", "--prefix", "indent", "--charset", "ascii",
                                 "--format", "{p}|{f}", "-e", "normal,build"]
                self.assertEqual(tree_argv, expected_tree + (["--no-default-features", "--features", "minimal"] if minimal else []))
                graph = report.records[-1]
                self.assertEqual(graph["status"], "captured")
                self.assertEqual(graph["root"], "iris-ui-shell")
                selected = {p["id"]: p for p in graph["packages"]}
                self.assertEqual(set(selected), {"iris-ui-shell", "common", "common-child", "build-tool", "macro", "helper", "git-dep"}
                                 | (set() if minimal else {"video"}))
                # Tree features win; workspace-wide leaked features are absent.
                self.assertEqual(selected["iris-ui-shell"]["features"], ["minimal"] if minimal else ["default", "video"])
                self.assertEqual(selected["common"]["features"], ["host", "runtime"])
                self.assertEqual(selected["common-child"]["features"], ["host-child", "runtime-child"])
                for pkg in selected.values():
                    self.assertNotIn("leaked", pkg["features"])
                self.assertEqual(selected["macro"]["contexts"], ["proc-macro"])
                self.assertEqual(selected["helper"]["contexts"], ["build"])
                self.assertEqual(selected["build-tool"]["contexts"], ["build"])
                self.assertEqual(selected["common"]["contexts"], ["build", "runtime"])
                self.assertEqual(selected["common-child"]["contexts"], ["build", "runtime"])
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

    def test_metadata_miss_retains_partial_product_tree(self):
        for platform, host in (("macos", "aarch64-apple-darwin"),
                               ("windows", "x86_64-pc-windows-msvc")):
            minimal = False
            tree = ("iris-ui-shell v0.4.0 (/workspace/crates/ui-shell)|" +
                    ("minimal" if minimal else "default,video,pdf") + "\n" +
                    ("" if minimal else "|-- video v1.2.3|decode\n") +
                    "|-- common v1.2.3|std\n"
                    "|-- macro v1.2.3 (proc-macro)|derive\n"
                    "|   `-- helper v1.2.3|\n"
                    "|-- git-dep v1.2.3 (https://example.invalid/repo#abc)|feature\n"
                    "[build-dependencies]\n"
                    "|-- common v1.2.3|std (*)\n"
                    "`-- build-tool v1.2.3|\n"
                    "    `-- helper v1.2.3| (*)\n")
            with self.subTest(platform=platform):
                report = audit(platform=platform)
                with patch.object(report, "cargo_metadata", side_effect=ValueError("aligned v0.4.3 not cached")) as meta, \
                     patch.object(report, "cargo_tree", return_value=tree) as cmd:
                    report.cargo_graph(f"host: {host}\n")
                for argv in (meta.call_args.args[0], cmd.call_args.args[0]):
                    self.assertIn("--locked", argv)
                    self.assertIn("--offline", argv)
                    self.assertIn(host, argv)
                    self.assertEqual("--no-default-features" in argv, minimal)
                    self.assertEqual("minimal" in argv, minimal)
                self.assertEqual(cmd.call_count, 1)  # no network retry
                graph = report.records[-1]
                self.assertEqual(graph["status"], "partial")
                self.assertFalse(graph["truncated"])
                self.assertIn("aligned", graph["metadata_error"])
                self.assertEqual(graph["variant"], "minimal" if minimal else "product")
                packages = {p["name"]: p for p in graph["packages"]}
                self.assertEqual(set(packages), {"iris-ui-shell", "common", "macro", "helper",
                                                "git-dep", "build-tool"} | (set() if minimal else {"video"}))
                root = packages["iris-ui-shell"]
                self.assertEqual(graph["root"], root["id"])
                self.assertEqual(root["tree_identity"], report.clean("iris-ui-shell v0.4.0 (/workspace/crates/ui-shell)"))
                self.assertEqual(root["features"], ["minimal"] if minimal else ["default", "pdf", "video"])
                self.assertEqual(root["source_kind"], "path")
                self.assertEqual(root["source"], report.clean("/workspace/crates/ui-shell"))
                self.assertEqual(packages["git-dep"]["source"], "https://example.invalid/repo#abc")
                self.assertEqual(packages["git-dep"]["source_kind"], "tree-location")
                self.assertIsNone(packages["common"]["source"])
                self.assertEqual(packages["common"]["source_kind"], "unavailable")
                self.assertIn("source", packages["common"]["missing_metadata"])
                self.assertEqual(packages["common"]["features"], ["std"])
                self.assertEqual(packages["common"]["contexts"], ["build", "runtime"])
                self.assertEqual(packages["macro"]["contexts"], ["proc-macro"])
                self.assertEqual(packages["helper"]["contexts"], ["build"])
                for package in packages.values():
                    self.assertEqual(package["status"], "partial")
                    self.assertEqual(package["version"], "0.4.0" if package is root else "1.2.3")
                    for field in ("license", "license_file"):
                        self.assertIsNone(package[field])
                        self.assertIn(field, package["missing_metadata"])
                edges = {e["destination"]: e for e in graph["edges"] if e["source"] == root["id"]}
                self.assertEqual(edges[packages["common"]["id"]]["dependency_kinds"], ["build", "normal"])
                self.assertTrue(edges[packages["macro"]["id"]]["proc_macro"])
                self.assertEqual(edges[packages["build-tool"]["id"]]["dependency_kind"], "build")

                bounded = audit(platform=platform, max_cargo_packages=2)
                with patch.object(bounded, "cargo_metadata", side_effect=ValueError("offline miss")), \
                     patch.object(bounded, "cargo_tree", return_value=tree):
                    bounded.cargo_graph(f"host: {host}\n")
                limited = bounded.records[-1]
                self.assertEqual(limited["status"], "partial")
                self.assertTrue(limited["truncated"])
                self.assertEqual(len(limited["packages"]), 2)
                ids = {p["id"] for p in limited["packages"]}
                self.assertTrue(all(e["source"] in ids and e["destination"] in ids for e in limited["edges"]))

    def test_metadata_miss_does_not_hide_tree_failure_or_ambiguous_identity(self):
        root = "iris-ui-shell v0.4.0 (/workspace/crates/ui-shell)|default\n"
        for tree in (ValueError("tree offline miss"), "", root + "`-- dep unknown identity|\n",
                     root + "|-- dep v1.0.0 (proc-macro)|\n`-- dep v1.0.0|\n"):
            with self.subTest(tree=tree):
                report = audit()
                with patch.object(report, "cargo_metadata", side_effect=ValueError("offline miss")), \
                     patch.object(report, "cargo_tree", **({"side_effect": tree} if isinstance(tree, Exception)
                                                          else {"return_value": tree})) as cmd:
                    report.cargo_graph("host: test-host\n")
                self.assertEqual(cmd.call_count, 1)
                graph = report.records[-1]
                self.assertEqual(graph["status"], "absent")
                self.assertEqual(graph["packages"], [])
                self.assertEqual(graph["edges"], [])

    def test_ambiguous_catalog_does_not_fall_back_to_tree_ids(self):
        packages = [dict(id="root", name="iris-ui-shell", version="0.4.0", source=None)]
        packages.extend(dict(id=identity, name="dep", version="1.0.0", source=identity)
                        for identity in ("registry+one", "registry+two"))
        report = audit()
        with patch.object(report, "cargo_metadata", return_value=dict(packages=packages)), \
             patch.object(report, "cargo_tree", return_value=(
                 "iris-ui-shell v0.4.0 (/workspace/crates/ui-shell)|default\n`-- dep v1.0.0|\n")):
            report.cargo_graph("host: test-host\n")
        self.assertEqual(report.records[-1]["status"], "absent")
        self.assertIn("ambiguous catalog", report.records[-1]["reason"])
        self.assertEqual(report.records[-1]["packages"], [])

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
            owner = next(r for r in report.records if r["kind"] == "system-dependency")
            self.assertEqual(owner, dict(kind="system-dependency", platform="linux", path=str(library),
                                         sha256=module.Audit.sha256_file(library),
                                         package="libavcodec99", owner="libavcodec99:amd64"))
            self.assertIn(("apt-cache", "policy", "libavcodec99:amd64"), calls)
            self.assertTrue(any(c[:2] == ("dpkg-query", "-W") and c[-1] == "libavcodec99:amd64" for c in calls))
            self.assertTrue(any(r.get("dependency") == "libmissing.so => not found" for r in report.records))
            self.assertFalse(any(Path(c[0]).name == "ffmpeg" for c in calls))
            limited = audit(max_native_files=1)
            with patch.object(limited, "command", side_effect=command):
                list(limited.closure([root], "linux"))
            self.assertTrue(any(r.get("reason") == "native closure scan limit reached" for r in limited.records))

    def test_linux_owner_requires_exact_dpkg_path_and_retains_usr_merge_binding(self):
        path = "/usr/lib/synthetic/libgcc_s.so.1"
        alias = path[4:]
        for replies, expected in (
            ({path: "libgcc-s1:amd64: " + path}, True),
            ({alias: "libgcc-s1:amd64: " + alias}, True),
            ({path: "libgcc-s1:amd64: " + path + ".other"}, False),
            ({path: "libgcc-s1:amd64: /other/libgcc_s.so.1"}, False),
            ({path: "diversion by libgcc-s1: " + path}, False),
            ({}, False),
        ):
            with self.subTest(replies=replies):
                report = audit()
                report.add("native-file", path=path, bytes=12, sha256="a" * 64)
                def command(*argv):
                    return replies.get(argv[-1], "") if argv[:2] == ("dpkg-query", "-S") else ""
                with patch.object(report, "closure", return_value=iter([Path(path)])), patch.object(
                        report, "command", side_effect=command), patch.object(report, "file") as file:
                    report.linux([Path(path)])
                owners = [r for r in report.records if r["kind"] == "system-dependency"]
                if expected:
                    self.assertEqual(owners, [dict(kind="system-dependency", platform="linux", path=path,
                                                  sha256="a" * 64, package="libgcc-s1", owner="libgcc-s1:amd64")])
                    file.assert_any_call(Path("/usr/share/doc/libgcc-s1/copyright"), text=True)
                else:
                    self.assertFalse(owners)
                    self.assertTrue(any(r["kind"] == "missing" and r.get("path") == path for r in report.records))

    def test_ffmpeg_probe_selects_each_exact_linked_library(self):
        report = audit()
        libraries = ("libavcodec.62.dylib", "libavformat.62.dylib", "libavdevice.62.dylib",
                     "libavfilter.11.dylib", "libavutil.60.dylib", "libswscale.9.dylib",
                     "libswresample.6.dylib", "libavcodec.so.60", "avcodec-62.dll",
                     "avformat-61.dll", "avutil-60.dll", "swscale-9.dll")
        with patch.object(report, "command") as command:
            for name in libraries:
                report.ffmpeg(Path(name))
            for name in ("ffmpeg.exe", "avcodec-62.dll.bak", "avcodec-62.exe", "libavcodec"):
                report.ffmpeg(Path(name))
        self.assertEqual(command.call_count, len(libraries))
        self.assertTrue(all(call.args[2] == "probe-library" for call in command.call_args_list))
        self.assertEqual([Path(call.args[3]).name for call in command.call_args_list], list(libraries))

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

    def test_macos_absolute_homebrew_dependency_repeats_identical_file_observations(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            exe = base / "Iris.app/Contents/MacOS/Iris"
            cellar = base / "Cellar"
            keg = cellar / "ffmpeg/1.0"
            lib = keg / "lib/libavcodec.99.dylib"
            for path, content in ((exe, b"staged executable"), (lib, b"external Homebrew dylib"),
                                  (keg / "INSTALL_RECEIPT.json", b"{}"),
                                  (keg / ".brew/ffmpeg.rb", b"# synthetic formula")):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            report = audit(platform="macos")

            def command(*argv):
                if argv[:2] == ("otool", "-L"):
                    dependency = f"\t{lib} (compatibility version 1.0)\n" if argv[-1] == str(exe) else ""
                    return f"{argv[-1]}:\n{dependency}"
                if argv == ("brew", "--cellar"):
                    return str(cellar)
                return ""

            with patch.object(report, "command", side_effect=command):
                report.macos([exe])
            path = report.clean(str(lib))
            observations = [r for r in report.records if r["kind"] == "file" and r["path"] == path]
            self.assertEqual(len(observations), 2)  # Native closure, then Homebrew candidate.
            self.assertEqual(observations[0], observations[1])
            self.assertEqual(observations[0]["sha256"], hashlib.sha256(lib.read_bytes()).hexdigest())
            self.assertEqual(observations[0]["bytes"], len(lib.read_bytes()))
            self.assertEqual(len([r for r in report.records
                                  if r["kind"] == "native-file" and r["path"] == path]), 1)
            provenance = next(r for r in report.records if r["kind"] == "homebrew")
            self.assertEqual((provenance["staged"], provenance["candidate"]), (path, path))
            match = next(r for r in report.records if r["kind"] == "provenance-match")
            self.assertEqual(match["candidates"], [path])
            self.assertEqual(match["status"], "candidate only")
            self.assertFalse(any(r["kind"] == "missing" for r in report.records))

    def test_windows_product_and_import_inspector(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Iris.exe"
            path.write_bytes(b"actual portable executable")
            dlls = [Path(directory) / name for name in ("avcodec-62.dll", "avformat-61.dll", "pdfium.dll")]
            for dll in dlls:
                dll.write_bytes(b"staged native bytes")
            report = audit()
            with patch.object(module.shutil, "which", return_value="llvm-readobj"), patch.object(report, "command") as command:
                report.windows(path)
            command.assert_any_call("llvm-readobj", "--coff-imports", str(path))
            self.assertEqual(len([call for call in command.call_args_list
                                  if call.args[:2] == ("llvm-readobj", "--coff-imports")]), 1)
            probes = [call for call in command.call_args_list if "probe-library" in call.args]
            self.assertEqual([call.args[3] for call in probes], [str(dll) for dll in dlls[:2]])
            self.assertTrue(all(call.args[0] == sys.executable
                                and Path(call.args[1]).name == "release-audit.py" for call in probes))
            self.assertFalse(any(Path(call.args[0]).name == "ffmpeg" for call in command.call_args_list))
            self.assertTrue(any(row["kind"] == "file" and row["path"] == str(dll) for dll in dlls
                                for row in report.records))
            build = next(r for r in report.records if r["kind"] == "build")
            self.assertEqual(build["variant"], "product")
            self.assertEqual(build["features"], "Cargo defaults")
            self.assertNotIn("cargo_flags", build)
            self.assertIn("verified exact-SHA product stage", build["native_preview"])
            report = audit()
            with patch.object(module.shutil, "which", return_value=None), patch.object(report, "command", return_value=""):
                report.windows(path)
            self.assertTrue(any(r["kind"] == "missing" for r in report.records))

    def test_pdfium_pins_match_fetch_policy(self):
        fetch = (ROOT / "scripts/fetch-pdfium-runtime.sh").read_text()
        def pin(name):
            return re.search(r"^" + name + r'="?([^"\n]+)"?$', fetch, re.M)[1]
        policy = module.PDFIUM_ATTESTATION
        self.assertEqual(pin("pdfium_commit"), module.PDFIUM_COMMIT)
        self.assertEqual(policy["signer_digest"], module.PDFIUM_COMMIT)
        self.assertEqual(policy["source_digest"], module.PDFIUM_COMMIT)
        for key, name in (("pin_sha256", "attestation_sha256"),
                          ("bundle_bytes", "attestation_bytes"), ("repo", "repo"),
                          ("signer_workflow", "signer_workflow"), ("source_ref", "source_ref")):
            self.assertEqual(pin("pdfium_" + name), str(policy[key]))
        archives = re.findall(r'asset="([^"]+)"\s+asset_bytes=(\d+)\s+asset_sha256="([^"]+)"', fetch)
        self.assertEqual({name: (int(size), digest) for name, size, digest in archives},
                         module.PDFIUM_ARCHIVES)

    def test_pdfium_attestation_requires_matching_staged_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            docs = Path(directory) / "usr/share/doc/iris/pdfium"
            library = Path(directory) / "usr/lib/iris/libpdfium.so"
            docs.mkdir(parents=True)
            library.parent.mkdir(parents=True)
            library.write_bytes(b"staged PDFium bytes")
            library_sha256 = hashlib.sha256(library.read_bytes()).hexdigest()
            policy = module.PDFIUM_ATTESTATION
            record = {
                "schema": 2,
                "runtime": {"path": "usr/lib/iris/libpdfium.so", "sha256": library_sha256},
                "attestation": dict(policy, status="verified", bundle_sha256=policy["pin_sha256"],
                                    verifier_version="2.83.0", verifier_sha256="a" * 64),
            }
            provenance = docs / "PROVENANCE.json"
            report = audit(max_command_bytes=4096)
            def check(value):
                provenance.write_text(json.dumps(value), encoding="utf-8")
                return report.pdfium_attestation(docs, library)
            def rejected(value):
                result = check(value)
                self.assertFalse(result["attestation_verified"])
                self.assertEqual(result["attestation_status"], "unknown")

            for asset, (size, digest) in module.PDFIUM_ARCHIVES.items():
                record["archive"] = dict(asset=asset, bytes=size, sha256=digest)
                record["attestation"]["subject_sha256"] = digest
                with self.subTest(asset=asset):
                    verified = check(record)
                    self.assertTrue(verified["attestation_verified"])
                    self.assertEqual(verified["asset_sha256"], digest)
                    self.assertEqual(verified["library_sha256"], library_sha256)
                    self.assertEqual(verified["attestation_pin_sha256"], policy["pin_sha256"])
                    self.assertEqual(verified["verifier_version"], "2.83.0")
                    self.assertEqual(verified["verifier_sha256"], "a" * 64)
                    self.assertIn("not TUF-root or legal approval", verified["note"])

            old_schema = copy.deepcopy(record)
            old_schema["schema"] = 1
            rejected(old_schema)

            # Every required attestation field must be present and exact, including
            # plausible but unpinned digests, signer workflow/ref, and archive subject.
            for key, value in record["attestation"].items():
                if key in ("verifier_version", "verifier_sha256"):
                    continue  # Observed verifier identity is validated, not pinned upstream.
                for replacement in (None, "0" * 64 if isinstance(value, str) else value + 1):
                    with self.subTest(field=key, replacement=replacement):
                        bad = copy.deepcopy(record)
                        if replacement is None:
                            del bad["attestation"][key]
                        else:
                            bad["attestation"][key] = replacement
                        rejected(bad)
            for key, invalid in (
                ("verifier_version", (None, "", "v2.83.0", "02.83.0", "2.83", "2.83.0-dev",
                                      "2.83.0\n", "1" * 65 + ".0.0", 283, [])),
                ("verifier_sha256", (None, "", "a" * 63, "a" * 65, "g" * 64, "A" * 64, 123, [])),
            ):
                bad = copy.deepcopy(record)
                del bad["attestation"][key]
                rejected(bad)
                for value in invalid:
                    with self.subTest(field=key, value=value):
                        bad["attestation"][key] = value
                        rejected(bad)
            for section in ("archive", "runtime"):
                for key, value in record[section].items():
                    for replacement in (None, "0" * 64 if isinstance(value, str) else value + 1):
                        with self.subTest(section=section, field=key, replacement=replacement):
                            bad = copy.deepcopy(record)
                            if replacement is None:
                                del bad[section][key]
                            else:
                                bad[section][key] = replacement
                            rejected(bad)
            bad = copy.deepcopy(record)
            # An internally consistent, but unpinned archive and subject is not enough.
            bad["archive"]["sha256"] = bad["attestation"]["subject_sha256"] = "0" * 64
            rejected(bad)
            manual_record = copy.deepcopy(record)
            manual_record["attestation"]["status"] = "not_attempted"
            manual_record["attestation"]["bundle_sha256"] = None
            manual_record["attestation"]["bundle_bytes"] = None
            manual_record["attestation"]["verifier_version"] = None
            manual_record["attestation"]["verifier_sha256"] = None
            manual = check(manual_record)
            self.assertFalse(manual["attestation_verified"])
            self.assertEqual(manual["attestation_status"], "not_attempted")
            for key in ("verifier_version", "verifier_sha256"):
                self.assertIsNone(manual[key])
                for value in ("absent", record["attestation"][key], "", False):
                    with self.subTest(manual_field=key, value=value):
                        bad = copy.deepcopy(manual_record)
                        if value == "absent":
                            del bad["attestation"][key]
                        else:
                            bad["attestation"][key] = value
                        rejected(bad)
            library.write_bytes(b"different staged library")
            rejected(record)
            rejected(manual_record)
            library.unlink()
            rejected(record)

    def test_native_evidence_binds_actual_stage_bytes_and_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory) / "stage"
            stage.mkdir()
            payload, proof = stage / "Iris", stage / "SYNTHETIC-EVIDENCE.txt"
            payload.write_bytes(b"synthetic executable, NOT a real release")
            proof.write_text("synthetic link/source/license/notice evidence; NOT legal approval")
            copyright = Path(directory) / "copyright"
            copyright.write_text("SYNTHETIC OS package copyright; NOT production license evidence")
            sidecar = Path(directory) / "closure.json"
            ref = lambda p: dict(path=str(p), sha256=module.Audit.sha256_file(p))
            payload_ref, proof_ref = ref(payload), ref(proof)
            modes = ("static", "embedded", "dynamic", "dlopen")
            value = dict(schema=1, artifacts=[dict(file=ref(p), coverage={
                mode: dict(components=[mode] if p == payload else [], evidence=[proof_ref])
                for mode in modes}) for p in (payload, proof)], components=[dict(
                    id=mode, artifact=payload_ref, mode=mode, name="synthetic-" + mode,
                    source=dict(origin="https://example.invalid/synthetic", revision="test-revision", evidence=[proof_ref]),
                    modifications=dict(status="unmodified", evidence=[proof_ref]),
                    license=dict(id="MIT", disposition="allowed", evidence=[ref(copyright)]),
                    obligations={key: dict(status="provided", evidence=[proof_ref])
                                 for key in ("notice", "source", "relinking")}) for mode in modes])

            def collect(content=None, **limits):
                if content is not None:
                    sidecar.write_text(content)
                report = audit(max_native_components=16, **limits)
                report.file(payload, native=True)
                report.file(copyright, text=True)
                report.native_evidence(stage, sidecar)
                return report

            report = collect(json.dumps(value))
            self.assertFalse(any(r["kind"] == "missing" for r in report.records), report.records)
            closure = next(r["evidence"] for r in report.records if r["kind"] == "native-closure")
            self.assertEqual(len(module.validate_closure(closure, report.records, 256, 16)), 4)
            # Reusing the same sidecar after a proof or payload change must not pass.
            for path in (proof, payload, copyright):
                original = path.read_bytes()
                path.write_bytes(original + b"tampered")
                failed = collect()
                self.assertTrue(any(r["kind"] == "missing" for r in failed.records))
                self.assertFalse(any(r["kind"] == "native-closure" for r in failed.records))
                path.write_bytes(original)
            def path_only(item):
                if isinstance(item, list):
                    return [path_only(child) for child in item]
                if isinstance(item, dict):
                    if set(item) == {"path", "sha256"}:
                        return dict(path=item["path"] if item["path"] == str(copyright) else Path(item["path"]).name)
                    return {key: path_only(child) for key, child in item.items()}
                return item

            manifest = path_only(value)
            for path in (payload, proof):
                original = path.read_bytes()
                path.write_bytes(original + b"new build")
                fresh = collect(json.dumps(manifest))
                self.assertFalse(any(r["kind"] == "missing" for r in fresh.records))
                inventory = next(r for r in fresh.records if r["kind"] == "staged-file" and r["path"] == str(path))
                self.assertEqual(inventory["bytes"], len(original + b"new build"))
                self.assertEqual(inventory["sha256"], module.Audit.sha256_file(path))
                path.write_bytes(original)
            for bad_path in ("../Iris", "./Iris", "dir//Iris", "dir/../Iris", "dir\\Iris",
                             "C:/Iris", "Iris/", "absent", "Iri*", "Iris\n", ""):
                invalid = copy.deepcopy(manifest)
                invalid["artifacts"][0]["file"]["path"] = bad_path
                failed = collect(json.dumps(invalid))
                self.assertTrue(any(r["kind"] == "missing" for r in failed.records), bad_path)
                self.assertFalse(any(r["kind"] == "native-closure" for r in failed.records), bad_path)
            # Supporting references cannot read arbitrary, uninventoried filesystem paths.
            invalid = copy.deepcopy(manifest)
            invalid["components"][0]["source"]["evidence"] = [dict(path="/etc/passwd")]
            self.assertTrue(any(r["kind"] == "missing" for r in collect(json.dumps(invalid)).records))
            collect(json.dumps(value))
            extra = stage / "unlisted"
            extra.write_bytes(b"unlisted payload")
            self.assertTrue(any(r["kind"] == "missing" for r in collect().records))
            extra.unlink()
            for content in ('{"schema":1,"schema":1}', '{"schema":1,"complete":true}',
                            json.dumps(dict(value, schema=2)), 'true'):
                failed = collect(content)
                self.assertTrue(any(r["kind"] == "missing" for r in failed.records))
                self.assertFalse(any(r["kind"] == "native-closure" for r in failed.records))
            failed = collect(json.dumps(value), max_native_files=1)
            self.assertTrue(any(r["kind"] == "missing" for r in failed.records))
            failed = collect(json.dumps(value), max_report_bytes=256)
            self.assertFalse(any(r["kind"] == "native-closure" for r in failed.records))
            self.assertTrue(failed.omitted or any(r["kind"] == "missing" for r in failed.records))
            sidecar.unlink()
            self.assertTrue(any(r["kind"] == "missing" for r in collect().records))

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


class BundledLinuxClosure(unittest.TestCase):
    def test_staged_elfs_skip_dpkg_and_keep_external_owner_check(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            stage = base / "dist/linux"
            executable = stage / "Iris.AppDir/usr/bin/Iris"
            library = stage / "Iris.AppDir/usr/lib/iris/libavcodec.so.60"
            external = base / "system/libgcc_s.so.1"
            for path in (executable, library, external):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"synthetic " + path.name.encode())
            staged = {str(executable), str(library)}
            for owned in (True, False):
                with self.subTest(owned=owned):
                    report = audit()
                    calls = []

                    def command(*argv):
                        calls.append(argv)
                        if argv[0] == "ldd":
                            if argv[-1] == str(executable):
                                return (f"{library.name} => {library} (0x1)\n"
                                        f"libgcc_s.so.1 => {external} (0x2)\n")
                            return "linux-vdso.so.1 (0x00007fff)\n"
                        if argv[:2] == ("dpkg-query", "-S") and owned and argv[-1] == str(external):
                            return "libgcc-s1:amd64: " + str(external) + "\n"
                        return ""

                    with patch.object(report, "command", side_effect=command):
                        report.linux([executable], stage=stage)
                    self.assertEqual(report.bundled, [executable.resolve(), library.resolve()])
                    self.assertFalse(any(argv[:2] == ("dpkg-query", "-S") and argv[-1] in staged
                                         for argv in calls))
                    self.assertEqual([argv[-1] for argv in calls
                                      if argv[:2] == ("dpkg-query", "-S")], [str(external)])
                    self.assertTrue(staged.issubset(
                        {row["path"] for row in report.records if row["kind"] == "native-file"}))
                    self.assertFalse(staged & {row.get("path") for row in report.records
                                               if row["kind"] == "system-dependency"})
                    missing = {(row.get("path"), row.get("reason")) for row in report.records
                               if row["kind"] == "missing"}
                    if owned:
                        self.assertFalse(missing, report.records)
                        self.assertEqual(next(row for row in report.records
                                              if row["kind"] == "system-dependency"),
                                         dict(kind="system-dependency", platform="linux",
                                              path=str(external),
                                              sha256=module.Audit.sha256_file(external),
                                              package="libgcc-s1", owner="libgcc-s1:amd64"))
                    else:
                        self.assertEqual(missing, {(str(external),
                                                   "no Debian package owner for external native file")})

    def test_bundled_paths_require_byte_bound_reviewed_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            library = stage / "libavcodec.so.60"
            library.write_bytes(b"synthetic bundled bytes")
            spelling, sha = str(library), module.Audit.sha256_file(library)
            report = audit()
            with patch.object(report, "closure", return_value=iter([library])),                     patch.object(report, "command", return_value=""):
                report.linux([library], stage=stage)
            self.assertEqual(report.bundled, [library.resolve()])
            report.file(library, native=True)  # closure()'s byte-bound inventory
            original = list(report.records)

            def sidecar(digest=None, ids=("ffmpeg-avcodec",), modes=("dynamic",)):
                return dict(schema=1, artifacts=[dict(file=dict(path=spelling,
                                                                sha256=digest or sha),
                                                      coverage={mode: dict(components=list(ids), evidence=[])
                                                                for mode in modes})],
                            components=[dict(id="ffmpeg-avcodec", mode="dynamic",
                                             artifact=dict(path=spelling, sha256=sha), name="synthetic")])

            def check(value=None):
                fresh = audit()
                fresh.records = copy.deepcopy(original)
                fresh.bundled = list(report.bundled)
                if value is not None:
                    fresh.add("native-closure", evidence=value)
                fresh.verify_bundled_native_evidence()
                return [row for row in fresh.records if row["kind"] == "missing"]

            valid = check(sidecar())
            self.assertFalse(valid, valid)
            for value, expected in (
                    (sidecar(digest="b" * 64), "matching native-closure artifact hash"),
                    (sidecar(ids=(), modes=()), "native-closure coverage"),
                    (sidecar(ids=("unlisted",)), "native-closure coverage"),
                    (dict(schema=1, artifacts=[], components=[]), "native-closure artifact"),
                    (None, "native-closure artifact")):
                with self.subTest(expected=expected):
                    missing = check(value)
                    self.assertEqual(len(missing), 1, missing)
                    self.assertEqual(missing[0]["path"], spelling)
                    self.assertIn(expected, missing[0]["reason"])


class WindowsNativeEvidencePipeline(unittest.TestCase):
    """Producer evidence -> sealed manifest -> audit closure, all offline."""

    def fixture(self, directory):
        base = Path(directory)
        ev, stage, release = base / "evidence", base / "stage", base / "release"
        for path in (ev, stage, release):
            path.mkdir()
        dist = base / "dist" / "windows"
        (dist / "Iris").mkdir(parents=True)
        # Generate the reviewed plan from checked-in contracts. A checked-out
        # tree has no build/validation/evidence directory, so the fixture must
        # never depend on evidence produced by an earlier runner.
        inputs_path = ev / "validation-inputs.json"
        generated = subprocess.run(
            [sys.executable, str(ROOT / "scripts/windows-ffmpeg-validation-config.py"),
             str(inputs_path)], capture_output=True, text=True)
        self.assertEqual(generated.returncode, 0, generated.stderr)
        inputs = json.loads(inputs_path.read_text())
        names = ["Iris.exe", "avcodec-62.dll", "avformat-61.dll", "avutil-60.dll",
                 "pdfium.dll", "FFMPEG-NOTICE.md",
                 "ffmpeg-8.1.3.tar.xz", "ffmpeg-legal-notices.tar.gz",
                 "ffmpeg-build-materials.tar.gz", "pdfium-notices.tar.gz",
                 "ffmpeg-source-bundle.json"]
        notice = (ROOT / "packaging/windows/ffmpeg-README.md").read_text()
        for name in names:
            data = notice.encode() if name == "FFMPEG-NOTICE.md" else (name + " binary").encode()
            (stage / name).write_bytes(data)
            (dist / "Iris" / name).write_bytes(data)
        files = [dict(name=name, sha256=module.Audit.sha256_file(stage / name),
                      bytes=(stage / name).stat().st_size) for name in names]
        inventory = dict(roots=[str(stage)], files=files)
        (ev / "product-inventory.json").write_text(json.dumps(inventory))
        (ev / "pe-import-closure.json").write_text(json.dumps(dict(
            roots=["Iris.exe"], binaries=names,
            imports=[{"from": "Iris.exe", "import": "avcodec-62.dll",
                      "resolution": "stage"}],
            unresolved=[],
            resolution_values=["system", "stage", "vc-redist", "os", "unresolved"],
            vc_imports=[], vc_self_test=dict(status="pass", dlls=[]))))
        assets = {}
        for name, key in (("Iris-windows.zip", "zip_sha256"),
                          ("Iris-windows-setup.exe", "installer_sha256")):
            data = (name + " bytes").encode()
            (release / name).write_bytes(data)
            (dist / name).write_bytes(data)
            digest = module.Audit.sha256_file(release / name)
            (dist / (name + ".sha256")).write_text(f"{digest}  {name}\r\n")
            assets[key] = digest
        (release / "product-inventory.json").write_text(json.dumps(inventory))
        (release / "release-package-result.json").write_text(json.dumps(dict(
            status="pass", entries=names, installer_source_count=len(names),
            inventory_equal=True, **assets)))
        return base, ev, release, stage, dist, names

    def run_evidence(self, *args):
        command = [sys.executable, str(ROOT / "scripts/windows-native-evidence.py"), *map(str, args)]
        return subprocess.run(command, capture_output=True, text=True)

    def add_native(self, ev, release, stage, dist, name):
        """Add one reviewed native DLL to every producer input and stage."""
        data = (name + " binary").encode()
        (stage / name).write_bytes(data)
        (dist / "Iris" / name).write_bytes(data)
        binding = dict(name=name, sha256=module.Audit.sha256_file(stage / name),
                       bytes=len(data))
        inventory = json.loads((ev / "product-inventory.json").read_text())
        inventory["files"].append(binding)
        (ev / "product-inventory.json").write_text(json.dumps(inventory))
        (release / "product-inventory.json").write_text(json.dumps(inventory))
        closure = json.loads((ev / "pe-import-closure.json").read_text())
        closure["binaries"].append(name)
        (ev / "pe-import-closure.json").write_text(json.dumps(closure))
        result = json.loads((release / "release-package-result.json").read_text())
        result["entries"].append(name)
        result["installer_source_count"] = len(result["entries"])
        (release / "release-package-result.json").write_text(json.dumps(result))

    def sealed(self, base, ev, release, stage, dist, output=None):
        prepared = self.run_evidence("prepare", "--evidence", ev, "--release", release,
                                     "--stage", stage, "--output", ev / "windows-native-evidence.json")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)
        output = output or base / "windows-native-closure.json"
        sealed = self.run_evidence("seal", "--manifest", ev / "windows-native-evidence.json",
                                   "--evidence", ev, "--assets", release, "--dist", dist,
                                   "--product", dist / "Iris", "--output", output)
        return sealed, output

    def test_sealed_evidence_becomes_an_audit_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            base, ev, release, stage, dist, names = self.fixture(directory)
            sealed, output = self.sealed(base, ev, release, stage, dist)
            self.assertEqual(sealed.returncode, 0, sealed.stderr)
            manifest = json.loads(output.read_text())
            self.assertEqual(set(manifest), {"schema", "artifacts", "components"})
            self.assertEqual(manifest["schema"], 1)
            self.assertEqual(len(manifest["artifacts"]), len(names) + 4)
            # The audit binds each reference to the exact staged bytes again.
            report = audit(max_native_components=64)
            for path in sorted(dist.rglob("*")):
                if path.is_file():
                    report.add("staged-file", path=str(path), bytes=path.stat().st_size,
                               sha256=module.Audit.sha256_file(path))
            native = dist / "Iris" / "Iris.exe"
            entry = dict(path=str(native), bytes=native.stat().st_size,
                         sha256=module.Audit.sha256_file(native))
            report.add("native-file", **entry)
            report.add("file", **entry)
            report.native_evidence(dist, output)
            self.assertFalse(any(row["kind"] == "missing" for row in report.records),
                             report.records)
            closure = next(row["evidence"] for row in report.records
                           if row["kind"] == "native-closure")
            components = module.validate_closure(closure, report.records, 256, 64)
            self.assertTrue(components)
            self.assertTrue(all(c["license"]["id"] in ("LGPL-2.1-or-later", "BSD-3-Clause")
                                for c in components))

    def test_missing_or_tampered_evidence_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            base, ev, release, stage, dist, names = self.fixture(directory)
            sealed, output = self.sealed(base, ev, release, stage, dist)
            self.assertEqual(sealed.returncode, 0, sealed.stderr)
            for path in (ev / "pe-import-closure.json", ev / "validation-inputs.json",
                         release / "Iris-windows.zip", dist / "Iris" / "pdfium.dll",
                         dist / "Iris-windows.zip.sha256"):
                original = path.read_bytes()
                path.write_bytes(original + b"tampered")
                failed = self.run_evidence("seal", "--manifest",
                                           ev / "windows-native-evidence.json",
                                           "--evidence", ev, "--assets", release,
                                           "--dist", dist, "--product", dist / "Iris",
                                           "--output", base / "rejected.json")
                self.assertNotEqual(failed.returncode, 0, path.name)
                self.assertIn("refused", failed.stderr)
                self.assertFalse((base / "rejected.json").exists())
                path.write_bytes(original)
            (dist / "Iris" / "unlisted.dll").write_bytes(b"unlisted")
            failed = self.run_evidence("seal", "--manifest", ev / "windows-native-evidence.json",
                                       "--evidence", ev, "--assets", release, "--dist", dist,
                                       "--product", dist / "Iris", "--output", base / "rejected.json")
            self.assertNotEqual(failed.returncode, 0)
            self.assertFalse((base / "rejected.json").exists())
            failed = self.run_evidence("seal", "--manifest", base / "absent.json",
                                       "--evidence", ev, "--assets", release, "--dist", dist,
                                       "--product", dist / "Iris", "--output", base / "rejected.json")
            self.assertNotEqual(failed.returncode, 0)
            self.assertFalse((base / "rejected.json").exists())

    def test_unknown_native_dll_in_reviewed_inventory_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            base, ev, release, stage, dist, names = self.fixture(directory)
            sealed, output = self.sealed(base, ev, release, stage, dist)
            self.assertEqual(sealed.returncode, 0, sealed.stderr)
            name = "evil.dll"
            self.add_native(ev, release, stage, dist, name)
            prepared = self.run_evidence("prepare", "--evidence", ev, "--release", release,
                                         "--stage", stage, "--output", ev / "rejected.json")
            self.assertNotEqual(prepared.returncode, 0)
            self.assertIn(name, prepared.stderr)
            self.assertFalse((ev / "rejected.json").exists())
            # The earlier sealed manifest cannot admit a shipped file it never reviewed.
            sealed = self.run_evidence("seal", "--manifest", ev / "windows-native-evidence.json",
                                       "--evidence", ev, "--assets", release, "--dist", dist,
                                       "--product", dist / "Iris", "--output", base / "rejected.json")
            self.assertNotEqual(sealed.returncode, 0)
            self.assertFalse((base / "rejected.json").exists())

    def test_unrecognized_ffmpeg_like_dlls_are_refused_by_prepare(self):
        # A filename starting like an FFmpeg library is not a license claim:
        # probe selection, policy and evidence must share one recognizer.
        for name in ("avcodec-extra.dll", "postproc-57.dll"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                base, ev, release, stage, dist, names = self.fixture(directory)
                self.assertIsNone(module.ffmpeg_component(name))
                self.add_native(ev, release, stage, dist, name)
                output = ev / "rejected.json"
                prepared = self.run_evidence("prepare", "--evidence", ev,
                                             "--release", release, "--stage", stage,
                                             "--output", output)
                self.assertNotEqual(prepared.returncode, 0)
                self.assertIn(name, prepared.stderr)
                self.assertFalse(output.exists())

    def test_shared_recognizer_assigns_the_ffmpeg_component(self):
        with tempfile.TemporaryDirectory() as directory:
            base, ev, release, stage, dist, names = self.fixture(directory)
            added = "libavcodec-62.dll"
            self.add_native(ev, release, stage, dist, added)
            prepared = self.run_evidence("prepare", "--evidence", ev,
                                         "--release", release, "--stage", stage,
                                         "--output", ev / "windows-native-evidence.json")
            self.assertEqual(prepared.returncode, 0, prepared.stderr)
            manifest = json.loads((ev / "windows-native-evidence.json").read_text())
            components = {row["artifact"]["path"]: row for row in manifest["components"]}
            self.assertIn("Iris/" + added, components)
            component = module.ffmpeg_component(added)
            self.assertEqual(component, "avcodec")
            for name in ("avcodec-62.dll", added):
                row = components["Iris/" + name]
                self.assertEqual(row["name"], "FFmpeg " + component)
                self.assertEqual(row["license"]["id"], "LGPL-2.1-or-later")
                self.assertEqual(row["license"]["disposition"], "review")
            ids = [row["id"] for row in manifest["components"]]
            self.assertEqual(len(ids), len(set(ids)))
            sealed, output = self.sealed(base, ev, release, stage, dist)
            self.assertEqual(sealed.returncode, 0, sealed.stderr)

    def test_uncovered_native_artifact_is_refused_at_seal(self):
        with tempfile.TemporaryDirectory() as directory:
            base, ev, release, stage, dist, names = self.fixture(directory)
            sealed, output = self.sealed(base, ev, release, stage, dist)
            self.assertEqual(sealed.returncode, 0, sealed.stderr)
            name = "Iris/zlib1.dll"
            data = b"unlicensed shipped dll"
            (dist / name).write_bytes(data)
            manifest = json.loads((ev / "windows-native-evidence.json").read_text())
            manifest["artifacts"].append(dict(
                file=dict(path=name, sha256=module.Audit.sha256_file(dist / name)),
                coverage=manifest["artifacts"][0]["coverage"]))
            manifest["expected_bytes"][name] = len(data)
            (ev / "crafted.json").write_text(json.dumps(manifest))
            sealed = self.run_evidence("seal", "--manifest", ev / "crafted.json",
                                       "--evidence", ev, "--assets", release, "--dist", dist,
                                       "--product", dist / "Iris", "--output", base / "rejected.json")
            self.assertNotEqual(sealed.returncode, 0)
            self.assertIn(name, sealed.stderr)
            self.assertFalse((base / "rejected.json").exists())

    def test_audit_fails_closed_when_closure_is_required(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "audit.json"
            command = [sys.executable, str(ROOT / "scripts/release-audit.py"), "windows",
                       "--output", str(output),
                       "--native-evidence", str(Path(directory) / "absent.json")]
            missing = subprocess.run(command + ["--require-native-evidence"],
                                     cwd=directory, capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("native evidence required but absent or invalid", missing.stderr)
            self.assertFalse(output.exists())
            # The default collector stays report-only for the other platforms.
            allowed = subprocess.run(command, cwd=directory, capture_output=True, text=True)
            self.assertEqual(allowed.returncode, 0, allowed.stderr)
            report = json.loads(output.read_text())
            self.assertTrue(any(row["kind"] == "missing" for row in report["records"]))
            self.assertFalse(any(row["kind"] == "native-closure" for row in report["records"]))



class InstalledWindowsPayload(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        script = ROOT / "scripts/windows-installed-smoke.py"
        spec = importlib.util.spec_from_file_location("windows_installed_smoke", script)
        cls.smoke = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.smoke)

    def stage(self, root, *, owned=(), extra=()):
        install = Path(root, "install")
        install.mkdir()
        verified = Path(root, "verified")
        verified.mkdir()
        product = {
            "Iris.exe": b"iris executable",
            "pdfium.dll": b"pdfium library",
            "FFMPEG-NOTICE.md": b"runtime notice",
        }
        for name, contents in product.items():
            (install / name).write_bytes(contents)
        inventory = {
            "roots": [],
            "files": [{"name": name, "sha256": hashlib.sha256(contents).hexdigest(),
                       "bytes": len(contents)} for name, contents in product.items()],
        }
        inventory_path = verified / "product-inventory.json"
        inventory_path.write_text(json.dumps(inventory), encoding="utf-8")
        setup = Path(root, "Iris-windows-setup.exe")
        setup.write_bytes(b"exact setup bytes")
        result_path = verified / "release-package-result.json"
        result_path.write_text(json.dumps({
            "status": "pass",
            "entries": list(product),
            "installer_sha256": hashlib.sha256(setup.read_bytes()).hexdigest(),
            "installer_bytes": setup.stat().st_size,
        }), encoding="utf-8")
        for name in owned:
            (install / name).write_bytes(b"installer generated")
        for name in extra:
            (install / name).write_bytes(b"unexpected payload")
        return install, inventory_path, result_path, setup

    def check(self, paths):
        return self.smoke.verify_install(*paths)

    def test_matching_installed_payload_passes_with_bounded_installer_owned_files(self):
        with tempfile.TemporaryDirectory() as root:
            paths = self.stage(root, owned=("unins000.exe", "unins000.dat", "unins000.msg"))
            record = self.check(paths)
            self.assertEqual(record["status"], "pass")
            self.assertEqual(record["product_entries"], 3)
            self.assertEqual(record["matched_entries"], 3)
            self.assertEqual(record["installer_sha256"], hashlib.sha256(b"exact setup bytes").hexdigest())
            self.assertEqual(record["inventory_sha256"],
                             hashlib.sha256(paths[1].read_bytes()).hexdigest())
            self.assertEqual([row["name"] for row in record["installer_owned"]],
                             ["unins000.dat", "unins000.exe", "unins000.msg"])
            for row in record["installer_owned"]:
                self.assertEqual(row["sha256"], hashlib.sha256(b"installer generated").hexdigest())

    def test_missing_or_modified_inventory_fails_closed(self):
        for mode in ("missing", "malformed", "hash", "bytes", "entry_removed", "installed_file_removed"):
            with tempfile.TemporaryDirectory() as root, self.subTest(mode=mode):
                paths = self.stage(root)
                install, inventory_path, _, _ = paths
                if mode == "missing":
                    inventory_path.unlink()
                elif mode == "malformed":
                    inventory_path.write_text("{not json", encoding="utf-8")
                elif mode == "installed_file_removed":
                    (install / "pdfium.dll").unlink()
                else:
                    inventory = json.loads(inventory_path.read_text())
                    if mode == "hash":
                        inventory["files"][0]["sha256"] = "0" * 64
                    elif mode == "bytes":
                        inventory["files"][0]["bytes"] += 1
                    elif mode == "entry_removed":
                        inventory["files"].pop()
                    inventory_path.write_text(json.dumps(inventory), encoding="utf-8")
                with self.assertRaises(ValueError):
                    self.check(paths)

    def test_unexpected_file_fails_unless_explicitly_installer_owned(self):
        with tempfile.TemporaryDirectory() as root:
            self.assertEqual(self.check(self.stage(root, owned=("unins000.exe",)))["status"], "pass")
        for name in ("surprise.dll", "preview_corpus_smoke.exe", "rustc.exe"):
            with tempfile.TemporaryDirectory() as root, self.subTest(name=name):
                with self.assertRaises(ValueError):
                    self.check(self.stage(root, extra=(name,)))
        with tempfile.TemporaryDirectory() as root:
            paths = self.stage(root, owned=("unins000.exe",))
            with patch.object(self.smoke, "MAX_INSTALLER_OWNED_BYTES", 2):
                with self.assertRaises(ValueError):
                    self.check(paths)
        with tempfile.TemporaryDirectory() as root:
            paths = self.stage(root)
            (paths[0] / "nested").mkdir()
            with self.assertRaises(ValueError):
                self.check(paths)

    def test_cli_writes_bound_evidence_and_fails_closed(self):
        with tempfile.TemporaryDirectory() as root:
            paths = self.stage(root, owned=("unins000.exe",))
            output = Path(root, "audit/windows-installer-smoke.json")
            def invoke():
                args = ["verify", "--install-dir", str(paths[0]), "--inventory", str(paths[1]),
                        "--result", str(paths[2]), "--setup", str(paths[3]), "--output", str(output)]
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    return self.smoke.main(args)
            self.assertEqual(invoke(), 0)
            record = json.loads(output.read_text())
            self.assertEqual(record["status"], "pass")
            self.assertEqual(record["installer_sha256"], hashlib.sha256(b"exact setup bytes").hexdigest())
            paths[3].write_bytes(b"tampered setup bytes")
            self.assertEqual(invoke(), 1)
            self.assertEqual(json.loads(output.read_text())["status"], "fail")

    def test_installer_hash_binding_fails_closed(self):
        for mode in ("setup", "result_hash", "invalid_hash", "result_bytes", "status", "entries", "renamed_setup"):
            with tempfile.TemporaryDirectory() as root, self.subTest(mode=mode):
                paths = self.stage(root)
                install, inventory_path, result_path, setup = paths
                result = json.loads(result_path.read_text())
                if mode == "setup":
                    setup.write_bytes(b"tampered setup bytes")
                elif mode == "renamed_setup":
                    setup = Path(root, "other-setup.exe")
                    setup.write_bytes(b"exact setup bytes")
                else:
                    if mode == "result_hash":
                        result["installer_sha256"] = "1" * 64
                    elif mode == "invalid_hash":
                        result["installer_sha256"] = "not-a-sha256"
                    elif mode == "result_bytes":
                        result["installer_bytes"] += 1
                    elif mode == "status":
                        result["status"] = "fail"
                    elif mode == "entries":
                        result["entries"].append("surprise.dll")
                    result_path.write_text(json.dumps(result), encoding="utf-8")
                with self.assertRaises(ValueError):
                    self.check((install, inventory_path, result_path, setup))

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
        self.assertIn("  workflow_dispatch:\n  push:", self.source)
        self.assertNotIn("source_ref", self.source)
        self.assertNotIn("inputs.", self.source)
        self.assertIn("ref: ${{ github.sha }}", self.jobs["release-preflight"])
        self.assertIn('echo "sha=$(git rev-parse HEAD)"', self.jobs["release-preflight"])
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

    def test_windows_installer_smoke_binds_inventory_and_exact_setup(self):
        job = self.jobs["package-windows"]
        smoke = job.split("      - name: Smoke-test Windows installer\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("$exported.installer_sha256 -notmatch", smoke)
        self.assertIn("Get-FileHash -Algorithm SHA256 -LiteralPath $setup", smoke)
        self.assertIn("python scripts/windows-installed-smoke.py verify", smoke)
        for flag in ("--install-dir", "--inventory", "--result", "--setup", "--output"):
            self.assertIn(flag, smoke)
        self.assertIn("verified/windows/product-inventory.json", smoke)
        self.assertIn("audit/windows-installer-smoke.json", smoke)
        self.assertIn("if ($LASTEXITCODE -ne 0) { throw", smoke)
        self.assertIn("finally", smoke)
        self.assertIn("unins000.exe", smoke)
        # The ZIP, required notice and checksum gates stay in the same job.
        self.assertIn("ZIP entries differ from the verified product inventory", job)
        self.assertIn('@("Iris.exe", "pdfium.dll", "FFMPEG-NOTICE.md")', job)
        self.assertIn("checksum verification failed", job)

    def test_failed_pdfium_verification_cannot_be_written_as_verified(self):
        fetch = (ROOT / "scripts/fetch-pdfium-runtime.sh").read_text()
        self.assertIn("set -euo pipefail", fetch)
        self.assertIn("[[ -n ${GH_TOKEN:-} ]]", fetch)
        verify = fetch.index('"$gh_executable" attestation verify "$tarball" --repo "$pdfium_repo" --bundle "$bundle"')
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
