#!/usr/bin/env python3
"""Offline unit contracts for the opt-in Windows PDFium 8179 candidate gate."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "windows_pdfium_candidate", ROOT / "scripts/windows-pdfium-candidate.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

DEFAULT_FILES = {
    "bin/pdfium.dll": b"candidate-pdfium-bytes",
    "LICENSE": b"BSD-3-Clause\n",
    "VERSION": b"MAJOR=160\nBUILD=8179\nMINOR=0\nPATCH=0\n",
    "licenses/pdfium.txt": b"PDFium license\n",
}


def make_sysroot(root, *, dlls=()):
    """Build a fake, platform-independent rustc sysroot toolchain."""
    root = Path(root)
    (root / "bin").mkdir(parents=True)
    (root / "bin" / "rustc.exe").write_bytes(b"real-rustc")
    for name, data in dlls:
        (root / "bin" / name).write_bytes(data)
    return root


def make_archive(files=None, links=()):
    """Build a small in-memory .tgz, including deliberately unsafe members."""
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w:gz") as tar:
        for name, data in (DEFAULT_FILES if files is None else files).items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        for name, target in links:
            info = tarfile.TarInfo(name)
            info.type = tarfile.SYMTYPE
            info.linkname = target
            tar.addfile(info)
    return payload.getvalue()


class PinContracts(unittest.TestCase):
    def test_exact_immutable_pins(self):
        self.assertEqual(mod.RELEASE, "chromium/8179")
        self.assertEqual(mod.VERSION, "160.0.8179.0")
        self.assertEqual(mod.SOURCE_COMMIT,
                         "02feff9f75486f5caac18147db45087b6ffbbec6")
        self.assertEqual(mod.ASSET, "pdfium-win-x64.tgz")
        self.assertEqual(mod.ASSET_BYTES, 4_046_589)
        self.assertEqual(mod.ASSET_SHA256,
                         "836a77c9053f624839c66010cddea7b675628a36b8831d"
                         "2330178bb169d241b6")
        self.assertEqual(mod.ATTESTATION_ASSET, "pdfium-attestation.json")
        self.assertEqual(mod.ATTESTATION_BYTES, 18_083)
        self.assertEqual(mod.ATTESTATION_SHA256,
                         "f9d4bb0be84c30c98b98b2550ed2f81efc4024cd172511a"
                         "5e786163c9ae8deb6")
        self.assertEqual(mod.REPO, "bblanchon/pdfium-binaries")
        self.assertEqual(mod.SIGNER_WORKFLOW,
                         "github.com/bblanchon/pdfium-binaries/.github/"
                         "workflows/build-all.yml")
        self.assertEqual(mod.SOURCE_REF, "refs/heads/master")
        self.assertEqual((mod.MAJOR, mod.BUILD), (160, 8179))
        self.assertEqual(mod.release_urls(), (
            "https://github.com/bblanchon/pdfium-binaries/releases/download/"
            "chromium/8179/pdfium-win-x64.tgz",
            "https://github.com/bblanchon/pdfium-binaries/releases/download/"
            "chromium/8179/pdfium-attestation.json",
        ))

    def test_shipped_windows_manifest_pin_remains_8138(self):
        manifest = json.loads((ROOT / "packaging/windows/ffmpeg.json").read_text())
        pdfium = manifest["pdfium"]
        self.assertEqual(pdfium["release"], "chromium/8138")
        self.assertEqual(pdfium["version"], "159.0.8138.0")
        self.assertEqual(pdfium["sha256"],
                         "8d48973e3eb472dda0f5f2435e595ac043f9f2219dd718833"
                         "92f549445afa8a5")
        self.assertEqual(mod.SHIPPED_RELEASE, pdfium["release"])
        self.assertNotEqual(mod.RELEASE, pdfium["release"])

    def test_exact_size_and_sha_rejection(self):
        data = b"candidate bytes\n"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "asset"
            path.write_bytes(data)
            pin = mod.file_pin(path, "asset", len(data),
                               hashlib.sha256(data).hexdigest())
            self.assertEqual(pin, {"name": "asset", "bytes": len(data),
                                   "sha256": hashlib.sha256(data).hexdigest()})
            with self.assertRaisesRegex(mod.CandidateError, "size mismatch"):
                mod.file_pin(path, "asset", len(data) + 1, pin["sha256"])
            with self.assertRaisesRegex(mod.CandidateError, "sha256 mismatch"):
                mod.file_pin(path, "asset", len(data), "0" * 64)
            path.write_bytes(data + b"x")
            with self.assertRaisesRegex(mod.CandidateError, "size mismatch"):
                mod.file_pin(path, "asset", len(data), "0" * 64)

    def test_download_requires_https_and_retries(self):
        url = mod.release_urls()[0]
        self.assertEqual(mod.require_https(url), url)
        for bad in (url.replace("https://", "http://"),
                    url.replace("github.com", "example.com"),
                    url.replace("/bblanchon/", "/someone-else/")):
            with self.subTest(url=bad), self.assertRaises(mod.CandidateError):
                mod.require_https(bad)
        attempts = []

        def flaky(target, timeout):
            attempts.append((target, timeout))
            if len(attempts) < 3:
                raise OSError("transient")
            return io.BytesIO(b"downloaded")

        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "asset.part"
            mod.download(url, destination, retries=2, delay=0, opener=flaky)
            self.assertEqual(destination.read_bytes(), b"downloaded")
            self.assertEqual(len(attempts), 3)
            self.assertTrue(all(item == (url, mod.DOWNLOAD_TIMEOUT_SECONDS)
                                for item in attempts))
            with self.assertRaisesRegex(mod.CandidateError, "after 3 attempts"):
                mod.download(url, destination, retries=2, delay=0,
                             opener=lambda target, timeout: (_ for _ in ()).throw(
                                 OSError("offline")))
            self.assertFalse(destination.exists())


class ArchiveContracts(unittest.TestCase):
    def test_unsafe_archive_members_rejected(self):
        unsafe = (
            {"../pdfium.dll": b"escape"},
            {"/pdfium.dll": b"absolute"},
            {"licenses/../../pdfium.dll": b"traversal"},
            {"C:/pdfium.dll": b"drive"},
            {"bin\\pdfium.dll": b"backslash"},
        )
        for files in unsafe:
            with self.subTest(files=files):
                with tempfile.TemporaryDirectory() as temporary:
                    path = Path(temporary) / "asset.tgz"
                    path.write_bytes(make_archive(dict(DEFAULT_FILES, **files)))
                    with self.assertRaises(mod.CandidateError):
                        mod.archive_layout(path)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "asset.tgz"
            path.write_bytes(make_archive(links=(("bin/pdfium.dll", "../../evil"),)))
            with self.assertRaises(mod.CandidateError):
                mod.archive_layout(path)

    def test_missing_required_members_rejected(self):
        for name in ("bin/pdfium.dll", "LICENSE", "VERSION", "licenses/pdfium.txt"):
            with self.subTest(missing=name):
                files = dict(DEFAULT_FILES)
                files.pop(name)
                with tempfile.TemporaryDirectory() as temporary:
                    path = Path(temporary) / "asset.tgz"
                    path.write_bytes(make_archive(files))
                    with self.assertRaises(mod.CandidateError):
                        mod.archive_layout(path)

    def test_valid_archive_extracts_candidate_and_checks_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "asset.tgz"
            archive.write_bytes(make_archive())
            layout = mod.archive_layout(archive)
            self.assertEqual(layout["dll"], "bin/pdfium.dll")
            extracted = mod.extract_required(archive, layout, root / "extract")
            self.assertEqual((extracted / "pdfium.dll").read_bytes(),
                             DEFAULT_FILES["bin/pdfium.dll"])
            self.assertEqual((extracted / "LICENSE").read_bytes(),
                             DEFAULT_FILES["LICENSE"])
            self.assertTrue((extracted / "licenses/pdfium.txt").is_file())
            self.assertEqual(mod.check_version(extracted / "VERSION")["build"], 8179)
            (extracted / "VERSION").write_bytes(b"MAJOR=159\nBUILD=8138\n")
            with self.assertRaisesRegex(mod.CandidateError, "VERSION"):
                mod.check_version(extracted / "VERSION")


class AttestationContracts(unittest.TestCase):
    def test_gh_command_has_every_identity_flag_and_no_token(self):
        command = mod.gh_command("/tmp/asset.tgz", "/tmp/bundle.json")
        self.assertEqual(command, [
            "gh", "attestation", "verify", "/tmp/asset.tgz",
            "--repo", "bblanchon/pdfium-binaries",
            "--bundle", "/tmp/bundle.json",
            "--signer-workflow",
            "github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml",
            "--signer-digest", "02feff9f75486f5caac18147db45087b6ffbbec6",
            "--source-digest", "02feff9f75486f5caac18147db45087b6ffbbec6",
            "--source-ref", "refs/heads/master",
        ])
        seen = {}

        def runner(args, **kwargs):
            seen["args"] = args
            seen["env"] = kwargs["env"]
            return subprocess.CompletedProcess(args, 0, "", "")

        consistent_env = {"GH_TOKEN": "sentinel-token",
                          "gh_token": "sentinel-token"}
        result = mod.verify_attestation("asset.tgz", "bundle.json", runner=runner,
                                        environ=consistent_env)
        self.assertEqual(result["status"], "verified")
        conflicting_env = {"GH_TOKEN": "sentinel-token",
                           "gh_token": "stale-token"}
        with self.assertRaisesRegex(mod.CandidateError, "consistent"):
            mod.verify_attestation("asset.tgz", "bundle.json", runner=runner,
                                   environ=conflicting_env)
        self.assertEqual(seen["args"], mod.gh_command("asset.tgz", "bundle.json"))
        self.assertEqual(seen["env"]["GH_TOKEN"], "sentinel-token")
        self.assertTrue(all("sentinel-token" not in part for part in seen["args"]))
        with self.assertRaisesRegex(mod.CandidateError, "GH_TOKEN"):
            mod.verify_attestation("asset.tgz", "bundle.json", runner=runner,
                                   environ={})
        failing = lambda args, **kwargs: subprocess.CompletedProcess(args, 1, "", "")
        with self.assertRaisesRegex(mod.CandidateError, "exit code 1"):
            mod.verify_attestation("asset.tgz", "bundle.json", runner=failing,
                                   environ={"GH_TOKEN": "sentinel-token"})


class PreviewContracts(unittest.TestCase):
    def test_strict_correctness_record_parsing(self):
        good = json.dumps({"record": "iris.preview_correctness.v1", "passed": True})
        latency = json.dumps({"record": "iris.preview_latency.v1", "passed": True})
        valid = ("noise\nPREVIEW_CORRECTNESS " + good + "\n"
                 + ("PREVIEW_LATENCY " + latency + "\n") * 5)
        self.assertEqual(mod.parse_preview_records(valid), {
            "status": "pass", "passed": True, "correctness_records": 1,
            "latency_records": 5})
        for output in (
            "",
            f"PREVIEW_CORRECTNESS {good}\nPREVIEW_CORRECTNESS {good}\n",
            'PREVIEW_CORRECTNESS {"passed": false}\n',
            'PREVIEW_CORRECTNESS {"record": "x"}\n',
            "PREVIEW_CORRECTNESS not-json\n",
            "PREVIEW_LATENCY not-json\n" + f"PREVIEW_CORRECTNESS {good}\n",
            f"PREVIEW_CORRECTNESS {good}\n",
            f"PREVIEW_CORRECTNESS {good}\n"
            + (f"PREVIEW_LATENCY {latency}\n" * 4),
        ):
            with self.subTest(output=output), self.assertRaises(mod.CandidateError):
                mod.parse_preview_records(output)

    def test_candidate_stage_never_contains_production_pdfium(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            smoke = root / "preview_corpus_smoke.exe"
            smoke.write_bytes(b"smoke")
            candidate = root / "candidate.dll"
            candidate.write_bytes(b"candidate")
            dll_dir = root / "vcpkg-bin"
            dll_dir.mkdir()
            (dll_dir / "avcodec-61.dll").write_bytes(b"a")
            (dll_dir / "avformat-61.dll").write_bytes(b"b")
            sysroot = make_sysroot(root / "sysroot",
                                   dlls=(("rustc_driver.dll", b"driver"),))
            stage = mod.stage_candidate(root / "work", smoke, dll_dir, candidate,
                                        sysroot=sysroot)
            self.assertEqual(sorted(p.name for p in stage.iterdir()), [
                "avcodec-61.dll", "avformat-61.dll", "pdfium.dll",
                "preview_corpus_smoke.exe", "rustc.exe", "rustc_driver.dll"])
            self.assertEqual((stage / "pdfium.dll").read_bytes(), b"candidate")
            (dll_dir / "pdfium.dll").write_bytes(b"production")
            with self.assertRaisesRegex(mod.CandidateError, "production PDFium"):
                mod.stage_candidate(root / "work2", smoke, dll_dir, candidate,
                                    sysroot=sysroot)

    def test_rustc_sysroot_probe_and_toolchain_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sysroot = make_sysroot(root / "sysroot", dlls=(
                ("rustc_driver.dll", b"driver"), ("rustc_std.dll", b"std")))
            seen = {}

            def probe(args, **kwargs):
                seen["args"] = args
                seen["kwargs"] = kwargs
                return subprocess.CompletedProcess(args, 0, f"{sysroot}\n", "")

            self.assertEqual(mod.rustc_sysroot(runner=probe), sysroot)
            self.assertEqual(seen["args"], ["rustc", "--print", "sysroot"])
            self.assertTrue(all("GH_TOKEN" not in part for part in seen["args"]))
            smoke = root / "preview_corpus_smoke.exe"
            smoke.write_bytes(b"smoke")
            candidate = root / "candidate.dll"
            candidate.write_bytes(b"candidate")
            dll_dir = root / "vcpkg-bin"
            dll_dir.mkdir()
            (dll_dir / "avcodec-61.dll").write_bytes(b"a")
            stage = mod.stage_candidate(root / "work", smoke, dll_dir, candidate,
                                        runner=probe)
            self.assertEqual((stage / "rustc.exe").read_bytes(), b"real-rustc")
            self.assertEqual((stage / "rustc_driver.dll").read_bytes(), b"driver")
            self.assertEqual((stage / "rustc_std.dll").read_bytes(), b"std")
            self.assertEqual(sorted(p.name for p in stage.iterdir()
                                    if p.suffix.lower() == ".dll"),
                             ["avcodec-61.dll", "pdfium.dll", "rustc_driver.dll",
                              "rustc_std.dll"])
            failing = lambda args, **kwargs: subprocess.CompletedProcess(args, 4, "", "")
            with self.assertRaisesRegex(mod.CandidateError, "probe failed"):
                mod.rustc_sysroot(runner=failing)
            empty = lambda args, **kwargs: subprocess.CompletedProcess(args, 0, "  ", "")
            with self.assertRaisesRegex(mod.CandidateError, "empty or unbounded"):
                mod.rustc_sysroot(runner=empty)
            (sysroot / "bin" / "rustc.exe").unlink()
            with self.assertRaisesRegex(mod.CandidateError, "rustc executable missing"):
                mod.stage_candidate(root / "work2", smoke, dll_dir, candidate,
                                    sysroot=sysroot)

    def test_rustc_sysroot_dll_bounds_and_duplicate_pdfium_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            smoke = root / "preview_corpus_smoke.exe"
            smoke.write_bytes(b"smoke")
            candidate = root / "candidate.dll"
            candidate.write_bytes(b"candidate")
            dll_dir = root / "vcpkg-bin"
            dll_dir.mkdir()
            (dll_dir / "avcodec-61.dll").write_bytes(b"a")
            for index, (dlls, message) in enumerate((
                (tuple((f"rustc{i}.dll", b"x")
                       for i in range(mod.MAX_RUSTC_DLLS + 1)), "exceeds bounds"),
                ((("pdfium.dll", b"production"),), "production PDFium"),
                ((("avcodec-61.dll", b"collision"),), "duplicate DLL names"),
            )):
                sysroot = make_sysroot(root / f"sysroot-{index}", dlls=dlls)
                with self.subTest(message=message), self.assertRaisesRegex(
                        mod.CandidateError, message):
                    mod.stage_candidate(root / f"work-{index}", smoke,
                                        dll_dir, candidate, sysroot=sysroot)

    def test_smoke_runs_with_only_stage_and_system_path(self):
        good = json.dumps({"record": "iris.preview_correctness.v1", "passed": True})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            system_root = root / "Windows"
            (system_root / "System32" / "WindowsPowerShell" / "v1.0").mkdir(parents=True)
            stage = root / "stage"
            stage.mkdir()
            corpus = root / "corpus"
            corpus.mkdir()
            (stage / "preview_corpus_smoke.exe").write_bytes(b"smoke")
            seen = {}

            def runner(args, **kwargs):
                seen["args"] = args
                seen["cwd"] = kwargs["cwd"]
                seen["env"] = kwargs["env"]
                seen["path"] = kwargs["env"]["PATH"].split(os.pathsep)
                latency = json.dumps({"record": "iris.preview_latency.v1"})
                output = (f"PREVIEW_CORRECTNESS {good}\n"
                          + (f"PREVIEW_LATENCY {latency}\n" * 5))
                return subprocess.CompletedProcess(args, 0, output, "")

            with patch.dict(os.environ, {"SystemRoot": str(system_root),
                                         "Path": "/bogus/windows-path"}, clear=False):
                dirs = mod.windows_system_path()
                preview = mod.run_smoke(stage, corpus, dirs, runner=runner)
            resolved_stage = stage.resolve()
            resolved_corpus = corpus.resolve()
            self.assertEqual(preview["status"], "pass")
            self.assertEqual(seen["args"], [str(resolved_stage / "preview_corpus_smoke.exe"),
                                            str(resolved_corpus)])
            self.assertEqual(seen["cwd"], str(resolved_stage))
            self.assertEqual(seen["path"], [str(resolved_stage)] + [str(p) for p in dirs])
            self.assertNotIn("Path", seen["env"])
            self.assertTrue(all(Path(part) in dirs for part in seen["path"][1:]))
            failing = lambda args, **kwargs: subprocess.CompletedProcess(args, 9, "", "")
            with self.assertRaisesRegex(mod.CandidateError, "exit code 9"):
                mod.run_smoke(stage, corpus, dirs, runner=failing)

    def test_bounded_evidence_records_gate_result(self):
        asset = {"name": mod.ASSET, "bytes": mod.ASSET_BYTES,
                 "sha256": mod.ASSET_SHA256}
        bundle = {"name": mod.ATTESTATION_ASSET, "bytes": mod.ATTESTATION_BYTES,
                  "sha256": mod.ATTESTATION_SHA256}
        attestation = {"status": "verified", "repo": mod.REPO}
        preview = {"status": "pass", "passed": True, "correctness_records": 1}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "evidence/pdfium-8179-candidate.json"
            mod.write_evidence(path, asset, bundle, attestation,
                               {"major": 160, "build": 8179, "version": mod.VERSION},
                               preview)
            report = json.loads(path.read_text())
            self.assertEqual(report["asset"], asset)
            self.assertEqual(report["attestation"]["sha256"], mod.ATTESTATION_SHA256)
            self.assertEqual(report["attestation"]["status"], "verified")
            self.assertEqual(report["preview"], preview)
            self.assertEqual(report["shipped_pin"],
                             {"release": "chromium/8138", "changed": False})
            self.assertIn("no exploitability", report["scope"])
            self.assertLessEqual(path.stat().st_size, mod.MAX_EVIDENCE_BYTES)


if __name__ == "__main__":
    unittest.main(verbosity=2)
