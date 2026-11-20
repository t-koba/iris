#!/usr/bin/env python3
"""Offline contracts for approved production Windows FFmpeg source acquisition.

No download, GPG key import, native build or package install. A generated xz
archive with a locally computed hash stands in for the upstream tarball; the
signature status uses the approved fingerprint. Verification logic is shared
with linux-ffmpeg.py, so these tests exercise the real verifier.
"""

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("win_source", ROOT / "scripts/windows-ffmpeg-source.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

FINGERPRINT = module.common.POLICY["fingerprint"]


def valid_status(subkey=None):
    key = subkey if subkey is not None else FINGERPRINT
    # First field is the signing subkey, last is the primary fingerprint that
    # GnuPG binds it to; the shared verifier requires the primary to match.
    return f"[GNUPG:] VALIDSIG {key} 2026-01-01 1 0 4 0 1 10 00 {FINGERPRINT}\n"


def make_archive(path, files=None):
    files = files if files is not None else {"ffmpeg-8.1.3/configure": b"#!/bin/sh\n", "ffmpeg-8.1.3/RELEASE": b"8.1.3\n"}
    with tarfile.open(path, "w:xz") as tar:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return hashlib.sha256(path.read_bytes()).hexdigest()


class SourcePinContracts(unittest.TestCase):
    def test_source_pin_is_approved_and_stable(self):
        pin = module.source_pin()
        self.assertEqual(pin["version"], "8.1.3")
        self.assertEqual(pin["url"], "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz")
        self.assertEqual(pin["sha256"], "7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3")
        self.assertEqual(pin["avcodec_version"], [62, 28, 103])
        self.assertNotIn("repository", pin)
        self.assertNotIn("commit", pin)

    def test_no_unsigned_prototype_source_in_module_or_plan(self):
        for relative in ("scripts/windows-ffmpeg-source.py", "packaging/windows/ffmpeg.json"):
            text = (ROOT / relative).read_text()
            for marker in ("db8537d8", "5aa9dc31", "9.1.0", "vcpkg_from_git", "git+"):
                self.assertNotIn(marker, text, f"unsigned source marker {marker} in {relative}")

    def test_pdfium_pin_is_approved(self):
        pin = module.pdfium_pin()
        self.assertEqual(pin["asset"], "pdfium-win-x64.tgz")
        self.assertEqual(pin["release"], "chromium/8138")
        self.assertEqual(pin["bytes"], 4031929)
        self.assertEqual(pin["sha256"], "8d48973e3eb472dda0f5f2435e595ac043f9f2219dd71883392f549445afa8a5")
        self.assertTrue(pin["attestation_required"])


class VerificationContracts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.archive = self.dir / "ffmpeg.tar.xz"
        self.sha = make_archive(self.archive)
        self.pin = dict(module.source_pin(), sha256=self.sha)

    def tearDown(self):
        self.tmp.cleanup()

    def test_verify_accepts_pinned_hash_and_signature(self):
        self.assertTrue(module.verify_archive(self.archive, valid_status(), self.pin))
        # A bound signing subkey is accepted, as in the shared verifier.
        self.assertTrue(module.verify_archive(self.archive, valid_status("A" * 40), self.pin))

    def test_verify_rejects_wrong_hash(self):
        bad = dict(self.pin, sha256="0" * 64)
        with self.assertRaises(ValueError):
            module.verify_archive(self.archive, valid_status(), bad)

    def test_verify_rejects_bad_or_wrong_key_signature(self):
        for status in ("", valid_status("B" * 40).replace(FINGERPRINT, "B" * 40),
                       valid_status() + "[GNUPG:] REVKEYSIG bad key\n", valid_status() + valid_status()):
            with self.subTest(status=status[:40]), self.assertRaises(ValueError):
                module.verify_archive(self.archive, status, self.pin)

    def test_verify_defaults_to_approved_pin(self):
        # With the real pin a synthetic archive must fail the hash check,
        # proving the default path is the approved 8.1.3 pin, not the fixture.
        with self.assertRaises(ValueError):
            module.verify_archive(self.archive, valid_status())


class StageContracts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.archive = self.dir / "ffmpeg.tar.xz"
        self.sha = make_archive(self.archive)
        self.pin = dict(module.source_pin(), sha256=self.sha)

    def tearDown(self):
        self.tmp.cleanup()

    def test_stage_writes_verified_manifest_and_extracts(self):
        dest = self.dir / "staged"
        manifest = module.stage(self.archive, valid_status(), dest, self.pin)
        self.assertTrue(manifest["signature_verified"])
        self.assertEqual(manifest["source_sha256"], self.sha)
        self.assertEqual((dest / "ffmpeg-8.1.3/RELEASE").read_bytes(), b"8.1.3\n")
        record = json.loads((dest / "acquisition.json").read_text())
        self.assertEqual(record, manifest)
        self.assertIn("ffmpeg-8.1.3/configure", record["files"])

    def test_stage_fails_closed_before_extraction_on_bad_input(self):
        dest = self.dir / "rejected"
        with self.assertRaises(ValueError):
            module.stage(self.archive, valid_status("C" * 40).replace(FINGERPRINT, "C" * 40), dest, self.pin)
        self.assertFalse(dest.exists(), "no destination may be created before verification passes")

    def test_stage_rejects_traversal(self):
        evil = self.dir / "evil.tar.xz"
        sha = make_archive(evil, {"../escape": b"x"})
        pin = dict(self.pin, sha256=sha)
        with self.assertRaises(ValueError):
            module.stage(evil, valid_status(), self.dir / "evil-out", pin)

    def test_stage_rejects_symlink_and_fifo(self):
        for kind in ("symlink", "fifo"):
            archive = self.dir / f"{kind}.tar.xz"
            with tarfile.open(archive, "w:xz") as tar:
                info = tarfile.TarInfo("ffmpeg-8.1.3/bad")
                info.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.FIFOTYPE
                info.linkname = "/etc/passwd" if kind == "symlink" else ""
                tar.addfile(info)
            sha = hashlib.sha256(archive.read_bytes()).hexdigest()
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                module.stage(archive, valid_status(), self.dir / f"{kind}-out", dict(self.pin, sha256=sha))


class WiringContracts(unittest.TestCase):
    def test_contract_wired_into_ci(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/test-windows-ffmpeg-source.py", ci)


if __name__ == "__main__":
    unittest.main()
