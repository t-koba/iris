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
                    max_native_files=256, command_seconds=5)
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
        self.assertIn("vars.IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES", self.source)

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
